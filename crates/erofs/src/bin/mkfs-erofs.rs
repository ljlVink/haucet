use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use erofs::{BuildOptions, Compression};

#[derive(Parser)]
#[command(
    name = "mkfs-erofs",
    version,
    about = "Build an EROFS filesystem from a directory"
)]
struct Args {
    #[arg(short = 'z', default_value = "lz4hc")]
    compression: Compression,
    #[arg(short = 'b', default_value_t = 4096)]
    block_size: u32,
    #[arg(short = 'C', default_value_t = 4096)]
    cluster_size: u32,
    #[arg(short = 'T')]
    timestamp: Option<u64>,
    #[arg(short = 'U')]
    uuid: Option<String>,
    #[arg(short = 'L', default_value = "")]
    volume_label: String,
    #[arg(long = "fs-config-file")]
    fs_config: Option<PathBuf>,
    #[arg(long)]
    file_contexts: Option<PathBuf>,
    #[arg(long)]
    metadata: Option<PathBuf>,
    #[arg(long, default_value = "")]
    mount_point: String,
    #[arg(long)]
    all_root: bool,
    #[arg(long)]
    force_uid: Option<u32>,
    #[arg(long)]
    force_gid: Option<u32>,
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    uid_offset: i64,
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    gid_offset: i64,
    #[arg(long)]
    ignore_mtime: bool,
    #[arg(short = 'x', allow_hyphen_values = true)]
    xattr_tolerance: Option<i32>,
    #[arg(short = 'E')]
    extended_options: Option<String>,
    output: PathBuf,
    source: PathBuf,
}

fn run(args: Args) -> anyhow::Result<()> {
    let mut extra = Vec::new();
    if let Some(uuid) = args.uuid {
        extra.extend(["-U".to_owned(), uuid]);
    }
    if let Some(features) = args.extended_options {
        extra.extend(["-E".to_owned(), features]);
    }
    if let Some(tolerance) = args.xattr_tolerance {
        extra.push(format!("-x{tolerance}"));
    }
    let options = BuildOptions {
        compression: args.compression,
        block_size: args.block_size,
        cluster_size: args.cluster_size,
        timestamp: args.timestamp,
        volume_label: args.volume_label,
        fs_config: args.fs_config,
        file_contexts: args.file_contexts,
        metadata_file: args.metadata,
        mount_point: args.mount_point,
        force_uid: if args.all_root {
            Some(0)
        } else {
            args.force_uid
        },
        force_gid: if args.all_root {
            Some(0)
        } else {
            args.force_gid
        },
        uid_offset: args.uid_offset,
        gid_offset: args.gid_offset,
        preserve_mtime: !args.ignore_mtime,
        ..BuildOptions::from_args(&extra)?
    };
    options.validate()?;
    let report = erofs::build(&args.source, &args.output, &options)?;
    eprintln!(
        "{}: {} inodes, {} compressed files, {} bytes",
        args.output.display(),
        report.inode_count,
        report.compressed_files,
        report.image_bytes
    );
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mkfs-erofs: {error:#}");
            ExitCode::FAILURE
        }
    }
}
