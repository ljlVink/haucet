use super::{WorkerResult, emit_log, summary_payload};
use anyhow::{Context, Result, ensure};
use common::fs_util;
use hm_fastboot::nusb::DeviceInfo;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FastbootCommand {
    #[default]
    Getvar,
    Oem,
    Erase,
    Continue,
}

impl FastbootCommand {
    pub const ALL: [Self; 4] = [Self::Getvar, Self::Oem, Self::Erase, Self::Continue];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Getvar => "getvar",
            Self::Oem => "oem",
            Self::Erase => "erase",
            Self::Continue => "continue",
        }
    }

    pub fn takes_argument(self) -> bool {
        self != Self::Continue
    }

    pub fn argument(self, input: &str) -> Result<&str> {
        if !self.takes_argument() {
            return Ok("");
        }
        let argument = input.trim();
        ensure!(
            !argument.is_empty()
                && !argument.chars().any(char::is_control)
                && (self == Self::Oem || !argument.chars().any(char::is_whitespace)),
            "{}",
            tr!("fastboot-command-invalid-argument")
        );
        Ok(argument)
    }
}

const CHUNK_SIZE: u32 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDevice {
    bus: String,
    address: u8,
    vid: u16,
    pid: u16,
    serial: Option<String>,
}

impl MemoryDevice {
    pub fn from_info(info: &DeviceInfo) -> Self {
        Self {
            bus: info.bus_id().to_owned(),
            address: info.device_address(),
            vid: info.vendor_id(),
            pid: info.product_id(),
            serial: info.serial_number().map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMap {
    pub device: MemoryDevice,
    pub regions: Vec<MemoryRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRegion {
    pub name: String,
    pub base: u64,
    pub size: u32,
}

impl MemoryRegion {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.is_empty() && !self.name.chars().any(char::is_control),
            "{}",
            tr!("fastboot-memory-invalid-name")
        );
        ensure!(
            self.size != 0 && self.base.is_multiple_of(4),
            "{}",
            tr!("fastboot-memory-invalid-range")
        );
        self.base
            .checked_add(u64::from(self.size))
            .context(tr!("fastboot-memory-invalid-range"))?;
        Ok(())
    }

    pub fn chunks(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        (0..self.size).step_by(CHUNK_SIZE as usize).map(|offset| {
            (
                self.base + u64::from(offset),
                (self.size - offset).min(CHUNK_SIZE),
            )
        })
    }

    pub fn suggested_filename(&self) -> String {
        let name: String = self
            .name
            .chars()
            .take(80)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{name}_{:x}.bin", self.base)
    }
}

fn parse_ddrdump(output: &str) -> Result<Vec<MemoryRegion>> {
    let mut regions = Vec::new();
    for (index, line) in output.lines().enumerate() {
        let base_start = field_start(line, "base");
        if base_start.is_none()
            && field_start(line, "size").is_none()
            && field_start(line, "mem").is_none()
        {
            continue;
        }
        let parse = || -> Option<MemoryRegion> {
            let (base, rest) = line[base_start?..].split_once(',')?;
            let mem_start = field_start(rest, "mem")?;
            let size = rest[..mem_start].trim().trim_end_matches(',');
            Some(MemoryRegion {
                name: field_value(&rest[mem_start..], "mem")?.to_owned(),
                base: hex_value(field_value(base, "base")?)?,
                size: u32::try_from(hex_value(field_value(size, "size")?)?).ok()?,
            })
        };
        let error = || tr!("fastboot-memory-parse-line", "line" => index + 1);
        let region = parse().with_context(error)?;
        region.validate().with_context(error)?;
        regions.push(region);
    }
    ensure!(!regions.is_empty(), "{}", tr!("fastboot-memory-empty"));
    Ok(regions)
}

fn field_start(text: &str, name: &str) -> Option<usize> {
    text.match_indices(name).find_map(|(index, _)| {
        let before = text[..index].chars().next_back();
        let boundary = before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
            || text[..index].ends_with("INFO")
            || text[..index].ends_with("TEXT");
        (boundary && text[index + name.len()..].trim_start().starts_with(':')).then_some(index)
    })
}

fn field_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.trim()
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix(':')
        .map(str::trim)
}

fn hex_value(text: &str) -> Option<u64> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))?;
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context(tr!("fastboot-runtime-error"))
}

pub(super) fn status() -> Result<WorkerResult> {
    let runtime = runtime()?;
    runtime.block_on(async {
        use hm_fastboot::nusb::{DeviceSelectionError, NusbFastBoot, require_single_device};
        let devices: Vec<_> = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?
            .collect();
        let list: Vec<_> = devices.iter().map(device_json).collect();
        let info = match require_single_device(devices.into_iter()) {
            Ok(info) => info,
            Err(DeviceSelectionError::NotFound) => {
                return Ok(WorkerResult {
                    ok: true,
                    summary: tr!("fastboot-not-found"),
                    payload: Some(serde_json::json!({
                        "connected": false,
                        "devices": list,
                    })),
                });
            }
            Err(DeviceSelectionError::Multiple) => {
                return Ok(WorkerResult {
                    ok: true,
                    summary: tr!("worker-fastboot-multiple-rejected"),
                    payload: Some(serde_json::json!({
                        "connected": false,
                        "devices": list,
                    })),
                });
            }
        };

        let mut vars = serde_json::Map::new();
        let opened = match NusbFastBoot::from_info(&info).await {
            Ok(mut fb) => {
                for var in ["product", "serialno", "version", "max-download-size"] {
                    match fb.get_var(var).await {
                        Ok(value) => {
                            vars.insert(var.to_owned(), serde_json::Value::String(value));
                        }
                        Err(error) => emit_log(&tr!("fastboot-getvar-error", "variable" => var, "error" => error.to_string())),
                    }
                }
                true
            }
            Err(error) => {
                emit_log(&tr!("open-device-error", "error" => format!("{error:#}")));
                false
            }
        };

        let product = vars
            .get("product")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| tr!("unknown-device"));
        Ok(WorkerResult {
            ok: true,
            summary: if opened {
                tr!("worker-fastboot-connected", "product" => product)
            } else {
                tr!("worker-fastboot-cannot-open")
            },
            payload: Some(serde_json::json!({
                "connected": opened,
                "devices": list,
                "vars": vars,
            })),
        })
    })
}

pub(super) fn reboot() -> Result<WorkerResult> {
    let runtime = runtime()?;
    runtime.block_on(async {
        use hm_fastboot::nusb::NusbFastBoot;

        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        let mut fb = NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;
        fb.reboot().await.context(tr!("fastboot-reboot-error"))?;

        Ok(WorkerResult {
            ok: true,
            summary: tr!("worker-reboot-sent"),
            payload: None,
        })
    })
}

pub(super) fn run_command(command: FastbootCommand, input: &str) -> Result<WorkerResult> {
    let argument = command.argument(input)?;
    let label = if argument.is_empty() {
        command.name().to_owned()
    } else {
        format!("{} {argument}", command.name())
    };
    let runtime = runtime()?;
    runtime.block_on(async {
        use hm_fastboot::nusb::NusbFastBoot;

        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        let mut fb = NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;
        emit_log(&format!("fastboot {label}"));
        let output = async {
            match command {
                FastbootCommand::Getvar => {
                    let value = fb.get_var(argument).await?;
                    Ok(format!("{argument}: {value}"))
                }
                FastbootCommand::Oem => Ok(fb.oem(argument).await?.join("\n")),
                FastbootCommand::Erase => fb.erase(argument).await.map(|()| String::new()),
                FastbootCommand::Continue => fb.continue_boot().await.map(|()| String::new()),
            }
        }
        .await
        .with_context(|| tr!("fastboot-command-error", "command" => label.clone()))?;
        let mut summary = tr!("fastboot-command-done", "command" => label);
        if !output.is_empty() {
            summary.push('\n');
            summary.push_str(&output);
        }
        Ok(WorkerResult {
            ok: true,
            summary,
            payload: None,
        })
    })
}

fn device_json(info: &hm_fastboot::nusb::DeviceInfo) -> serde_json::Value {
    use hm_fastboot::nusb::clean_device_string;

    serde_json::json!({
        "bus": info.bus_id(),
        "addr": info.device_address(),
        "vid": format!("{:04x}", info.vendor_id()),
        "pid": format!("{:04x}", info.product_id()),
        "product": info
            .product_string()
            .map(|s| clean_device_string(s).unwrap_or_else(|| s.to_owned()))
            .unwrap_or_default(),
        "serial": info
            .serial_number()
            .map(|s| clean_device_string(s).unwrap_or_else(|| s.to_owned()))
            .unwrap_or_default(),
    })
}

pub(super) fn flash(image: &Path, target: &str) -> Result<WorkerResult> {
    let runtime = runtime()?;
    runtime.block_on(async {
        use hm_fastboot::nusb::{FlashEvent, NusbFastBoot};
        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        let mut fb = NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;

        let mut progress = |event: FlashEvent<'_>| match event {
            FlashEvent::Message(msg) => emit_log(msg),
            FlashEvent::Part { index, total } => {
                emit_log(&tr!("flash-part-progress", "index" => index, "total" => total));
            }
        };
        fb.flash_image(target, image, &mut progress)
            .await
            .with_context(|| tr!("flash-image-error", "image" => image.display().to_string(), "target" => target.to_owned()))?;
        Ok(WorkerResult {
            ok: true,
            summary: tr!("worker-image-flashed", "image" => image.display().to_string(), "target" => target.to_owned()),
            payload: None,
        })
    })
}

pub(super) fn extract(partition: &str, output: &Path) -> Result<WorkerResult> {
    let runtime = runtime()?;
    runtime.block_on(async {
        use hm_fastboot::nusb::{ExtractPartEvent, NusbFastBoot};

        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        let mut fb = NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;

        let mut progress = |event| match event {
            ExtractPartEvent::Started(range) => emit_log(&tr!(
                "extract-part-range",
                "partition" => partition.to_owned(),
                "offset" => format!("0x{:x}", range.offset),
                "length" => format!("0x{:x}", range.length),
            )),
            ExtractPartEvent::Progress { written, total } => emit_log(&tr!(
                "extract-part-progress",
                "written" => written,
                "total" => total,
            )),
        };
        let range = fb
            .extract_part(partition, output, &mut progress)
            .await
            .with_context(|| {
                tr!(
                    "extract-part-error",
                    "partition" => partition.to_owned(),
                    "output" => output.display().to_string(),
                )
            })?;
        Ok(WorkerResult {
            ok: true,
            summary: tr!(
                "worker-partition-extracted",
                "partition" => partition.to_owned(),
                "output" => output.display().to_string(),
                "length" => range.length,
            ),
            payload: None,
        })
    })
}

async fn read_memory_map(fb: &mut hm_fastboot::nusb::NusbFastBoot) -> Result<Vec<MemoryRegion>> {
    let lines = fb
        .oem("ddrdump")
        .await
        .context(tr!("fastboot-memory-list-error"))?;
    for line in &lines {
        emit_log(line);
    }
    parse_ddrdump(&lines.join("\n"))
}

pub(super) fn memory_list() -> Result<WorkerResult> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        let mut fb = hm_fastboot::nusb::NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;
        let regions = read_memory_map(&mut fb).await?;
        summary_payload(
            tr!("fastboot-memory-listed", "count" => regions.len()),
            MemoryMap {
                device: MemoryDevice::from_info(&info),
                regions,
            },
        )
    })
}

pub(super) fn upload_memory(
    device: &MemoryDevice,
    region: &MemoryRegion,
    output: &Path,
) -> Result<WorkerResult> {
    region.validate()?;
    let runtime = runtime()?;
    let mut fb = runtime.block_on(async {
        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        ensure!(
            &MemoryDevice::from_info(&info) == device,
            "{}",
            tr!("fastboot-memory-device-changed")
        );
        let mut fb = hm_fastboot::nusb::NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;
        ensure!(
            read_memory_map(&mut fb).await?.contains(region),
            "{}",
            tr!("fastboot-memory-region-changed")
        );
        Ok::<_, anyhow::Error>(fb)
    })?;

    fs_util::atomic_write(output, &format!("memory-{}", std::process::id()), |writer| {
        runtime.block_on(async {
            let mut written = 0u64;
            for (address, length) in region.chunks() {
                let params = format!("0x{address:x}:0x{length:x}");
                let data = fb.upload_memory(&params, length).await?;
                ensure!(data.len() == length as usize, "{}", tr!("fastboot-memory-short-read"));
                writer.write_all(&data)?;
                written += u64::from(length);
                emit_log(&tr!("fastboot-memory-progress", "written" => written, "total" => region.size));
            }
            Ok(())
        })
    })
    .with_context(|| tr!("fastboot-memory-download-error", "name" => region.name.clone(), "output" => output.display().to_string()))?;
    Ok(WorkerResult {
        ok: true,
        summary: tr!("fastboot-memory-downloaded", "name" => region.name.clone(), "output" => output.display().to_string(), "length" => region.size),
        payload: None,
    })
}

pub(super) fn storage_analyse() -> Result<WorkerResult> {
    let runtime = runtime()?;
    runtime.block_on(async {
        let devices = hm_fastboot::nusb::devices()
            .await
            .context(tr!("enumerate-usb-error"))?;
        let info = single_device(devices)?;
        let mut fb = hm_fastboot::nusb::NusbFastBoot::from_info(&info)
            .await
            .context(tr!("open-fastboot-device-error"))?;
        let head = fb
            .read_storage_head()
            .await
            .context(tr!("fastboot-storage-head-error"))?;
        let gpt = common::formats::gpt::parse_storage_head(&head)
            .context(tr!("fastboot-storage-parse-error"))?;
        ensure!(
            !gpt.tables.is_empty(),
            "{}",
            tr!("fastboot-storage-no-table")
        );

        emit_log(&tr!(
            "fastboot-storage-header",
            "offset" => format!("0x{:X}", gpt.tables[0].image_offset),
            "block" => gpt.tables[0].block_size,
        ));
        summary_payload(
            tr!("fastboot-storage-listed", "count" => gpt.partition_count()),
            gpt,
        )
    })
}

fn single_device<T>(devices: impl Iterator<Item = T>) -> Result<T> {
    use hm_fastboot::nusb::{DeviceSelectionError, require_single_device};

    require_single_device(devices).map_err(|error| match error {
        DeviceSelectionError::NotFound => {
            anyhow::anyhow!(tr!("fastboot-device-required"))
        }
        DeviceSelectionError::Multiple => {
            anyhow::anyhow!(tr!("fastboot-single-device-required"))
        }
    })
}
