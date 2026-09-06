use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::*;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(any(unix, windows)))]
compile_error!("extract-erofs currently supports Unix and Windows hosts");

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessDefaults {
    pub umask: u32,
    pub superuser: bool,
}

pub(crate) mod errno {
    #[cfg(unix)]
    pub const EUCLEAN: i32 = libc::EUCLEAN;
    #[cfg(windows)]
    pub const EUCLEAN: i32 = 117;
}

pub(crate) fn join_host_path(base: &str, child: &str) -> String {
    Path::new(base).join(child).to_string_lossy().into_owned()
}

pub(crate) fn validate_image_component(component: &[u8]) -> Result<&str> {
    let component = std::str::from_utf8(component).map_err(|_| Error::efscorrupted())?;
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.contains(['/', '\\', '\0', ':'])
        || !matches!(
            Path::new(component).components().next(),
            Some(Component::Normal(_))
        )
        || Path::new(component).components().count() != 1
    {
        return Err(Error::efscorrupted());
    }
    Ok(component)
}

pub(crate) fn join_image_path(root: &str, image_path: &str) -> Result<String> {
    if !image_path.starts_with('/') {
        return Err(Error::efscorrupted());
    }
    let mut path = PathBuf::from(root);
    for component in image_path.split('/').filter(|part| !part.is_empty()) {
        path.push(validate_image_component(component.as_bytes())?);
    }
    Ok(path.to_string_lossy().into_owned())
}

pub(crate) fn symlink_target_is_dir(root: &str, link_path: &str, target: &str) -> bool {
    let resolved = if target.starts_with('/') {
        join_image_path(root, target).map(PathBuf::from)
    } else {
        Ok(Path::new(link_path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(target))
    };
    resolved.is_ok_and(|path| path.is_dir())
}
