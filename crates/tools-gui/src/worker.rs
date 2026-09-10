pub(crate) mod fastboot;

use self::fastboot::{FastbootCommand, MemoryDevice, MemoryRegion};
use anyhow::{Context, Result, ensure};
use common::formats::{cpio, erofs, ext4, header::check_fmt_full};
use common::package::{PackageFormat, UpdateLayout};
use common::{entropy, fs_util, nvme, oeminfo, package, partition, ramdisk};
use hisi_vcom::transport::{self, SerialVcomDevice};
use hisi_vcom::vcom;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::Path;

pub const WORKER_ENV: &str = "HAUCET_GUI_WORKER";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JobOp {
    OnlineFetch {
        url: String,
    },
    PackageInspect {
        input: String,
        layout: UpdateLayout,
    },
    PackageUnpack {
        input: String,
        output: String,
        partitions: Vec<String>,
        all_erofs: bool,
        layout: UpdateLayout,
        force: bool,
    },
    ErofsUnpack {
        image: String,
        output: String,
        force: bool,
    },
    ErofsRepack {
        workspace: String,
        output: String,
        allow_grow: bool,
    },
    Ext4Unpack {
        image: String,
        output: String,
        force: bool,
    },
    RamdiskUnpack {
        image: String,
        output: String,
        force: bool,
    },
    RamdiskRepack {
        workspace: String,
        original: String,
        output: String,
    },
    RamdiskPatch {
        image: String,
        binary: String,
        output: String,
    },
    RamdiskProbe {
        image: String,
    },
    PartitionInfo {
        image: String,
    },
    NvmeInspect {
        image: String,
    },
    OemInfoInspect {
        image: String,
    },
    OemInfoExportImage {
        image: String,
        block: oeminfo::OemInfoBlockSummary,
        output: String,
    },
    NvmeEdit {
        image: String,
        key: String,
        value: String,
        value_format: String,
    },
    FastbootStatus {},
    FastbootReboot {},
    FastbootCommand {
        command: FastbootCommand,
        argument: String,
    },
    FastbootFlash {
        image: String,
        target: String,
    },
    FastbootExtract {
        partition: String,
        output: String,
    },
    FastbootMemoryList {},
    FastbootUploadMemory {
        device: MemoryDevice,
        region: MemoryRegion,
        output: String,
    },
    FastbootStorageAnalyse {},
    VcomStatus {},
    VcomFlash {
        port: String,
        address: u32,
        file: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub op: JobOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResult {
    pub ok: bool,
    pub summary: String,
    pub payload: Option<serde_json::Value>,
}

pub fn is_worker_mode() -> bool {
    std::env::var_os(WORKER_ENV).is_some()
}

pub fn run_worker() -> i32 {
    let mut input = String::new();
    let read = std::io::stdin().read_to_string(&mut input);
    let result = match read {
        Ok(_) => serde_json::from_str::<JobSpec>(&input)
            .context(tr!("worker-invalid-spec"))
            .and_then(|spec| execute(&spec.op)),
        Err(e) => Err(e).context(tr!("worker-read-spec-error")),
    };
    let result = match result {
        Ok(result) => result,
        Err(e) => WorkerResult {
            ok: false,
            summary: format!("{e:#}"),
            payload: None,
        },
    };
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "t": "result",
            "ok": result.ok,
            "summary": result.summary,
            "payload": result.payload,
        }))
        .expect("serializing worker result")
    );
    0
}

fn execute(op: &JobOp) -> Result<WorkerResult> {
    match op {
        JobOp::OnlineFetch { url } => {
            let info = online_fetcher::fetch_version(url)?;
            summary_payload(
                tr!("online-fetched", "bytes" => info.downloaded_bytes, "requests" => info.range_requests),
                info,
            )
        }
        JobOp::PackageInspect { input, layout } => {
            let index = package::inspect(Path::new(input), *layout)?;
            let summary = match index.format {
                PackageFormat::UpdateApp => {
                    tr!("worker-app-inspected", "count" => index.components.len())
                }
                PackageFormat::UpdateBin => {
                    tr!("worker-package-inspected", "count" => index.components.len(), "layout" => layout_label(index.layout))
                }
            };
            summary_payload(summary, index)
        }
        JobOp::PackageUnpack {
            input,
            output,
            partitions,
            all_erofs,
            layout,
            force,
        } => {
            package::unpack_full(
                Path::new(input),
                Path::new(output),
                partitions,
                *all_erofs,
                *layout,
                *force,
            )?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!("worker-package-unpacked", "output" => output.clone()),
                payload: None,
            })
        }
        JobOp::ErofsUnpack {
            image,
            output,
            force,
        } => {
            erofs::unpack(Path::new(image), Path::new(output), *force)?;
            let manifest = erofs::read_manifest(Path::new(output))?;
            summary_payload(
                tr!("worker-erofs-unpacked", "output" => output.clone(), "partition" => manifest.partition.clone()),
                manifest,
            )
        }
        JobOp::ErofsRepack {
            workspace,
            output,
            allow_grow,
        } => {
            erofs::repack(Path::new(workspace), Path::new(output), *allow_grow)?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!("worker-erofs-repacked", "output" => output.clone()),
                payload: None,
            })
        }
        JobOp::Ext4Unpack {
            image,
            output,
            force,
        } => {
            let report = ext4::unpack(Path::new(image), Path::new(output), *force)?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!(
                    "worker-ext4-unpacked",
                    "output" => output.clone(),
                    "files" => report.files,
                    "directories" => report.directories
                ),
                payload: None,
            })
        }
        JobOp::RamdiskUnpack {
            image,
            output,
            force,
        } => {
            let image = fs_util::canonical_path(Path::new(image))?;
            fs_util::prepare_dir_excluding(
                Path::new(output),
                &tr!("output-directory"),
                *force,
                &[&image],
            )?;
            ramdisk::unpack(&image, Path::new(output))?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!("worker-ramdisk-unpacked", "output" => output.clone()),
                payload: None,
            })
        }
        JobOp::RamdiskRepack {
            workspace,
            original,
            output,
        } => {
            let workspace = fs_util::canonical_path(Path::new(workspace))?;
            let original = fs_util::canonical_path(Path::new(original))?;
            let output = fs_util::absolute_path(Path::new(output))?;
            ramdisk::repack(&workspace, &original, &output)?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!("worker-ramdisk-repacked", "output" => output.display().to_string()),
                payload: None,
            })
        }
        JobOp::RamdiskPatch {
            image,
            binary,
            output,
        } => {
            let output = fs_util::absolute_path(Path::new(output))?;
            ramdisk::patch(Path::new(image), Path::new(binary), &output)?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!("worker-patch-written", "output" => output.display().to_string()),
                payload: None,
            })
        }
        JobOp::RamdiskProbe { image } => {
            let payload = probe_ramdisk(Path::new(image))?;
            let summary = match (
                payload["patched"].as_bool().unwrap_or(false),
                payload["layout_known"].as_bool().unwrap_or(false),
            ) {
                (true, _) => tr!("worker-image-patched"),
                (false, true) => tr!("stock-image-patchable"),
                _ => tr!("worker-ramdisk-layout-unknown"),
            };
            Ok(WorkerResult {
                ok: true,
                summary,
                payload: Some(payload),
            })
        }
        JobOp::PartitionInfo { image } => {
            let entropy_summary = entropy::analyze_file(Path::new(image))?;
            let partition_summary = partition::summarize(Path::new(image)).ok();
            let label = match &partition_summary {
                Some(partition::PartitionSummary::Harmony(h)) => {
                    tr!("worker-harmony-partition", "partition" => h.cert.partition_name.clone())
                }
                Some(partition::PartitionSummary::Rvt(info)) => {
                    tr!("worker-rvt-image", "count" => info.descriptors.len())
                }
                Some(partition::PartitionSummary::Gpt(info)) => {
                    tr!("worker-gpt-image", "tables" => info.tables.len(), "partitions" => info.partition_count())
                }
                Some(partition::PartitionSummary::SecImage(info)) => tr!(
                    "worker-sec-image",
                    "image" => info.image_name.clone(),
                    "partition" => info.partition_name.clone(),
                ),
                Some(partition::PartitionSummary::HvbWrapped { .. }) => {
                    tr!("hvb-wrapped-partition-image")
                }
                None => tr!("worker-partition-unknown"),
            };
            summary_payload(
                tr!(
                    "worker-partition-entropy",
                    "label" => label,
                    "entropy" => format!("{:.6}", entropy_summary.entropy_bits_per_byte),
                    "percent" => format!("{:.2}", entropy_summary.normalized_percent()),
                ),
                serde_json::json!({
                    "partition": partition_summary,
                    "entropy": entropy_summary,
                }),
            )
        }
        JobOp::NvmeInspect { image } => {
            let summary = nvme::inspect(Path::new(image))?;
            let crc_status = if summary.crc_supported {
                tr!("worker-crc-errors", "count" => summary.crc_invalid)
            } else {
                tr!("worker-crc-disabled")
            };
            summary_payload(
                tr!("worker-nve-summary", "copies" => summary.active_blocks, "entries" => summary.valid_items, "crc" => crc_status),
                summary,
            )
        }
        JobOp::OemInfoInspect { image } => {
            let summary = oeminfo::inspect(Path::new(image))?;
            summary_payload(
                tr!("worker-oeminfo-summary", "total" => summary.total_blocks, "active" => summary.active_blocks, "inactive" => summary.inactive_blocks),
                summary,
            )
        }
        JobOp::OemInfoExportImage {
            image,
            block,
            output,
        } => {
            oeminfo::export_embedded_image(Path::new(image), block, Path::new(output))?;
            Ok(WorkerResult {
                ok: true,
                summary: tr!("worker-oeminfo-exported", "output" => output.clone()),
                payload: None,
            })
        }
        JobOp::NvmeEdit {
            image,
            key,
            value,
            value_format,
        } => {
            let result = nvme::edit_file_in_place(Path::new(image), key, value, value_format)?;
            summary_payload(
                tr!(
                    "worker-nve-edited",
                    "source" => result.source_block,
                    "target" => result.committed_block,
                    "count" => result.updated_items,
                    "generation" => result.age,
                    "backup" => result.backup_path.clone(),
                ),
                result,
            )
        }
        JobOp::FastbootStatus {} => fastboot::status(),
        JobOp::FastbootReboot {} => fastboot::reboot(),
        JobOp::FastbootCommand { command, argument } => fastboot::run_command(*command, argument),
        JobOp::FastbootFlash { image, target } => {
            ensure!(!target.trim().is_empty(), "{}", tr!("partition-name-empty"));
            fastboot::flash(Path::new(image), target.trim())
        }
        JobOp::FastbootExtract { partition, output } => {
            ensure!(
                !partition.trim().is_empty(),
                "{}",
                tr!("partition-name-empty")
            );
            fastboot::extract(partition.trim(), Path::new(output))
        }
        JobOp::FastbootMemoryList {} => fastboot::memory_list(),
        JobOp::FastbootUploadMemory {
            device,
            region,
            output,
        } => fastboot::upload_memory(device, region, Path::new(output)),
        JobOp::FastbootStorageAnalyse {} => fastboot::storage_analyse(),
        JobOp::VcomStatus {} => vcom_status(),
        JobOp::VcomFlash {
            port,
            address,
            file,
        } => vcom_flash(port.trim(), *address, Path::new(file)),
    }
}

fn vcom_status() -> Result<WorkerResult> {
    let ports = transport::list_vcom_serial_ports()?
        .into_iter()
        .map(|port| {
            serde_json::json!({
                "name": port.name,
                "description": port.description,
            })
        })
        .collect::<Vec<_>>();
    let serial_count = ports.len();

    Ok(WorkerResult {
        ok: true,
        summary: tr!("worker-vcom-status", "ports" => serial_count),
        payload: Some(serde_json::json!({
            "ports": ports,
        })),
    })
}

fn vcom_flash(port: &str, address: u32, file: &Path) -> Result<WorkerResult> {
    ensure!(!port.is_empty(), "{}", tr!("vcom-port-empty"));
    let data = fs::read(file)
        .with_context(|| tr!("reading-file", "file" => file.display().to_string()))?;
    let mut device = SerialVcomDevice::open(port, 115200)
        .with_context(|| tr!("opening-vcom-port", "port" => port.to_owned()))?;
    let mut log = |message: &str| emit_log(message);

    vcom::upload(&mut device, &data, address, &mut log, &mut |sent, total| {
        if total > 0 && (sent == total || sent % (total / 10 + 1) == 0) {
            emit_log(&tr!("progress-bytes", "sent" => sent, "total" => total));
        }
    })?;

    Ok(WorkerResult {
        ok: true,
        summary: tr!(
            "worker-vcom-finished",
            "file" => file.display().to_string(),
            "port" => port.to_owned(),
            "address" => format!("0x{address:08X}"),
        ),
        payload: Some(serde_json::json!({
            "port": port,
            "address": format!("0x{address:08X}"),
            "bytes": data.len(),
        })),
    })
}

fn emit_log(text: &str) {
    let line = serde_json::to_string(&serde_json::json!({ "t": "log", "s": text }))
        .expect("serializing log line");
    println!("{line}");
}

fn probe_ramdisk(image: &Path) -> Result<serde_json::Value> {
    let frame = common::formats::harmony::HvbFrame::load(image)
        .with_context(|| tr!("reading-file", "file" => image.display().to_string()))?;
    let payload = frame.extract_image_payload();
    ensure!(!payload.is_empty(), "{}", tr!("image-no-payload"));
    let fmt = check_fmt_full(payload);
    let cpio_bytes = if fmt.is_compressed() {
        common::compress::decompress_vec(fmt, payload).map_err(std::io::Error::other)?
    } else {
        payload.to_vec()
    };
    let cpio = cpio::Cpio::load_from_data(&cpio_bytes)?;
    let patch_status = ramdisk::patch_status(&cpio);
    let patched = patch_status == ramdisk::RamdiskPatchStatus::Patched;
    let has_init_early = cpio.exists("bin/init_early");
    Ok(serde_json::json!({
        "patched": patched,
        "has_init_early": has_init_early,
        "layout_known": patch_status != ramdisk::RamdiskPatchStatus::Unsupported,
        "payload_format": fmt.as_str(),
        "payload_len": payload.len(),
        "header_size": frame.harmony.hdr_size,
        "cert_image_len": frame.cert.image_len,
        "cert_original_len": frame.cert.image_original_len,
    }))
}

fn summary_payload<T: Serialize>(summary: String, payload: T) -> Result<WorkerResult> {
    Ok(WorkerResult {
        ok: true,
        summary,
        payload: Some(serde_json::to_value(payload)?),
    })
}

fn layout_label(layout: UpdateLayout) -> &'static str {
    match layout {
        UpdateLayout::Auto => "auto",
        UpdateLayout::L1 => "L1",
        UpdateLayout::L2 => "L2",
    }
}
