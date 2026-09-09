use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use common::formats::cpio::{Cpio, parse_cpio_mode};
use common::package::UpdateLayout;
use common::{
    entropy,
    formats::{erofs, ext4},
    fs_util, oeminfo, package, ramdisk,
};
use std::path::{Path, PathBuf};

mod fastboot;
mod vcom;

#[derive(Debug, Parser)]
#[command(
    name = "haucet",
    version = common::version::VERSION,
    about,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Unpack update.bin, UPDATE.APP, or a ZIP containing either format
    #[command(arg_required_else_help = true)]
    Unpack(FullUnpackArgs),
    /// Unpack or repack an EROFS image
    #[command(arg_required_else_help = true)]
    Erofs {
        #[command(subcommand)]
        command: ErofsCommand,
    },
    /// Unpack an ext2/ext4 image
    #[command(arg_required_else_help = true)]
    Ext4 {
        #[command(subcommand)]
        command: Ext4Command,
    },
    /// Run commands on a cpio archive and save changes in place
    #[command(arg_required_else_help = true)]
    Cpio {
        /// Input cpio archive, modified in place
        incpio: PathBuf,
        #[command(subcommand)]
        command: CpioCommands,
    },
    /// Inspect HARMONY!/HVB, RVT, GPT, or Huawei secure-image contents
    #[command(arg_required_else_help = true)]
    PartitionInfo { image: PathBuf },
    /// Calculate Shannon entropy for a file
    #[command(arg_required_else_help = true)]
    Entropy { file: PathBuf },
    /// Inspect blocks and A/B copies in a Huawei/Honor OEMINFO image
    #[command(arg_required_else_help = true)]
    Oeminfo {
        image: PathBuf,
        /// Show a sanitized preview for text-like payloads
        #[arg(short, long)]
        preview: bool,
    },
    /// Operate on a device connected in fastboot mode
    #[command(arg_required_else_help = true)]
    Fastboot {
        #[command(subcommand)]
        command: FastbootCommand,
    },
    /// Operate on a HiSilicon bootrom VCOM port
    #[command(arg_required_else_help = true)]
    Vcom {
        #[command(subcommand)]
        command: VcomCommand,
    },
    /// Unpack, repack, or patch a ramdisk image
    #[command(arg_required_else_help = true)]
    Ramdisk {
        #[command(subcommand)]
        command: RamdiskCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FastbootCommand {
    /// List connected fastboot devices
    Devices,
    /// Query a fastboot variable (getvar), such as `product`
    GetVar {
        /// Variable name, e.g. `product` or `max-download-size`
        var: String,
    },
    /// Read a memory range from the device into a file
    #[command(name = "upload-memory")]
    UploadMemory {
        /// Memory range in ADDRESS:LENGTH form, for example 0x100000:0x1000
        params: String,
        /// Output file for the received bytes
        output: PathBuf,
    },
    /// Read a raw storage range from the device into a file
    #[command(name = "upload-storage")]
    UploadStorage {
        /// Storage range in OFFSET:LENGTH form, for example 0x0:0x1000
        params: String,
        /// Output file for the received bytes
        output: PathBuf,
    },
    /// Extract a complete partition from the device into an image file
    #[command(name = "extract-part")]
    ExtractPart {
        /// Partition name, e.g. `oeminfo`, `boot`, or `system`
        partition: String,
        /// Output image file
        output: PathBuf,
    },
    /// Flash an image, using ultraflash automatically when supported
    Flash {
        /// Target partition name, e.g. `updater`, `ramdisk`, or `vendor`
        partition: String,
        /// Image file to flash
        image: PathBuf,
    },
    /// Erase a partition
    Erase {
        /// Target partition name
        partition: String,
    },
    /// Reboot into the bootloader
    #[command(name = "reboot-bootloader")]
    RebootBootloader,
    /// Reboot into recovery
    #[command(name = "reboot-recovery")]
    RebootRecovery,
    /// Reboot into fastboot
    #[command(name = "reboot-fastboot")]
    RebootFastboot,
    /// Reboot the device out of fastboot mode
    Reboot,
    /// Continue booting the device
    Continue,
    /// Send a vendor-specific OEM command, such as `oem device-info`
    Oem {
        /// OEM command and optional arguments
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        command: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
enum VcomCommand {
    /// List recognized Huawei VCOM serial ports
    Devices,
    /// Upload a loader binary to a VCOM port at an address
    Flash {
        /// Serial port name, for example COM3
        port: String,
        /// Target address, written as hexadecimal, for example 0x80000000
        #[arg(value_parser = hisi_vcom::vcom::parse_address)]
        address: u32,
        /// Loader binary to upload
        file: PathBuf,
    },
}

#[derive(Debug, Args)]
struct FullUnpackArgs {
    /// Raw update.bin / UPDATE.APP file or ZIP/ZIP64 package
    input: PathBuf,
    /// Output workspace; extracted images are written to images/
    #[arg(short, long, required_unless_present = "list")]
    out: Option<PathBuf>,
    /// List package images without extracting them
    #[arg(long, conflicts_with_all = ["out", "partitions", "all_erofs", "force"])]
    list: bool,
    /// APP: select images; update.bin: select filesystems to expand
    #[arg(short = 'p', long = "partition")]
    partitions: Vec<String>,
    /// Expand only EROFS filesystems (update.bin only)
    #[arg(long, conflicts_with = "partitions")]
    all_erofs: bool,
    /// Component table layout (update.bin only)
    #[arg(long, default_value_t = UpdateLayout::Auto)]
    layout: UpdateLayout,
    /// APP: replace matching images; update.bin: replace the output workspace
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Subcommand)]
enum ErofsCommand {
    /// Unpack an EROFS image into a workspace directory
    Unpack {
        image: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Repack an EROFS workspace into an image
    Repack {
        workspace: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        allow_grow: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Ext4Command {
    /// Unpack an ext2/ext4 image into a directory
    Unpack {
        image: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RamdiskCommand {
    /// Unpack an HMOS ramdisk image into a workspace directory
    Unpack {
        image: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Repack a ramdisk workspace using its original image
    Repack {
        workspace: PathBuf,
        original_image: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Patch bin/init_early in a ramdisk image
    #[command(alias = "ramdiskpatch")]
    Patch {
        image: PathBuf,
        binary: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum CpioCommands {
    /// Exit 0 if an entry exists, otherwise 1
    Exists {
        /// Archive entry to check
        entry: String,
    },
    /// List entries in the archive
    Ls {
        /// List entries recursively
        #[arg(short = 'r', long)]
        recursive: bool,
        /// Entry path; defaults to `/`
        path: Option<String>,
    },
    /// Remove an entry from the archive
    Rm {
        /// Remove recursively
        #[arg(short = 'r', long)]
        recursive: bool,
        /// Archive entry to remove
        entry: String,
    },
    /// Create a directory in the archive
    Mkdir {
        /// Directory mode in octal, such as `0750`
        mode: String,
        /// Directory entry to create
        entry: String,
    },
    /// Create a symbolic link in the archive
    Ln {
        /// Link target
        src: String,
        /// Link entry to create
        dst: String,
    },
    /// Move an entry in the archive
    Mv {
        /// Source entry
        src: String,
        /// Destination entry
        dst: String,
    },
    /// Add a file to the archive
    Add {
        /// File mode in octal, such as `0750`
        mode: String,
        /// Archive entry to create
        entry: String,
        /// Input file on the host filesystem
        infile: String,
    },
    /// Extract all entries or one entry from the archive
    Extract {
        /// Archive entry to extract; omit together with OUT to extract everything
        #[arg(requires = "out")]
        entry: Option<String>,
        /// Host path to write the extracted entry to
        #[arg(requires = "entry")]
        out: Option<String>,
    },
    /// Exit 0 for stock, 1 for patched, or 2 for unsupported
    Test,
}

fn run_unpack_command(args: FullUnpackArgs) -> Result<()> {
    if args.list {
        let index = package::inspect(&args.input, args.layout)?;
        println!("format: {}", index.format);
        if let Some(version) = index.package_version {
            println!("version: {version}");
        }
        println!(
            "{:<24} {:>12} {:>18}",
            "IMAGE", "SIZE (BYTES)", "DATA OFFSET"
        );
        for component in index.components {
            println!(
                "{:<24} {:>12} {:#018X}",
                component.output_name, component.size, component.data_offset
            );
        }
        return Ok(());
    }
    package::unpack_full(
        &args.input,
        args.out
            .as_deref()
            .context("--out is required for unpacking")?,
        &args.partitions,
        args.all_erofs,
        args.layout,
        args.force,
    )?;
    Ok(())
}

fn run_erofs_command(command: ErofsCommand) -> Result<()> {
    match command {
        ErofsCommand::Unpack { image, out, force } => {
            erofs::unpack(&image, &out, force)?;
            Ok(())
        }
        ErofsCommand::Repack {
            workspace,
            output,
            allow_grow,
        } => erofs::repack(&workspace, &output, allow_grow),
    }
}

fn run_ext4_command(command: Ext4Command) -> Result<()> {
    match command {
        Ext4Command::Unpack { image, out, force } => {
            ext4::unpack(&image, &out, force)?;
            Ok(())
        }
    }
}

fn run_partition_info_command(image: PathBuf) -> Result<()> {
    Ok(common::partition::info(&image)?)
}

fn run_entropy_command(file: PathBuf) -> Result<()> {
    let summary = entropy::analyze_file(&file)?;
    println!("file: {}", file.display());
    println!("size: {} bytes", summary.size);
    println!(
        "entropy: {:.6} bits/byte ({:.2}%)",
        summary.entropy_bits_per_byte,
        summary.normalized_percent()
    );
    println!("unique byte values: {}", summary.unique_bytes);
    if let Some(most_common) = summary.most_common {
        println!(
            "most common byte: 0x{:02X} ({} bytes, {:.2}%)",
            most_common.byte,
            most_common.count,
            most_common.ratio * 100.0
        );
    }
    Ok(())
}

fn run_oeminfo_command(image: PathBuf, preview: bool) -> Result<()> {
    let summary = oeminfo::inspect(&image)?;
    println!("file: {}", image.display());
    println!(
        "size: {} bytes (0x{:X})",
        summary.file_size, summary.file_size
    );
    println!(
        "banks: 2 x {} bytes (0x{:X})",
        summary.region_size, summary.region_size
    );
    println!(
        "blocks: {} ({} active, {} inactive; A {}, B {}, outside {})",
        summary.total_blocks,
        summary.active_blocks,
        summary.inactive_blocks,
        summary.region_a_blocks,
        summary.region_b_blocks,
        summary.unknown_region_blocks
    );
    println!(
        "layouts: {} standard, {} compact, {} reused",
        summary.standard_blocks, summary.compact_blocks, summary.reused_blocks
    );
    let overview = oeminfo::overview(&summary);
    if let Some(model) = overview.product_model.as_deref() {
        println!("model: {model}");
    }
    if let Some(version) = overview.base_version.as_deref() {
        println!("base version: {version}");
    }
    if let Some(version) = overview.full_version.as_deref() {
        println!("full version: {version}");
    }
    if let Some(version) = overview.base_component.as_deref() {
        println!("base component (86): {version}");
    }
    if let Some(version) = overview.cust_version.as_deref() {
        println!("cust version (80): {version}");
    }
    if let Some(version) = overview.preload_version.as_deref() {
        println!("preload version (82): {version}");
    }
    if let Some(present) = overview.device_certificate {
        println!(
            "device certificate (2601/2603): {}",
            if present { "present" } else { "absent" }
        );
    }
    for (id, sub_id, text) in &overview.other_versions {
        println!("version-like ({id}/{sub_id}): {text}");
    }
    if summary.discarded_headers != 0 {
        println!(
            "warning: discarded {} of {} OEM_INFO header candidates",
            summary.discarded_headers, summary.candidate_headers
        );
    }
    println!();
    println!(
        "{:<10} {:>6} {:>6} {:>7} {:>8} {:>10} {:<16} {:<8} TYPE",
        "OFFSET", "ID", "SUB", "AGE", "LENGTH", "BANK/COPY", "LAYOUT", "HEADER"
    );
    for block in summary.blocks {
        println!(
            "0x{:08X} {:>6} {:>6} {:>7} {:>8} {:<10} {:<16} 0x{:03X} {}",
            block.offset,
            block.id,
            block.sub_id,
            block.age,
            block.length,
            format!(
                "{}/{}",
                block.region,
                if block.active { "active" } else { "backup" }
            ),
            block.layout,
            block.header_size,
            block.payload_kind
        );
        if let (true, Some(text)) = (preview, block.text_preview) {
            println!("  preview: {text}");
        }
        if let Some(version) = block.image_version_hex {
            println!(
                "  image: version=0x{} random_adjust={}",
                version,
                block.image_random_adjust.unwrap_or(0)
            );
        } else if let Some(description) = block.tlv_description {
            println!("  tlv: {} parts ({description})", block.tlv_parts);
        }
    }
    Ok(())
}

fn run_cpio_command(file: &Path, command: CpioCommands) -> Result<()> {
    let file_str = file
        .to_str()
        .with_context(|| format!("cpio path is not valid UTF-8: {}", file.display()))?;
    let mut cpio = if file.exists() {
        Cpio::load_from_file(file_str)?
    } else {
        Cpio::new()
    };

    let status = match command {
        CpioCommands::Exists { entry } => i32::from(!cpio.exists(&entry)),
        CpioCommands::Ls { recursive, path } => {
            cpio.ls(path.as_deref().unwrap_or("/"), recursive);
            0
        }
        CpioCommands::Rm { recursive, entry } => {
            cpio.rm(&entry, recursive);
            cpio.dump(file_str)?;
            0
        }
        CpioCommands::Mkdir { mode, entry } => {
            cpio.mkdir(parse_cpio_mode(&mode)?, &entry);
            cpio.dump(file_str)?;
            0
        }
        CpioCommands::Ln { src, dst } => {
            cpio.ln(&src, &dst);
            cpio.dump(file_str)?;
            0
        }
        CpioCommands::Mv { src, dst } => {
            cpio.mv(&src, &dst)?;
            cpio.dump(file_str)?;
            0
        }
        CpioCommands::Add {
            mode,
            entry,
            infile,
        } => {
            cpio.add(parse_cpio_mode(&mode)?, &entry, &infile)?;
            cpio.dump(file_str)?;
            0
        }
        CpioCommands::Extract { entry, out } => {
            match (entry.as_deref(), out.as_deref()) {
                (None, None) => {
                    cpio.extract(&[])?;
                }
                (Some(entry), Some(out)) => cpio.extract(&[entry, out])?,
                _ => unreachable!("clap requires extract entry and output together"),
            }
            0
        }
        CpioCommands::Test => match ramdisk::patch_status(&cpio) {
            ramdisk::RamdiskPatchStatus::Patchable => 0,
            ramdisk::RamdiskPatchStatus::Patched => 1,
            ramdisk::RamdiskPatchStatus::Unsupported => 2,
        },
    };

    if status != 0 {
        std::process::exit(status);
    }
    Ok(())
}

fn run_fastboot_command(command: FastbootCommand) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create tokio runtime fail")?;
    runtime.block_on(async move {
        match command {
            FastbootCommand::Devices => fastboot::devices().await,
            FastbootCommand::GetVar { var } => fastboot::get_var(&var).await,
            FastbootCommand::UploadMemory { params, output } => {
                fastboot::upload_memory(&params, &output).await
            }
            FastbootCommand::UploadStorage { params, output } => {
                fastboot::upload_storage(&params, &output).await
            }
            FastbootCommand::ExtractPart { partition, output } => {
                fastboot::extract_part(&partition, &output).await
            }
            FastbootCommand::Flash { partition, image } => {
                fastboot::flash(&partition, &image).await
            }
            FastbootCommand::Erase { partition } => fastboot::erase(&partition).await,
            FastbootCommand::RebootBootloader => fastboot::reboot_bootloader().await,
            FastbootCommand::RebootRecovery => fastboot::reboot_recovery().await,
            FastbootCommand::RebootFastboot => fastboot::reboot_fastboot().await,
            FastbootCommand::Reboot => fastboot::reboot().await,
            FastbootCommand::Continue => fastboot::continue_boot().await,
            FastbootCommand::Oem { command } => fastboot::oem(&command).await,
        }
    })
}

fn run_vcom_command(command: VcomCommand) -> Result<()> {
    match command {
        VcomCommand::Devices => vcom::devices(),
        VcomCommand::Flash {
            port,
            address,
            file,
        } => vcom::flash(&port, address, &file),
    }
}

fn run_ramdisk_command(command: RamdiskCommand) -> Result<()> {
    match command {
        RamdiskCommand::Unpack { image, out, force } => {
            let image = fs_util::canonical_path(&image)?;
            fs_util::prepare_dir_excluding(&out, "output directory", force, &[&image])?;
            Ok(ramdisk::unpack(&image, &out)?)
        }
        RamdiskCommand::Repack {
            workspace,
            original_image,
            out,
        } => {
            let workspace = fs_util::canonical_path(&workspace)?;
            let original_image = fs_util::canonical_path(&original_image)?;
            let out = fs_util::absolute_path(&out)?;
            Ok(ramdisk::repack(&workspace, &original_image, &out)?)
        }
        RamdiskCommand::Patch { image, binary, out } => Ok(ramdisk::patch(&image, &binary, &out)?),
    }
}

fn main() {
    let result = match Cli::parse().command {
        Command::Unpack(args) => run_unpack_command(args),
        Command::Erofs { command } => run_erofs_command(command),
        Command::Ext4 { command } => run_ext4_command(command),
        Command::Cpio { incpio, command } => run_cpio_command(&incpio, command),
        Command::PartitionInfo { image } => run_partition_info_command(image),
        Command::Entropy { file } => run_entropy_command(file),
        Command::Oeminfo { image, preview } => run_oeminfo_command(image, preview),
        Command::Fastboot { command } => run_fastboot_command(command),
        Command::Vcom { command } => run_vcom_command(command),
        Command::Ramdisk { command } => run_ramdisk_command(command),
    };

    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
