// The upstream-compatible extractor retains format definitions and helpers for
// EROFS features that are parsed but not yet exposed by the public API.
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod data;
mod decompress;
mod dir;
#[allow(dead_code)]
mod erofs_fs;
mod error;
#[allow(dead_code)]
mod extract;
mod fragments;
mod inode;
#[allow(dead_code)]
mod io;
mod log;
mod node;
#[allow(dead_code)]
mod platform;
#[allow(dead_code)]
mod sb;
#[allow(dead_code)]
mod xattr;
mod zmap;

use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clap::Parser;

use config::{Config, parse_extract_config};
use data::Sb;
use erofs_fs::*;
use inode::erofs_mode_to_ftype;
use node::ErofsNode;
use sb::SbInfo;

pub use config::Args as CliArgs;

pub const VERSION: &str = concat!("extract-erofs ", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractMode {
    #[default]
    Full,
    ConfigOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractOptions {
    pub mode: ExtractMode,
    pub threads: Option<usize>,
    pub overwrite: bool,
    pub offset: u64,
    pub verify_xattr_digests: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            mode: ExtractMode::Full,
            threads: None,
            overwrite: false,
            offset: 0,
            verify_xattr_digests: false,
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    InvalidOptions(String),
    Initialization(String),
    UnsupportedCompression(String),
    FileFailures {
        count: usize,
        exception_log: PathBuf,
    },
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(message) => write!(f, "invalid extraction options: {message}"),
            Self::Initialization(message) => {
                write!(f, "could not initialize EROFS extraction: {message}")
            }
            Self::UnsupportedCompression(algorithms) => write!(
                f,
                "unsupported EROFS compression algorithm(s): {algorithms}; only LZ4/LZ4HC and Zstd are supported"
            ),
            Self::FileFailures {
                count,
                exception_log,
            } => write!(
                f,
                "{count} filesystem item(s) could not be extracted; details are in {}",
                exception_log.display()
            ),
        }
    }
}

impl std::error::Error for ExtractError {}

pub fn extract(image: &Path, output: &Path, options: ExtractOptions) -> Result<(), ExtractError> {
    let image_path = image
        .to_str()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            ExtractError::InvalidOptions(format!("invalid image path {}", image.display()))
        })?;
    let output_path = output
        .to_str()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            ExtractError::InvalidOptions(format!("invalid output path {}", output.display()))
        })?;
    let hw = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let mut config = Config::new(hw);
    config.set_image_path(image_path);
    config.set_out_dir(output_path);
    config.offset = options.offset;
    config.overwrite = options.overwrite;
    config.verify_xattr_digests = options.verify_xattr_digests;
    config.check_decomp = true;
    config.is_extract_all = true;
    config.is_extract_config = options.mode == ExtractMode::ConfigOnly;
    if let Some(threads) = options.threads {
        config.thread_num = u32::try_from(threads)
            .map_err(|_| ExtractError::InvalidOptions("thread count is too large".to_owned()))?;
    }
    if config.thread_num > config.limit_hardware_concurrency {
        return Err(ExtractError::InvalidOptions(format!(
            "thread count must be between 1 and {}",
            config.limit_hardware_concurrency
        )));
    }
    if config.thread_num == 0 {
        config.thread_num = config.hardware_concurrency;
    }
    if config.init_out_dir() != config::RET_EXTRACT_DONE {
        return Err(ExtractError::InvalidOptions(format!(
            "refusing output directory {}",
            output.display()
        )));
    }
    run_extraction(config)
}

pub fn run_cli() -> Result<(), ExtractError> {
    let args =
        CliArgs::try_parse().map_err(|error| ExtractError::InvalidOptions(error.to_string()))?;
    run_cli_args(args)
}

pub fn run_cli_args(args: CliArgs) -> Result<(), ExtractError> {
    let hw = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    if args.help {
        config::usage(hw, hw * 3);
        return Ok(());
    }
    if args.version {
        config::print_version();
        return Ok(());
    }

    let mut config = Config::new(hw);
    let code = parse_extract_config(&args, &mut config);
    if code != config::RET_EXTRACT_CONFIG_DONE {
        return Err(ExtractError::InvalidOptions(format!(
            "command-line configuration failed (status {code})"
        )));
    }
    run_extraction(config)
}

fn uuid_unparse_lower(uuid: &[u8; 16]) -> String {
    let g = |a: usize, b: usize| ((uuid[a] as u32) << 8) | uuid[b] as u32;
    format!(
        "{:04x}{:04x}-{:04x}-{:04x}-{:04x}-{:04x}{:04x}{:04x}",
        g(0, 1),
        g(2, 3),
        g(4, 5),
        g(6, 7),
        g(8, 9),
        g(10, 11),
        g(12, 13),
        g(14, 15)
    )
}

fn ctime_trimmed(timestamp: i64) -> String {
    platform::format_local_time(timestamp)
}

fn print_initialized_node(nodes: &[ErofsNode]) {
    for n in nodes {
        println!(
            "Extract: type={:<7} dataLayout={:<19} {} {}",
            n.get_type_str(),
            n.get_data_layout_str(),
            n.fs_config,
            n.selinux_label
        );
    }
}

fn write_fs_config_and_selinux_label(
    config: &Config,
    nodes: &[ErofsNode],
    sbi: &SbInfo,
) -> Result<(), ExtractError> {
    let config_path = platform::join_host_path(&config.config_dir, &config.image_base_name);
    let fs_config_path = format!("{}_fs_config", config_path);
    let selinux_labels_path = format!("{}_file_contexts", config_path);

    log::logi(&format!(
        "{}fs_config|file_contexts|fs_options  {}saving...{}",
        log::BROWN,
        log::GREEN2_BOLD,
        log::RESET
    ));

    let mut fsc = std::fs::File::create(&fs_config_path).map_err(|error| {
        ExtractError::Initialization(format!("creating {fs_config_path}: {error}"))
    })?;
    let mut sel = std::fs::File::create(&selinux_labels_path).map_err(|error| {
        ExtractError::Initialization(format!("creating {selinux_labels_path}: {error}"))
    })?;
    let mut is_root = true;
    for n in nodes {
        fsc.write_all(format!("{}\n", n.fs_config).as_bytes())
            .map_err(|error| {
                ExtractError::Initialization(format!("writing {fs_config_path}: {error}"))
            })?;
        if !n.selinux_label_config.is_empty() {
            sel.write_all(format!("{}\n", n.selinux_label_config).as_bytes())
                .map_err(|error| {
                    ExtractError::Initialization(format!("writing {selinux_labels_path}: {error}"))
                })?;
        }
        if is_root && n.path == "/" {
            is_root = false;
            for other in node::OTHER_PATHS_IN_ROOT_DIR {
                let fs_config = format!(
                    "{} {} {} {:04o}",
                    other,
                    n.inode.i_uid,
                    n.inode.i_gid,
                    n.inode.i_mode & 0o777
                );
                let selinux_label =
                    node::handle_special_symbols(&format!("{} {}", other, n.selinux_label));
                fsc.write_all(format!("{}\n", fs_config).as_bytes())
                    .map_err(|error| {
                        ExtractError::Initialization(format!("writing {fs_config_path}: {error}"))
                    })?;
                sel.write_all(format!("{}\n", selinux_label).as_bytes())
                    .map_err(|error| {
                        ExtractError::Initialization(format!(
                            "writing {selinux_labels_path}: {error}"
                        ))
                    })?;
            }
        }
    }
    if !config.is_extract_target && !config.is_extract_target_config {
        let mkfs_option_path = format!("{}_fs_options", config_path);
        let mut opt = std::fs::File::create(&mkfs_option_path).map_err(|error| {
            ExtractError::Initialization(format!("creating {mkfs_option_path}: {error}"))
        })?;
        let build_time = sbi.epoch + sbi.build_time as i64;
        let time_str = ctime_trimmed(build_time);
        let uuid = uuid_unparse_lower(&sbi.uuid);
        let is_big_pcluster = sbi.feature_incompat & EROFS_FEATURE_INCOMPAT_BIG_PCLUSTER != 0;
        opt.write_all(format!("Filesystem created:    {}\n", time_str).as_bytes())
            .map_err(|error| {
                ExtractError::Initialization(format!("writing {mkfs_option_path}: {error}"))
            })?;
        opt.write_all(format!("Filesystem UUID:       {}\n", uuid).as_bytes())
            .map_err(|error| {
                ExtractError::Initialization(format!("writing {mkfs_option_path}: {error}"))
            })?;
        let has_ishare = sbi.feature_compat & EROFS_FEATURE_COMPAT_ISHARE_XATTRS != 0;
        opt.write_all(
            format!(
                "mkfs.erofs options:    -zlz4hc {}-T {} -U {} {}--fs-config-file={} --file-contexts={} {}_repack.img {}\n",
                if is_big_pcluster { "-C 16384 " } else { "" },
                build_time,
                uuid,
                if has_ishare { "--xattr-inode-digest " } else { "" },
                fs_config_path,
                selinux_labels_path,
                config.image_base_name,
                config.out_dir
            )
            .as_bytes(),
        )
        .map_err(|error| ExtractError::Initialization(format!("writing {mkfs_option_path}: {error}")))?;
    }
    log::logi(&format!(
        "{}fs_config|file_contexts|fs_options  {}done.{}",
        log::BROWN,
        log::GREEN2_BOLD,
        log::RESET
    ));
    Ok(())
}

fn print_operation_time(start: Instant) {
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{}{}The operation took: {}{}{:.3}{}{} second(s).{}",
        log::TAG_INFO,
        log::GREEN2_BOLD,
        log::RESET,
        log::RED2,
        secs,
        log::RESET,
        log::GREEN2_BOLD,
        log::RESET
    );
}

fn extract_task(
    config: &Config,
    node: &mut ErofsNode,
    hardlinks: &Mutex<HashMap<u64, String>>,
    progress: &AtomicU64,
    exception_size: &AtomicU64,
) {
    let err = extract::write_to_file(config, node, hardlinks);
    node.inode.clear_metadata_cache();
    if node.init_exception_info(err) {
        exception_size.fetch_add(1, Ordering::Relaxed);
    }
    progress.fetch_add(1, Ordering::Relaxed);
}

fn progress_mt(total: u64, counter: &AtomicU64) {
    let mut prev = 0.0f32;
    loop {
        let cur = counter.load(Ordering::Relaxed);
        let pct = cur as f32 / total as f32 * 100.0;
        if pct > prev {
            print!(
                "{}{}[ {}{}{:.2}%{}{} ]{}\r",
                log::TAG_INFO,
                log::GREEN2_BOLD,
                log::RESET,
                log::RED2,
                pct,
                log::RESET,
                log::GREEN2_BOLD,
                log::RESET
            );
            let _ = std::io::stdout().flush();
            if pct == 100.0 {
                println!();
            }
            prev = pct;
        }
        if cur >= total {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn erofs_node_classification(
    nodes: &[ErofsNode],
    hardlinks: &Mutex<HashMap<u64, String>>,
) -> (Vec<usize>, Vec<usize>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if erofs_mode_to_ftype(n.inode.i_mode) == EROFS_FT_DIR {
            dirs.push(i);
        } else {
            if n.get_nlink() > 1 {
                let mut guard = hardlinks.lock().unwrap();
                guard.entry(n.get_nid()).or_insert_with(|| n.path.clone());
            }
            files.push(i);
        }
    }
    (dirs, files)
}

fn run_extraction(mut eo: Config) -> Result<(), ExtractError> {
    let start = Instant::now();

    let dev = match io::Device::open(&eo.image_path, eo.offset) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("<E> erofs_io: failed to open {}: {}", eo.image_path, e);
            log::loge(&format!("failed to open '{}'", eo.image_path));
            log::loge("Failed to initialize erofs info");
            return Err(ExtractError::Initialization(format!(
                "failed to open {}: {e}",
                eo.image_path
            )));
        }
    };
    let sbi: Sb = match sb::erofs_read_superblock(dev) {
        Ok(s) => Arc::new(s),
        Err(error) => {
            log::loge("failed to read superblock");
            log::loge("Failed to initialize erofs info");
            return Err(ExtractError::Initialization(format!(
                "failed to read EROFS superblock: {error}"
            )));
        }
    };

    let mut unsupported = Vec::new();
    if sbi.available_compr_algs & (1 << Z_EROFS_COMPRESSION_LZMA) != 0 {
        unsupported.push("LZMA");
    }
    if sbi.available_compr_algs & (1 << Z_EROFS_COMPRESSION_DEFLATE) != 0 {
        unsupported.push("DEFLATE");
    }
    if !unsupported.is_empty() {
        return Err(ExtractError::UnsupportedCompression(unsupported.join(", ")));
    }

    let ishare = fragments::erofs_xattr_get_ishare_prefix(&sbi);
    if eo.verify_xattr_digests {
        match ishare {
            Some(p) => eo.digest_xattr_name = p,
            None => {
                log::loge(
                    "image has no inode digest xattrs (was --xattr-inode-digest used during mkfs?)",
                );
                return Err(ExtractError::Initialization(
                    "image has no inode digest xattrs (was --xattr-inode-digest used during mkfs?)"
                        .to_owned(),
                ));
            }
        }
    }

    let mut nodes: Vec<ErofsNode> = Vec::new();
    let ok = if eo.is_print_target || eo.is_extract_target || eo.is_extract_target_config {
        if eo.is_extract_target_config {
            let mut targets = Vec::new();
            if let Ok(f) = std::fs::File::open(&eo.target_config_path) {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    targets.push(line);
                }
            }
            if !targets.is_empty() {
                node::init_erofs_node_by_targets(&mut nodes, &sbi, &targets, eo.target_recursive)
            } else {
                log::loge(&format!(
                    "target config '{}' does not exist! ",
                    eo.target_config_path
                ));
                node::init_erofs_node_by_targets(&mut nodes, &sbi, &eo.targets, eo.target_recursive)
            }
        } else {
            node::init_erofs_node_by_targets(&mut nodes, &sbi, &eo.targets, eo.target_recursive)
        }
    } else if eo.is_print_all || eo.is_extract_all {
        node::init_erofs_node_by_root(&mut nodes, &sbi).is_ok() && !nodes.is_empty()
    } else {
        false
    };

    if !ok {
        return Err(ExtractError::Initialization(
            "could not initialize filesystem nodes".to_owned(),
        ));
    }

    if eo.is_print_target || eo.is_print_all {
        print_initialized_node(&nodes);
        return Ok(());
    }

    log::logi(&format!("{}Starting...{}", log::GREEN2_BOLD, log::RESET));

    if (eo.is_extract_target || eo.is_extract_all) && eo.is_extract_config {
        if eo.create_extract_config_dir() != 0 {
            return Err(ExtractError::Initialization(
                "could not create configuration output directory".to_owned(),
            ));
        }
        write_fs_config_and_selinux_label(&eo, &nodes, &sbi)?;
        print_operation_time(start);
        return Ok(());
    }

    if eo.is_extract_target || eo.is_extract_all {
        let err = eo.create_extract_config_dir() & eo.create_extract_out_dir();
        if err != 0 {
            return Err(ExtractError::Initialization(
                "could not create extraction output directories".to_owned(),
            ));
        }
        write_fs_config_and_selinux_label(&eo, &nodes, &sbi)?;

        let hardlinks: Arc<Mutex<HashMap<u64, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let (dir_nodes, other_nodes) = erofs_node_classification(&nodes, &hardlinks);

        let progress = Arc::new(AtomicU64::new(0));
        let exception_size = Arc::new(AtomicU64::new(0));
        let total = nodes.len() as u64;

        let nodes: Vec<Mutex<ErofsNode>> = nodes.into_iter().map(Mutex::new).collect();
        let nodes = Arc::new(nodes);

        if eo.thread_num == 1 {
            let progress_thread = {
                let progress = Arc::clone(&progress);
                std::thread::spawn(move || progress_mt(total, &progress))
            };
            for &i in &dir_nodes {
                let mut guard = nodes[i].lock().unwrap();
                extract_task(&eo, &mut guard, &hardlinks, &progress, &exception_size);
            }
            for &i in &other_nodes {
                let mut guard = nodes[i].lock().unwrap();
                extract_task(&eo, &mut guard, &hardlinks, &progress, &exception_size);
            }
            let _ = progress_thread.join();
        } else {
            log::logi(&format!(
                "{}Using {}{}{}{} threads{}",
                log::GREEN2_BOLD,
                log::RESET,
                log::RED2,
                eo.thread_num,
                log::GREEN2_BOLD,
                log::RESET
            ));
            let progress_thread = {
                let progress = Arc::clone(&progress);
                std::thread::spawn(move || progress_mt(total, &progress))
            };
            for &i in &dir_nodes {
                let mut guard = nodes[i].lock().unwrap();
                extract_task(&eo, &mut guard, &hardlinks, &progress, &exception_size);
            }

            let thread_num = eo.thread_num as usize;
            let eo_ref = &eo;
            std::thread::scope(|scope| {
                let queue = Arc::new(Mutex::new(0usize));
                let other_nodes = Arc::new(other_nodes);
                let mut workers = Vec::with_capacity(thread_num);
                for _ in 0..thread_num {
                    let queue = Arc::clone(&queue);
                    let nodes = Arc::clone(&nodes);
                    let other_nodes = Arc::clone(&other_nodes);
                    let hardlinks = Arc::clone(&hardlinks);
                    let progress = Arc::clone(&progress);
                    let exception_size = Arc::clone(&exception_size);
                    workers.push(scope.spawn(move || {
                        loop {
                            let idx = {
                                let mut q = queue.lock().unwrap();
                                if *q >= other_nodes.len() {
                                    break;
                                }
                                let v = *q;
                                *q += 1;
                                v
                            };
                            let mut guard = nodes[other_nodes[idx]].lock().unwrap();
                            extract_task(
                                eo_ref,
                                &mut guard,
                                &hardlinks,
                                &progress,
                                &exception_size,
                            );
                        }
                    }));
                }
                for w in workers {
                    let _ = w.join();
                }
            });
            let _ = progress_thread.join();
        }

        let failures = exception_size.load(Ordering::Relaxed);
        if failures > 0 {
            let log_path = platform::join_host_path(&eo.config_dir, "exception.log");
            let mut f = std::fs::File::create(&log_path).map_err(|error| {
                ExtractError::Initialization(format!("creating {log_path}: {error}"))
            })?;
            for n in nodes.iter() {
                let guard = n.lock().unwrap();
                if let Some(info) = &guard.exception_info {
                    f.write_all(format!("{}\n", info).as_bytes())
                        .map_err(|error| {
                            ExtractError::Initialization(format!("writing {log_path}: {error}"))
                        })?;
                }
            }
            log::loge(&format!(
                "{}An exception occurred while fetching, the info has been saved!{}",
                log::RED2,
                log::RESET
            ));
        }

        print_operation_time(start);
        if failures > 0 {
            return Err(ExtractError::FileFailures {
                count: failures as usize,
                exception_log: PathBuf::from(platform::join_host_path(
                    &eo.config_dir,
                    "exception.log",
                )),
            });
        }
        return Ok(());
    }

    print_operation_time(start);
    Ok(())
}
