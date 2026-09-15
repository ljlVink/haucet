use anyhow::{Context, Result, bail, ensure};
use common::flash::{FlashScript, FlashStep, RebootMode, ResolvedScript, ResolvedStep};
use hisi_vcom::transport::{self, SerialVcomDevice};
use hisi_vcom::vcom;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const VCOM_BAUD: u32 = 115_200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortChoice {
    pub name: String,
    pub description: String,
}

pub trait FlashHost {
    fn log(&mut self, message: &str);
    fn progress(&mut self, step: usize, total: usize, step_ref: &FlashStep);
    fn select_port(&mut self, ports: &[PortChoice]) -> Result<String>;
    fn alert(&mut self, message: &str) -> Result<()>;
    fn transfer(&mut self, sent: u64, total: u64);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub name: String,
    pub completed: usize,
    pub total: usize,
}

pub fn load(path: &Path) -> Result<(FlashScript, ResolvedScript)> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading flash script {}", path.display()))?;
    let script = FlashScript::parse(&text)?;
    let base = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let resolved = script.resolve(base)?;
    Ok((script, resolved))
}

pub fn describe(step: &FlashStep) -> String {
    match step {
        FlashStep::WaitVcom { timeout_secs } => {
            format!("wait for VCOM port (up to {timeout_secs}s)")
        }
        FlashStep::VcomUpload { port, file, .. } => {
            format!("VCOM upload {file} via {port}")
        }
        FlashStep::WaitFastboot { timeout_secs } => {
            format!("wait for fastboot device (up to {timeout_secs}s)")
        }
        FlashStep::FastbootAssert { variable, value } => format!("assert {variable} = {value}"),
        FlashStep::FastbootFlash { partition, file } => format!("flash {file} to {partition}"),
        FlashStep::FastbootErase { partition } => format!("erase {partition}"),
        FlashStep::FastbootOem { command } => format!("oem {command}"),
        FlashStep::FastbootReboot { mode } => format!("reboot ({})", mode_name(*mode)),
        FlashStep::Alert { message } => format!("alert: {message}"),
        FlashStep::Sleep { millis } => format!("sleep {millis} ms"),
    }
}

pub fn mode_name(mode: RebootMode) -> &'static str {
    match mode {
        RebootMode::System => "system",
        RebootMode::Bootloader => "bootloader",
        RebootMode::Fastboot => "fastboot",
        RebootMode::Recovery => "recovery",
        RebootMode::Continue => "continue",
    }
}

pub fn resolve_port(spec: &str, ports: &[PortChoice], host: &mut dyn FlashHost) -> Result<String> {
    let spec = spec.trim();
    if !spec.eq_ignore_ascii_case("auto") {
        ensure!(
            ports.iter().any(|port| port.name == spec),
            "VCOM port {spec} not found; available: {}",
            port_list(ports)
        );
        return Ok(spec.to_owned());
    }
    match ports {
        [] => bail!("no VCOM serial port found"),
        [only] => Ok(only.name.clone()),
        many => host.select_port(many),
    }
}

fn port_list(ports: &[PortChoice]) -> String {
    ports
        .iter()
        .map(|port| format!("{} ({})", port.name, port.description))
        .collect::<Vec<_>>()
        .join(", ")
}

fn vcom_ports() -> Result<Vec<PortChoice>> {
    Ok(transport::list_vcom_serial_ports()?
        .into_iter()
        .map(|port| PortChoice {
            name: port.name,
            description: port.description,
        })
        .collect())
}

pub fn run(script: &ResolvedScript, host: &mut dyn FlashHost) -> Result<RunSummary> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating the flash executor runtime")?;
    let total = script.steps.len();
    for (index, resolved) in script.steps.iter().enumerate() {
        host.progress(index, total, &resolved.step);
        run_step(resolved, host, &runtime).with_context(|| {
            format!("step {}/{} ({}) failed", index + 1, total, resolved.kind())
        })?;
    }
    Ok(RunSummary {
        name: script.name.clone(),
        completed: total,
        total,
    })
}

fn run_step(
    resolved: &ResolvedStep,
    host: &mut dyn FlashHost,
    runtime: &tokio::runtime::Runtime,
) -> Result<()> {
    match &resolved.step {
        FlashStep::WaitVcom { timeout_secs } => {
            let deadline = Instant::now() + Duration::from_secs(*timeout_secs);
            loop {
                let ports = vcom_ports()?;
                if let [only] = ports.as_slice() {
                    host.log(&format!("VCOM port present: {}", only.name));
                    return Ok(());
                }
                if !ports.is_empty() {
                    host.log(&format!("{} VCOM ports present", ports.len()));
                    return Ok(());
                }
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for a VCOM port"
                );
                thread::sleep(POLL_INTERVAL);
            }
        }
        FlashStep::VcomUpload { port, address, .. } => {
            let ports = vcom_ports()?;
            let port = resolve_port(port, &ports, host)?;
            let address = vcom::parse_address(address)
                .map_err(|error| anyhow::anyhow!("invalid VCOM address: {error}"))?;
            let file = resolved
                .file
                .as_deref()
                .expect("resolve guarantees a file for vcom_upload");
            let data = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            ensure!(!data.is_empty(), "loader file is empty: {}", file.display());
            host.log(&format!(
                "uploading {} ({} bytes) via {port}",
                file.display(),
                data.len()
            ));
            let mut device = SerialVcomDevice::open(&port, VCOM_BAUD)
                .with_context(|| format!("opening VCOM port {port}"))?;
            let host = RefCell::new(host);
            vcom::upload(
                &mut device,
                &data,
                address,
                &mut |message| host.borrow_mut().log(message),
                &mut |sent, total| host.borrow_mut().transfer(sent, total),
            )
            .map_err(|error| anyhow::anyhow!("VCOM upload failed: {error}"))?;
            host.borrow_mut().log("VCOM upload finished");
            Ok(())
        }
        FlashStep::WaitFastboot { timeout_secs } => {
            let deadline = Instant::now() + Duration::from_secs(*timeout_secs);
            loop {
                let found = fastboot_count(runtime)?;
                if found == 1 {
                    host.log("fastboot device present");
                    return Ok(());
                }
                ensure!(
                    found == 0,
                    "{found} fastboot devices found; connect exactly one"
                );
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for a fastboot device"
                );
                thread::sleep(POLL_INTERVAL);
            }
        }
        FlashStep::FastbootAssert { variable, value } => {
            with_fastboot(runtime, |mut fb| async move {
                let actual = fb.get_var(variable).await?;
                ensure!(
                    actual.trim() == value.trim(),
                    "device {variable} is {actual:?}; script requires {value:?}"
                );
                Ok(())
            })
        }
        FlashStep::FastbootFlash { partition, .. } => {
            let file = resolved
                .file
                .as_deref()
                .expect("resolve guarantees a file for fastboot_flash");
            let host = RefCell::new(host);
            let partition = partition.clone();
            let file = file.to_owned();
            with_fastboot(runtime, move |mut fb| async move {
                let mut progress = |event: hm_fastboot::nusb::FlashEvent<'_>| match event {
                    hm_fastboot::nusb::FlashEvent::Message(message) => {
                        host.borrow_mut().log(message)
                    }
                    hm_fastboot::nusb::FlashEvent::Part { index, total } => host
                        .borrow_mut()
                        .log(&format!("flash part {}/{}", index + 1, total)),
                };
                fb.flash_image(&partition, &file, &mut progress).await?;
                Ok(())
            })
        }
        FlashStep::FastbootErase { partition } => {
            let partition = partition.clone();
            with_fastboot(
                runtime,
                |mut fb| async move { Ok(fb.erase(&partition).await?) },
            )
        }
        FlashStep::FastbootOem { command } => {
            let command = command.clone();
            with_fastboot(runtime, |mut fb| async move {
                for line in fb.oem(&command).await? {
                    host.log(&line);
                }
                Ok(())
            })
        }
        FlashStep::FastbootReboot { mode } => {
            let mode = *mode;
            with_fastboot(runtime, |mut fb| async move {
                let result = match mode {
                    RebootMode::System => fb.reboot().await,
                    RebootMode::Bootloader => fb.reboot_bootloader().await,
                    RebootMode::Fastboot => fb.reboot_fastboot().await,
                    RebootMode::Recovery => fb.reboot_recovery().await,
                    RebootMode::Continue => fb.continue_boot().await,
                };
                Ok(result?)
            })
        }
        FlashStep::Alert { message } => host.alert(message),
        FlashStep::Sleep { millis } => {
            thread::sleep(Duration::from_millis(*millis));
            Ok(())
        }
    }
}

fn fastboot_count(runtime: &tokio::runtime::Runtime) -> Result<usize> {
    runtime.block_on(async {
        let devices: Vec<_> = hm_fastboot::nusb::devices()
            .await
            .context("enumerating USB devices")?
            .collect();
        Ok(devices.len())
    })
}

fn with_fastboot<F, Fut>(runtime: &tokio::runtime::Runtime, op: F) -> Result<()>
where
    F: FnOnce(hm_fastboot::nusb::NusbFastBoot) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    runtime.block_on(async {
        let devices: Vec<_> = hm_fastboot::nusb::devices()
            .await
            .context("enumerating USB devices")?
            .collect();
        let count = devices.len();
        ensure!(
            count == 1,
            "expected exactly one fastboot device, found {count}"
        );
        let info = devices.into_iter().next().expect("count checked");
        let fb = hm_fastboot::nusb::NusbFastBoot::from_info(&info)
            .await
            .map_err(|error| anyhow::anyhow!("opening fastboot device: {error}"))?;
        op(fb).await
    })
}

pub fn report_bucket(sent: u64, total: u64, last_bucket: &mut u64) -> bool {
    if total == 0 {
        return false;
    }
    let bucket = sent.min(total).saturating_mul(10) / total;
    if bucket <= *last_bucket {
        return false;
    }
    *last_bucket = bucket;
    true
}
