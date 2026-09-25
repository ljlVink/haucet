use anyhow::{Context, Result, ensure};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;

pub fn prepare_dir(dir: &Path, what: &str, force: bool) -> Result<()> {
    if dir.exists() {
        let mut entries = fs::read_dir(dir)?;
        if entries.next().is_some() {
            ensure!(force, "{what} is not empty: {}", dir.display());
            fs::remove_dir_all(dir)
                .with_context(|| format!("removing old {what} {}", dir.display()))?;
        }
    }
    fs::create_dir_all(dir)?;
    Ok(())
}

pub fn prepare_dir_excluding(
    dir: &Path,
    what: &str,
    force: bool,
    protected_paths: &[&Path],
) -> Result<()> {
    for protected in protected_paths {
        ensure_output_does_not_contain(protected, dir)?;
    }
    prepare_dir(dir, what, force)
}

pub fn ensure_output_does_not_contain(input: &Path, output: &Path) -> Result<()> {
    let resolved_input = canonical_path(input)?;
    let resolved_output = resolve_path_allow_missing(output)?;
    ensure!(
        !resolved_input.starts_with(&resolved_output),
        "refusing to use output directory {} because it contains the input file {}",
        output.display(),
        input.display()
    );
    Ok(())
}

pub fn canonical_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("resolving {}", path.display()))
}

pub fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn resolve_path_allow_missing(path: &Path) -> Result<PathBuf> {
    let absolute = absolute_path(path)?;
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                resolved.push(component.as_os_str());
                if matches!(component, Component::Normal(_))
                    && resolved
                        .try_exists()
                        .with_context(|| format!("checking path {}", resolved.display()))?
                {
                    resolved = canonical_path(&resolved)?;
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(
                    matches!(
                        resolved.components().next_back(),
                        Some(Component::Normal(_))
                    ),
                    "path escapes the filesystem root: {}",
                    path.display()
                );
                resolved.pop();
            }
        }
    }
    Ok(resolved)
}

pub fn is_simple_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
        && Path::new(name).components().count() == 1
}

pub fn safe_join(base: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !path.is_absolute(),
        "manifest path must be relative: {relative:?}"
    );
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
        "unsafe manifest path {relative:?}"
    );
    Ok(base.join(path))
}

pub fn sibling_temporary(output: &Path, label: &str) -> Result<PathBuf> {
    let name = output
        .file_name()
        .and_then(OsStr::to_str)
        .context("output filename is not UTF-8")?;
    Ok(output.with_file_name(format!(".{name}.{label}.part")))
}

pub fn atomic_write<F>(final_path: &Path, label: &str, write: F) -> Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<()>,
{
    let temporary = sibling_temporary(final_path, label)?;
    ensure!(
        !temporary.exists(),
        "temporary output already exists: {}",
        temporary.display()
    );
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let result = (|| -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        let mut writer = BufWriter::with_capacity(IO_BUFFER_SIZE, file);
        write(&mut writer)?;
        writer.flush()?;
        fs::rename(&temporary, final_path).with_context(|| {
            format!("moving {} to {}", temporary.display(), final_path.display())
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
pub fn set_unix_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn set_unix_mode(_path: &Path, _mode: Option<u32>) -> Result<()> {
    Ok(())
}

pub fn create_backup(
    path: &Path,
    contents: &[u8],
    source_permissions: fs::Permissions,
    label: &str,
) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{label} path has no file name"))?
        .to_string_lossy();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?;
    let mut suffix = 0_u32;
    loop {
        let suffix_text = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let backup = parent.join(format!(
            "{name}.bak_{}_{}{suffix_text}",
            now.as_secs(),
            now.subsec_nanos()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut backup_file = match options.open(&backup) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix = suffix
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("too many {label} backup name collisions"))?;
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "creating {label} backup {} from {}",
                        backup.display(),
                        path.display()
                    )
                });
            }
        };
        let write_result = (|| -> Result<()> {
            backup_file.write_all(contents)?;
            fs::set_permissions(&backup, source_permissions.clone())?;
            backup_file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            drop(backup_file);
            let _ = fs::remove_file(&backup);
            return Err(error)
                .with_context(|| format!("writing {label} backup {}", backup.display()));
        }
        return Ok(backup);
    }
}
