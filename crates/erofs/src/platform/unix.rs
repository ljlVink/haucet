use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};

use super::ProcessDefaults;

fn cstring(path: &str) -> io::Result<CString> {
    CString::new(path.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn cvt(ret: libc::c_int) -> io::Result<()> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn process_defaults() -> ProcessDefaults {
    let superuser = unsafe { libc::geteuid() } == 0;
    // libc::umask changes a process-global setting, so reading it would make
    // an embedded extraction change the host application's behavior.
    ProcessDefaults {
        umask: 0o022,
        superuser,
    }
}

pub(crate) fn io_error_code(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EIO)
}

pub(crate) fn error_message(code: i32) -> String {
    unsafe { CStr::from_ptr(libc::strerror(code)) }
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    file.read_at(buffer, offset)
}

pub(crate) fn create_dirs(path: &str, mode: u32) -> io::Result<()> {
    fn create_one(path: &str, mode: u32) -> io::Result<()> {
        let path = cstring(path)?;
        let ret = unsafe { libc::mkdir(path.as_ptr(), mode as libc::mode_t) };
        cvt(ret)
    }

    let bytes = path.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'/' {
            let parent = &path[..i];
            if std::fs::metadata(parent).is_err() {
                create_one(parent, mode)?;
            }
        }
    }
    if !path.is_empty() && std::fs::metadata(path).is_err() {
        create_one(path, mode)?;
    }
    Ok(())
}

pub(crate) fn open_output_file(path: &str, overwrite: bool) -> io::Result<File> {
    let mut flags = libc::O_WRONLY | libc::O_CREAT | libc::O_NOFOLLOW;
    flags |= if overwrite {
        libc::O_TRUNC
    } else {
        libc::O_EXCL
    };
    let path = cstring(path)?;
    let fd = unsafe { libc::open(path.as_ptr(), flags, 0o700) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(crate) fn open_truncate(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)
}

pub(crate) fn remove_file(path: &str) -> io::Result<()> {
    let path = cstring(path)?;
    cvt(unsafe { libc::unlink(path.as_ptr()) })
}

pub(crate) fn remove_dir(path: &str) -> io::Result<()> {
    let path = cstring(path)?;
    cvt(unsafe { libc::rmdir(path.as_ptr()) })
}

pub(crate) fn set_mode(path: &str, mode: u32) -> io::Result<()> {
    let path = cstring(path)?;
    cvt(unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) })
}

pub(crate) fn create_symlink(
    target: &str,
    path: &str,
    _target_is_dir: bool,
    _extraction_root: &str,
) -> io::Result<()> {
    let target = cstring(target)?;
    let path = cstring(path)?;
    cvt(unsafe { libc::symlink(target.as_ptr(), path.as_ptr()) })
}

pub(crate) fn create_hard_link(source: &str, target: &str) -> io::Result<()> {
    let source = cstring(source)?;
    let target = cstring(target)?;
    cvt(unsafe { libc::link(source.as_ptr(), target.as_ptr()) })
}

pub(crate) fn create_special(path: &str, mode: u32, device: u32) -> io::Result<()> {
    let path = cstring(path)?;
    cvt(unsafe { libc::mknod(path.as_ptr(), mode as libc::mode_t, device as libc::dev_t) })
}

pub(crate) fn set_times(
    path: &str,
    seconds: u64,
    nanoseconds: u32,
    no_follow: bool,
) -> io::Result<()> {
    let times = [
        libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: nanoseconds as libc::c_long,
        },
        libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: nanoseconds as libc::c_long,
        },
    ];
    let flags = if no_follow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    let path = cstring(path)?;
    cvt(unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), flags) })
}

pub(crate) fn set_owner(path: &str, uid: u32, gid: u32) -> io::Result<()> {
    let path = cstring(path)?;
    cvt(unsafe { libc::lchown(path.as_ptr(), uid, gid) })
}

pub(crate) fn format_local_time(timestamp: i64) -> String {
    unsafe {
        let value = timestamp as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&value, &mut tm).is_null() {
            return String::new();
        }
        let mut buffer = [0i8; 128];
        let format = b"%a %b %e %H:%M:%S %Y\0";
        libc::strftime(
            buffer.as_mut_ptr(),
            buffer.len(),
            format.as_ptr() as *const i8,
            &tm,
        );
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .trim()
            .to_string()
    }
}

pub(crate) fn is_root_path(path: &str) -> bool {
    !path.is_empty() && path.bytes().all(|byte| byte == b'/')
}
