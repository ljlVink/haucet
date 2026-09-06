use clap::Parser;
use std::path::Path;

pub const RET_EXTRACT_DONE: i32 = 0;
pub const RET_EXTRACT_CONFIG_DONE: i32 = 1;
pub const RET_EXTRACT_CONFIG_FAIL: i32 = 2;
pub const RET_EXTRACT_INIT_FAIL: i32 = 3;
pub const RET_EXTRACT_INIT_NODE_FAIL: i32 = 4;
pub const RET_EXTRACT_OUTDIR_ROOT: i32 = 5;
pub const RET_EXTRACT_OPEN_FILE: i32 = 6;
pub const RET_EXTRACT_CREATE_DIR_FAIL: i32 = 7;
pub const RET_EXTRACT_CREATE_FILE_FAIL: i32 = 8;
pub const RET_EXTRACT_THREAD_NUM_ERROR: i32 = 9;
pub const RET_EXTRACT_FAIL_SKIP: i32 = 10;
pub const RET_EXTRACT_FAIL_EXIT: i32 = 11;

pub struct Config {
    pub image_path: String,
    pub image_base_name: String,
    pub offset: u64,
    pub out_dir: String,
    pub config_dir: String,
    pub target_path: String,
    pub targets: Vec<String>,
    pub target_config_path: String,

    pub umask: u32,
    pub superuser: bool,
    pub preserve_owner: bool,
    pub preserve_perms: bool,
    pub verify_xattr_digests: bool,
    pub digest_xattr_name: String,

    pub is_print_all: bool,
    pub is_print_target: bool,
    pub is_extract_all: bool,
    pub is_extract_target: bool,
    pub is_extract_config: bool,
    pub is_extract_target_config: bool,
    pub target_recursive: bool,
    pub overwrite: bool,
    pub check_decomp: bool,
    pub is_silent: bool,
    pub thread_num: u32,
    pub hardware_concurrency: u32,
    pub limit_hardware_concurrency: u32,
}

impl Config {
    pub fn new(hw: u32) -> Config {
        let defaults = crate::platform::process_defaults();
        Config {
            image_path: String::new(),
            image_base_name: String::new(),
            offset: 0,
            out_dir: String::new(),
            config_dir: String::new(),
            target_path: String::new(),
            targets: Vec::new(),
            target_config_path: String::new(),

            umask: defaults.umask,
            superuser: defaults.superuser,
            preserve_owner: defaults.superuser,
            preserve_perms: defaults.superuser,
            verify_xattr_digests: false,
            digest_xattr_name: String::new(),

            is_print_all: false,
            is_print_target: false,
            is_extract_all: false,
            is_extract_target: false,
            is_extract_config: false,
            is_extract_target_config: false,
            target_recursive: false,
            overwrite: false,
            check_decomp: false,
            is_silent: false,
            thread_num: 0,
            hardware_concurrency: hw,
            limit_hardware_concurrency: hw * 3,
        }
    }

    pub fn set_image_path(&mut self, path: &str) {
        self.image_path = path.trim().to_string();
    }

    pub fn set_out_dir(&mut self, path: &str) {
        self.out_dir = path.trim().to_string();
        if !self.image_path.is_empty() {
            let ps = self.image_path.rfind(['/', '\\']);
            let base = match ps {
                Some(i) => &self.image_path[i + 1..],
                None => self.image_path.as_str(),
            };
            self.image_base_name = match base.find('.') {
                Some(i) => base[..i].to_string(),
                None => base.to_string(),
            };
        }
    }

    pub fn set_targets(&mut self, path: &str) {
        self.target_path = path.trim().to_string();
        self.targets = split_string(&self.target_path, ',');
    }

    pub fn set_target_config_path(&mut self, path: &str) {
        self.target_config_path = path.trim().to_string();
    }

    pub fn init_out_dir(&mut self) -> i32 {
        if self.out_dir.is_empty() {
            self.out_dir = format!("./{}", self.image_base_name);
            self.config_dir = "./config".to_string();
        } else {
            while self.out_dir.len() > 1
                && !crate::platform::is_root_path(&self.out_dir)
                && (self.out_dir.ends_with('/') || self.out_dir.ends_with('\\'))
            {
                self.out_dir.pop();
            }
            if self.out_dir.len() >= crate::node::PATH_MAX {
                crate::log::loge("outDir directory name too long!");
                return RET_EXTRACT_OUTDIR_ROOT;
            }
            if crate::platform::is_root_path(&self.out_dir) {
                crate::log::loge(&format!("Not allow extracting to root: '{}'", self.out_dir));
                return RET_EXTRACT_OUTDIR_ROOT;
            }
            self.config_dir = crate::platform::join_host_path(&self.out_dir, "config");
            self.out_dir = crate::platform::join_host_path(&self.out_dir, &self.image_base_name);
        }
        RET_EXTRACT_DONE
    }

    fn create_dir(&self, tag: &str, path: &str) -> i32 {
        if std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) {
            return RET_EXTRACT_DONE;
        }
        match crate::extract::mkdirs(path, 0o755) {
            Ok(()) => RET_EXTRACT_DONE,
            Err(e) => {
                crate::log::loge(&format!("create {} dir fail: '{}'({})", tag, path, e));
                RET_EXTRACT_CREATE_DIR_FAIL
            }
        }
    }

    pub fn create_extract_config_dir(&self) -> i32 {
        self.create_dir("config", &self.config_dir)
    }

    pub fn create_extract_out_dir(&self) -> i32 {
        self.create_dir("out", &self.out_dir)
    }
}

fn split_string(s: &str, delimiter: char) -> Vec<String> {
    s.split(delimiter)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

#[derive(Parser, Debug)]
#[command(
    name = "extract.erofs",
    disable_help_flag = true,
    disable_version_flag = true,
    trailing_var_arg = false
)]
pub struct Args {
    #[arg(short = 'h', long = "help", action = clap::ArgAction::SetTrue)]
    pub help: bool,

    #[arg(short = 'V', long = "version", action = clap::ArgAction::SetTrue)]
    pub version: bool,

    #[arg(short = 'i', long = "image", value_name = "FILE")]
    pub image: Option<String>,

    #[arg(long = "offset", value_name = "#")]
    pub offset: Option<String>,

    #[arg(long = "xattr-inode-digest", action = clap::ArgAction::SetTrue)]
    pub xattr_inode_digest: bool,

    #[arg(short = 'o', long = "outdir", value_name = "X")]
    pub outdir: Option<String>,

    #[arg(short = 'P', long = "print", value_name = "X")]
    pub print: Option<String>,

    #[arg(short = 'f', long = "overwrite", action = clap::ArgAction::SetTrue)]
    pub overwrite: bool,

    #[arg(short = 'X', long = "extract", value_name = "X")]
    pub extract: Option<String>,

    #[arg(short = 'c', long = "config", value_name = "FILE")]
    pub config: Option<String>,

    #[arg(long = "only-cfg", action = clap::ArgAction::SetTrue)]
    pub only_cfg: bool,

    #[arg(short = 'p', action = clap::ArgAction::SetTrue)]
    pub p: bool,

    #[arg(short = 'x', action = clap::ArgAction::SetTrue)]
    pub x: bool,

    #[arg(short = 's', action = clap::ArgAction::SetTrue)]
    pub s: bool,

    #[arg(short = 'r', action = clap::ArgAction::SetTrue)]
    pub r: bool,

    #[arg(short = 'T', value_name = "#")]
    pub threads: Option<String>,
}

pub fn print_version() {
    let compressors = "lz4, lz4hc, zstd";
    println!(
        "  {}erofs-utils:{}            {}n/a (rust port){}",
        crate::log::BROWN,
        crate::log::RESET,
        crate::log::RED2,
        crate::log::RESET
    );
    println!(
        "  {}extract.erofs:{}          {}{}{}",
        crate::log::BROWN,
        crate::log::RESET,
        crate::log::RED2,
        crate::VERSION,
        crate::log::RESET
    );
    println!(
        "  {}available compressors:{}  {}{}{}",
        crate::log::BROWN,
        crate::log::RESET,
        crate::log::RED2,
        compressors,
        crate::log::RESET
    );
    println!(
        "  {}extract author:{}         {}rust port{}",
        crate::log::BROWN,
        crate::log::RESET,
        crate::log::RED2,
        crate::log::RESET
    );
}

pub fn usage(hw: u32, limit: u32) {
    let g = crate::log::GREEN2_BOLD;
    let b = crate::log::BROWN;
    let r = crate::log::RESET;
    println!(
        "{b}usage: [options]{r}\n\
         \x20\x20{g}-h, --help{b}              {b}Display this help and exit{r}\n\
         \x20\x20{g}-i, --image=[FILE]{b}      {b}Image file{r}\n\
         \x20\x20{g}--offset=#{b}              {b}skip # bytes at the beginning of IMAGE{r}\n\
         \x20\x20{g}--xattr-inode-digest{b}    {b}verify per-inode digests recorded as extended attributes{r}\n\
         \x20\x20{g}-p{b}                      {b}Print all entrys{r}\n\
         \x20\x20{g}-P, --print=X{b}           {b}Print the target of path X{r}\n\
         \x20\x20{g}-x{b}                      {b}Extract all items{r}\n\
         \x20\x20{g}-X, --extract=X{b}         {b}Extract the target of path X{r}\n\
         \x20\x20{g}-c, --config=[FILE]{b}     {b}Target of config{r}\n\
         \x20\x20{g}-r{b}                      {b}When using config, recurse directories{r}\n\
         \x20\x20{g}-s{b}                      {b}Silent mode, Don't show progress{r}\n\
         \x20\x20{g}-f, --overwrite{b}         {b}[{g}default: skip{b}] overwrite files that already exist{r}\n\
         \x20\x20{g}-T#{b}                     {b}[{g}1-{limit}{b}] Use # threads, default: -T0, is {g}{hw}{r}\n\
         \x20\x20{g}--only-cfg{b}              {b}Only extract fs_config|file_contexts|fs_options{r}\n\
         \x20\x20{g}-o, --outdir=X{b}          {b}Output dir{r}\n\
         \x20\x20{g}-V, --version{b}           {b}Print the version info{r}\n",
        g = g,
        b = b,
        r = r,
        limit = limit,
        hw = hw,
    );
}

pub fn parse_c_ull(s: &str) -> Option<u64> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let (radix, digits) = if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
    {
        (16, rest)
    } else if s.len() > 1 && s.starts_with('0') {
        (8, &s[1..])
    } else {
        (10, s)
    };
    if digits.is_empty() {
        return None;
    }
    let v = u64::from_str_radix(digits, radix).ok()?;
    Some(v)
}

pub fn parse_extract_config(args: &Args, eo: &mut Config) -> i32 {
    let mut ret = RET_EXTRACT_CONFIG_FAIL;
    let mut enter_check_opt = false;

    if args.help {
        usage(eo.hardware_concurrency, eo.limit_hardware_concurrency);
        return ret;
    }
    if args.version {
        print_version();
        return ret;
    }

    if let Some(img) = &args.image {
        enter_check_opt = true;
        eo.set_image_path(img);
    }
    if let Some(od) = &args.outdir {
        enter_check_opt = true;
        eo.set_out_dir(od);
    }
    if args.p {
        enter_check_opt = true;
        eo.is_print_all = true;
    }
    if let Some(p) = &args.print {
        enter_check_opt = true;
        eo.is_print_target = true;
        eo.set_targets(p);
    }
    if args.overwrite {
        enter_check_opt = true;
        eo.overwrite = true;
    }
    if args.x {
        enter_check_opt = true;
        eo.check_decomp = true;
        eo.is_extract_all = true;
    }
    if let Some(x) = &args.extract {
        enter_check_opt = true;
        eo.check_decomp = true;
        eo.is_extract_target = true;
        eo.set_targets(x);
    }
    if let Some(c) = &args.config {
        enter_check_opt = true;
        eo.is_extract_target_config = true;
        eo.set_target_config_path(c);
    }
    if args.s {
        enter_check_opt = true;
        eo.is_silent = true;
    }
    if args.r {
        enter_check_opt = true;
        eo.target_recursive = true;
    }
    if let Some(t) = &args.threads {
        enter_check_opt = true;
        if let Some(n) = parse_c_ull(t) {
            eo.thread_num = n as u32;
        }
    }
    if args.only_cfg {
        enter_check_opt = true;
        eo.is_extract_config = true;
    }
    if let Some(off) = &args.offset {
        enter_check_opt = true;
        if let Some(n) = parse_c_ull(off) {
            eo.offset = n;
        }
    }
    if args.xattr_inode_digest {
        enter_check_opt = true;
        eo.digest_xattr_name = String::new();
        eo.verify_xattr_digests = true;
        eo.check_decomp = true;
    }

    if enter_check_opt {
        if eo.image_path.is_empty() {
            ret = RET_EXTRACT_OPEN_FILE;
            return ret;
        }

        ret = eo.init_out_dir();
        if ret != RET_EXTRACT_DONE {
            return ret;
        }

        if eo.thread_num > eo.limit_hardware_concurrency {
            ret = RET_EXTRACT_THREAD_NUM_ERROR;
            crate::log::loge(&format!(
                "Threads min: 1 , max: {}",
                eo.limit_hardware_concurrency
            ));
            return ret;
        }
        if eo.thread_num == 0 {
            eo.thread_num = eo.hardware_concurrency;
        }
        ret = RET_EXTRACT_CONFIG_DONE;
    } else {
        usage(eo.hardware_concurrency, eo.limit_hardware_concurrency);
    }
    ret
}

pub fn file_name_no_ext(path: &str) -> String {
    let base = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match base.find('.') {
        Some(i) => base[..i].to_string(),
        None => base,
    }
}
