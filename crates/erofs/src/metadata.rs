use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
#[cfg(not(unix))]
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::build_options::BuildOptions;
use crate::inode::{S_IFDIR, S_IFLNK, S_IFMT, S_IFREG};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryMetadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u64,
    pub mtime_nsec: u32,
    pub rdev: u32,
    pub original_nid: Option<u64>,
    pub symlink: Option<String>,
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

/// Metadata that cannot be represented by an extracted Windows source tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataManifest {
    pub version: u32,
    pub block_size: u32,
    pub uuid: [u8; 16],
    pub volume_label: String,
    pub build_time: u64,
    #[serde(default)]
    pub extraction_root: Option<PathBuf>,
    pub entries: BTreeMap<String, EntryMetadata>,
}

pub(crate) fn from_nodes(
    nodes: &[crate::node::ErofsNode],
    sbi: &crate::sb::SbInfo,
) -> Result<MetadataManifest> {
    let mut entries = BTreeMap::new();
    let mut label = [0; 16];
    sbi.dev
        .read_at(&mut label, crate::erofs_fs::EROFS_SUPER_OFFSET + 64)?;
    let label_end = label.iter().position(|&b| b == 0).unwrap_or(label.len());
    for node in nodes {
        let mut inode = node.inode.clone();
        let symlink = if inode.i_mode & S_IFMT == S_IFLNK {
            ensure!(inode.i_size <= 65535, "symlink target exceeds 65535 bytes");
            let mut target = vec![0; inode.i_size as usize];
            crate::data::inode_pread(&mut inode, &mut target, 0)?;
            Some(String::from_utf8(target).context("symlink target is not UTF-8")?)
        } else {
            None
        };
        entries.insert(
            node.path.clone(),
            EntryMetadata {
                mode: inode.i_mode,
                uid: inode.i_uid,
                gid: inode.i_gid,
                mtime: inode.i_mtime,
                mtime_nsec: inode.i_mtime_nsec,
                rdev: inode.i_rdev,
                original_nid: Some(inode.nid),
                symlink,
                xattrs: crate::xattr::read_all(&mut inode)
                    .with_context(|| format!("reading xattrs for {}", node.path))?,
            },
        );
    }
    Ok(MetadataManifest {
        version: 1,
        block_size: sbi.blksiz(),
        uuid: sbi.uuid,
        volume_label: String::from_utf8(label[..label_end].to_vec())
            .context("volume label is not UTF-8")?,
        build_time: u64::try_from(sbi.epoch)?
            .checked_add(sbi.build_time as u64)
            .context("build time overflow")?,
        extraction_root: None,
        entries,
    })
}

#[derive(Debug)]
struct FsConfig {
    uid: u32,
    gid: u32,
    mode: u32,
    capabilities: Option<u64>,
}

struct FileContext {
    regex: Regex,
    mode: Option<u32>,
    label: String,
}

pub(crate) struct MetadataResolver {
    options: BuildOptions,
    manifest: Option<MetadataManifest>,
    config: BTreeMap<String, FsConfig>,
    contexts: Vec<FileContext>,
}

impl MetadataResolver {
    pub fn load(options: &BuildOptions) -> Result<Self> {
        let manifest = options
            .metadata_file
            .as_ref()
            .map(|path| -> Result<MetadataManifest> {
                let manifest: MetadataManifest =
                    serde_json::from_reader(BufReader::new(File::open(path)?))
                        .with_context(|| format!("reading metadata {}", path.display()))?;
                ensure!(
                    manifest.version == 1,
                    "unsupported EROFS metadata version {}",
                    manifest.version
                );
                for (path, entry) in &manifest.entries {
                    ensure!(
                        path.starts_with('/')
                            && !path.contains('\0')
                            && !path.split('/').any(|part| matches!(part, "." | "..")),
                        "invalid metadata path {path:?}"
                    );
                    ensure!(
                        entry.mode <= u16::MAX as u32 && entry.mtime_nsec < 1_000_000_000,
                        "invalid inode metadata for {path}"
                    );
                }
                Ok(manifest)
            })
            .transpose()?;
        let mut config = BTreeMap::new();
        if let Some(path) = &options.fs_config {
            for (line_num, line) in fs::read_to_string(path)?.lines().enumerate() {
                let fields = shlex::split(line).with_context(|| {
                    format!("invalid fs_config quoting at line {}", line_num + 1)
                })?;
                if fields.is_empty() {
                    continue;
                }
                ensure!(fields.len() >= 4, "invalid fs_config line {}", line_num + 1);
                let mode = u32::from_str_radix(&fields[3], 8).context("invalid fs_config mode")?;
                ensure!(mode <= 0o7777, "fs_config permission bits exceed 07777");
                let mut capabilities = None;
                for field in &fields[4..] {
                    if let Some(value) = field.strip_prefix("capabilities=") {
                        capabilities = Some(
                            if let Some(hex) = value
                                .strip_prefix("0x")
                                .or_else(|| value.strip_prefix("0X"))
                            {
                                u64::from_str_radix(hex, 16)?
                            } else {
                                value.parse()?
                            },
                        );
                    } else {
                        bail!("unsupported fs_config field {field:?}");
                    }
                }
                config.insert(
                    normalize_path(&fields[0]),
                    FsConfig {
                        uid: fields[1].parse().context("invalid fs_config uid")?,
                        gid: fields[2].parse().context("invalid fs_config gid")?,
                        mode,
                        capabilities,
                    },
                );
            }
        }
        let mut contexts = Vec::new();
        if let Some(path) = &options.file_contexts {
            for (line_num, line) in fs::read_to_string(path)?.lines().enumerate() {
                let line = line.trim_matches('\0').trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                // Keep backslashes in SELinux path regexes; these are not shell words.
                let fields: Vec<_> = line.split_whitespace().collect();
                ensure!(
                    matches!(fields.len(), 2 | 3),
                    "invalid file_contexts line {}",
                    line_num + 1
                );
                let mode = if fields.len() == 3 {
                    Some(match fields[1] {
                        "--" => S_IFREG,
                        "-d" => S_IFDIR,
                        "-l" => S_IFLNK,
                        "-c" => crate::inode::S_IFCHR,
                        "-b" => crate::inode::S_IFBLK,
                        "-p" => crate::inode::S_IFIFO,
                        "-s" => crate::inode::S_IFSOCK,
                        other => bail!("unsupported file_contexts type {other:?}"),
                    })
                } else {
                    None
                };
                contexts.push(FileContext {
                    regex: Regex::new(&format!("^(?:{})$", fields[0])).with_context(|| {
                        format!("invalid file_contexts pattern at line {}", line_num + 1)
                    })?,
                    mode,
                    label: fields[fields.len() - 1].trim_end_matches('\0').to_owned(),
                });
            }
        }
        Ok(Self {
            options: options.clone(),
            manifest,
            config,
            contexts,
        })
    }

    pub fn resolve(
        &self,
        relative: &str,
        host: &Path,
        host_metadata: &fs::Metadata,
    ) -> Result<EntryMetadata> {
        let mut entry = host_entry(host_metadata)?;
        let original = self
            .manifest
            .as_ref()
            .and_then(|m| m.entries.get(relative))
            .filter(|e| e.mode & S_IFMT == entry.mode & S_IFMT);
        if let Some(original) = original {
            entry = original.clone();
        } else if !self.options.no_xattrs {
            entry.xattrs = host_xattrs(host)?;
        }
        if entry.mode & S_IFMT == S_IFLNK {
            entry.symlink = Some(self.symlink_target(relative, host, original)?);
        }
        let mounted = format!(
            "/{}/{}",
            self.options.mount_point,
            relative.trim_start_matches('/')
        );
        let mounted = normalize_path(&mounted);
        let config = self
            .config
            .get(&mounted)
            .or_else(|| self.config.get(&normalize_path(relative)));
        if let Some(config) = config {
            entry.uid = config.uid;
            entry.gid = config.gid;
            entry.mode = (entry.mode & S_IFMT) | config.mode;
            if let Some(caps) = config.capabilities {
                let existing = entry.xattrs.get("security.capability");
                if caps == 0 {
                    entry.xattrs.remove("security.capability");
                } else if existing.and_then(|v| capability_bits(v)) != Some(caps) {
                    let mut value = vec![0; 20];
                    value[..4].copy_from_slice(&0x02000001_u32.to_le_bytes());
                    value[4..8].copy_from_slice(&(caps as u32).to_le_bytes());
                    value[12..16].copy_from_slice(&((caps >> 32) as u32).to_le_bytes());
                    entry.xattrs.insert("security.capability".to_owned(), value);
                }
            } else if original.is_none() {
                entry.xattrs.remove("security.capability");
            }
        }
        let mounted_context = format!("/{}", mounted);
        for context in self.contexts.iter().rev() {
            if context.mode.is_none_or(|mode| mode == entry.mode & S_IFMT)
                && (context.regex.is_match(&mounted_context) || context.regex.is_match(relative))
            {
                if context.label == "<<none>>" {
                    entry.xattrs.remove("security.selinux");
                } else if !entry
                    .xattrs
                    .get("security.selinux")
                    .is_some_and(|existing| {
                        let length = existing
                            .iter()
                            .rposition(|&byte| byte != 0)
                            .map_or(0, |index| index + 1);
                        existing[..length] == *context.label.as_bytes()
                    })
                {
                    entry.xattrs.insert(
                        "security.selinux".to_owned(),
                        context.label.as_bytes().to_vec(),
                    );
                }
                break;
            }
        }
        entry.uid = adjusted_id(
            self.options.force_uid.unwrap_or(entry.uid),
            self.options.uid_offset,
        )?;
        entry.gid = adjusted_id(
            self.options.force_gid.unwrap_or(entry.gid),
            self.options.gid_offset,
        )?;
        if !self.options.preserve_mtime {
            entry.mtime = self.options.timestamp.unwrap_or(0);
            entry.mtime_nsec = 0;
        } else if original.is_none()
            && let Some(timestamp) = self.options.timestamp
            && entry.mtime >= timestamp
        {
            entry.mtime = timestamp;
            entry.mtime_nsec = 0;
        }
        Ok(entry)
    }

    fn symlink_target(
        &self,
        relative: &str,
        host: &Path,
        original: Option<&EntryMetadata>,
    ) -> Result<String> {
        let observed =
            fs::read_link(host).with_context(|| format!("reading symlink {}", host.display()))?;
        #[cfg(not(windows))]
        {
            let _ = (relative, original);
            observed
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("symlink target is not UTF-8"))
        }
        #[cfg(windows)]
        {
            let observed = observed
                .to_str()
                .context("symlink target is not UTF-8")?
                .replace('\\', "/");
            let observed = observed.as_str();
            let observed_components = windows_path_components(observed)?;
            if let Some(target) = original.and_then(|e| e.symlink.as_ref()) {
                if target == observed {
                    return Ok(target.clone());
                }
                if target.starts_with('/')
                    && let Some(root) = self
                        .manifest
                        .as_ref()
                        .and_then(|m| m.extraction_root.as_ref())
                {
                    let expected = root.join(target.trim_start_matches('/'));
                    let expected = windows_path_components(
                        expected.to_str().context("extraction root is not UTF-8")?,
                    )?;
                    if windows_components_match(&observed_components, &expected) {
                        return Ok(target.clone());
                    }
                }
            }
            let mut root = host.to_path_buf();
            for _ in relative.trim_start_matches('/').split('/') {
                root.pop();
            }
            let root = windows_path_components(root.to_str().context("source root is not UTF-8")?)?;
            if observed_components.len() >= root.len()
                && windows_components_match(&observed_components[..root.len()], &root)
            {
                return Ok(format!("/{}", observed_components[root.len()..].join("/")));
            }
            ensure!(
                !observed.contains(':') && !observed.starts_with("//"),
                "symlink {} points outside its source tree: {observed}",
                host.display()
            );
            Ok(observed.to_owned())
        }
    }
}

#[cfg(windows)]
fn windows_path_components(path: &str) -> Result<Vec<String>> {
    use std::path::Component;
    let path = path.replace('\\', "/");
    let path = if let Some(unc) = path.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else {
        path.strip_prefix("//?/").unwrap_or(&path).to_owned()
    };
    let mut components: Vec<String> = Vec::new();
    for component in Path::new(&path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if components
                    .last()
                    .is_some_and(|part| part != "/" && part != ".." && !part.ends_with(':')) =>
            {
                components.pop();
            }
            Component::RootDir => components.push("/".to_owned()),
            _ => components.push(
                component
                    .as_os_str()
                    .to_str()
                    .context("Windows path is not UTF-8")?
                    .to_owned(),
            ),
        }
    }
    Ok(components)
}

#[cfg(windows)]
fn windows_components_match(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn normalize_path(path: &str) -> String {
    path.trim_matches('/').to_owned()
}

fn adjusted_id(id: u32, offset: i64) -> Result<u32> {
    u32::try_from(
        i64::from(id)
            .checked_add(offset)
            .context("ownership offset overflow")?,
    )
    .context("ownership offset is outside the u32 range")
}

fn capability_bits(value: &[u8]) -> Option<u64> {
    if value.len() < 12 {
        return None;
    }
    let magic = u32::from_le_bytes(value[..4].try_into().ok()?) & 0xff000000;
    let low = u32::from_le_bytes(value[4..8].try_into().ok()?) as u64;
    match magic {
        0x01000000 if value.len() == 12 => Some(low),
        0x02000000 | 0x03000000 if matches!(value.len(), 20 | 24) => {
            Some(low | (u32::from_le_bytes(value[12..16].try_into().ok()?) as u64) << 32)
        }
        _ => None,
    }
}

fn host_entry(metadata: &fs::Metadata) -> Result<EntryMetadata> {
    #[cfg(unix)]
    let (mode, uid, gid, mtime, mtime_nsec, rdev) = {
        use std::os::unix::fs::MetadataExt;
        (
            metadata.mode(),
            metadata.uid(),
            metadata.gid(),
            u64::try_from(metadata.mtime()).context("timestamps before 1970 are unsupported")?,
            metadata.mtime_nsec() as u32,
            u32::try_from(metadata.rdev()).context("device number exceeds EROFS encoding")?,
        )
    };
    #[cfg(not(unix))]
    let (mode, uid, gid, mtime, mtime_nsec, rdev) = {
        let mode = if metadata.file_type().is_symlink() {
            S_IFLNK | 0o777
        } else if metadata.is_dir() {
            S_IFDIR | 0o755
        } else if metadata.is_file() {
            S_IFREG | 0o644
        } else {
            bail!("unsupported host file type");
        };
        let time = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .context("timestamps before 1970 are unsupported")?;
        (mode, 0, 0, time.as_secs(), time.subsec_nanos(), 0)
    };
    Ok(EntryMetadata {
        mode,
        uid,
        gid,
        mtime,
        mtime_nsec,
        rdev,
        original_nid: None,
        symlink: None,
        xattrs: BTreeMap::new(),
    })
}

fn host_xattrs(path: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut attributes = BTreeMap::new();
    #[cfg(unix)]
    for name in
        xattr::list(path).with_context(|| format!("listing xattrs on {}", path.display()))?
    {
        let name = name
            .into_string()
            .map_err(|_| anyhow::anyhow!("xattr name is not UTF-8"))?;
        if let Some(value) = xattr::get(path, &name)? {
            attributes.insert(name, value);
        }
    }
    #[cfg(not(unix))]
    let _ = (path, &mut attributes);
    Ok(attributes)
}

