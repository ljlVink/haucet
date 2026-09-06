use std::fs::{File, FileTimes, OpenOptions};
use std::io;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::{Component, Path};
use std::time::{Duration, UNIX_EPOCH};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
};

use super::ProcessDefaults;

pub(crate) fn process_defaults() -> ProcessDefaults {
    ProcessDefaults {
        umask: 0o022,
        superuser: false,
    }
}

pub(crate) fn io_error_code(error: &io::Error) -> i32 {
    match error.kind() {
        io::ErrorKind::NotFound => libc::ENOENT,
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => libc::EACCES,
        io::ErrorKind::AlreadyExists => libc::EEXIST,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => libc::EINVAL,
        io::ErrorKind::OutOfMemory => libc::ENOMEM,
        io::ErrorKind::NotADirectory => libc::ENOTDIR,
        io::ErrorKind::IsADirectory => libc::EISDIR,
        io::ErrorKind::Unsupported => libc::EOPNOTSUPP,
        _ => libc::EIO,
    }
}

pub(crate) fn error_message(code: i32) -> String {
    match code {
        libc::ENOENT => "No such file or directory".to_string(),
        libc::EIO => "I/O error".to_string(),
        libc::ENOMEM => "Not enough memory".to_string(),
        libc::EACCES => "Permission denied".to_string(),
        libc::EEXIST => "File exists".to_string(),
        libc::ENOTDIR => "Not a directory".to_string(),
        libc::EISDIR => "Is a directory".to_string(),
        libc::EINVAL => "Invalid argument".to_string(),
        libc::ENODATA => "No data available".to_string(),
        libc::EOPNOTSUPP => "Operation not supported".to_string(),
        super::errno::EUCLEAN => "Filesystem metadata is corrupted".to_string(),
        _ => format!("OS error {code}"),
    }
}

pub(crate) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    file.seek_read(buffer, offset)
}

pub(crate) fn create_dirs(path: &str, _mode: u32) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

pub(crate) fn open_output_file(path: &str, overwrite: bool) -> io::Result<File> {
    if overwrite && let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.is_dir() {
            return Err(io::Error::from(io::ErrorKind::IsADirectory));
        }
        if metadata.file_type().is_symlink() {
            std::fs::remove_file(path)?;
        }
    }

    let mut options = OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    options.open(path)
}

pub(crate) fn open_truncate(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

pub(crate) fn remove_file(path: &str) -> io::Result<()> {
    std::fs::remove_file(path)
}

pub(crate) fn remove_dir(path: &str) -> io::Result<()> {
    std::fs::remove_dir(path)
}

pub(crate) fn set_mode(path: &str, mode: u32) -> io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    std::fs::set_permissions(path, permissions)
}

pub(crate) fn create_symlink(
    target: &str,
    path: &str,
    target_is_dir: bool,
    extraction_root: &str,
) -> io::Result<()> {
    let host_target = if target.starts_with('/') {
        super::join_image_path(extraction_root, target).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe absolute symlink target",
            )
        })?
    } else {
        target.to_string()
    };
    if target_is_dir {
        std::os::windows::fs::symlink_dir(host_target, path)
    } else {
        std::os::windows::fs::symlink_file(host_target, path)
    }
}

pub(crate) fn create_hard_link(source: &str, target: &str) -> io::Result<()> {
    std::fs::hard_link(source, target)
}

pub(crate) fn create_special(_path: &str, _mode: u32, _device: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows cannot create Unix special files",
    ))
}

pub(crate) fn set_times(
    path: &str,
    seconds: u64,
    nanoseconds: u32,
    _no_follow: bool,
) -> io::Result<()> {
    if nanoseconds >= 1_000_000_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "timestamp nanoseconds are out of range",
        ));
    }
    let timestamp = UNIX_EPOCH
        .checked_add(Duration::new(seconds, nanoseconds))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "timestamp is out of range"))?;

    let file = OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    file.set_times(
        FileTimes::new()
            .set_accessed(timestamp)
            .set_modified(timestamp),
    )
}

pub(crate) fn set_owner(_path: &str, _uid: u32, _gid: u32) -> io::Result<()> {
    Ok(())
}

pub(crate) fn format_local_time(timestamp: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    unsafe {
        let value = timestamp as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_s(&mut tm, &value) != 0 {
            return String::new();
        }
        let Some(weekday) = WEEKDAYS.get(tm.tm_wday as usize) else {
            return String::new();
        };
        let Some(month) = MONTHS.get(tm.tm_mon as usize) else {
            return String::new();
        };
        format!(
            "{} {} {:>2} {:02}:{:02}:{:02} {}",
            weekday,
            month,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            tm.tm_year + 1900
        )
    }
}

pub(crate) fn is_root_path(path: &str) -> bool {
    let mut components = Path::new(path).components();
    match components.next() {
        Some(Component::Prefix(_)) => {
            matches!(components.next(), Some(Component::RootDir)) && components.next().is_none()
        }
        Some(Component::RootDir) => components.next().is_none(),
        _ => false,
    }
}
