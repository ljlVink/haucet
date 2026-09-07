use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};

use crate::compression::Compression;

/// Options for building a single-device EROFS image from a directory.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub compression: Compression,
    pub block_size: u32,
    pub cluster_size: u32,
    pub timestamp: Option<u64>,
    pub uuid: Option<[u8; 16]>,
    pub volume_label: String,
    pub fs_config: Option<PathBuf>,
    pub file_contexts: Option<PathBuf>,
    pub metadata_file: Option<PathBuf>,
    pub mount_point: String,
    pub force_uid: Option<u32>,
    pub force_gid: Option<u32>,
    pub uid_offset: i64,
    pub gid_offset: i64,
    pub preserve_mtime: bool,
    /// Disable host xattr scanning; recorded metadata still applies.
    pub no_xattrs: bool,
    pub xattr_tolerance: u32,
    pub inline_data: bool,
    pub compact_indexes: bool,
    pub checksum: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            compression: Compression::default(),
            block_size: 4096,
            cluster_size: 4096,
            timestamp: None,
            uuid: None,
            volume_label: String::new(),
            fs_config: None,
            file_contexts: None,
            metadata_file: None,
            mount_point: String::new(),
            force_uid: None,
            force_gid: None,
            uid_offset: 0,
            gid_offset: 0,
            preserve_mtime: true,
            no_xattrs: false,
            xattr_tolerance: 2,
            inline_data: true,
            compact_indexes: true,
            checksum: true,
        }
    }
}

impl BuildOptions {
    /// Parse mkfs.erofs options only, without the output and source operands.
    /// Unsupported upstream modes fail explicitly instead of changing semantics.
    pub fn from_args(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            let (key, inline) = if arg.starts_with("--") {
                arg.split_once('=')
                    .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)))
            } else if arg.starts_with('-') && arg.as_bytes().get(1).is_some_and(u8::is_ascii) {
                (&arg[..2], (arg.len() > 2).then_some(&arg[2..]))
            } else {
                bail!("unexpected mkfs.erofs operand {arg:?}");
            };
            let takes_value = matches!(
                key,
                "-z" | "-b"
                    | "-C"
                    | "-T"
                    | "-U"
                    | "-L"
                    | "-d"
                    | "-x"
                    | "-E"
                    | "--fs-config-file"
                    | "--file-contexts"
                    | "--metadata"
                    | "--mount-point"
                    | "--force-uid"
                    | "--force-gid"
                    | "--uid-offset"
                    | "--gid-offset"
            );
            let value = if takes_value {
                if let Some(value) = inline {
                    value
                } else {
                    index += 1;
                    args.get(index)
                        .with_context(|| format!("{key} requires a value"))?
                }
            } else {
                ensure!(inline.is_none(), "{key} does not accept a value");
                ""
            };
            match key {
                "-z" => options.compression = value.parse()?,
                "-b" => options.block_size = value.parse().context("invalid block size")?,
                "-C" => options.cluster_size = value.parse().context("invalid cluster size")?,
                "-T" => options.timestamp = Some(value.parse().context("invalid timestamp")?),
                "-U" => options.uuid = Some(parse_uuid(value)?),
                "-L" => options.volume_label = value.to_owned(),
                "--fs-config-file" => options.fs_config = Some(value.into()),
                "--file-contexts" => options.file_contexts = Some(value.into()),
                "--metadata" => options.metadata_file = Some(value.into()),
                "--mount-point" => options.mount_point = value.trim_matches('/').to_owned(),
                "--force-uid" => options.force_uid = Some(value.parse().context("invalid uid")?),
                "--force-gid" => options.force_gid = Some(value.parse().context("invalid gid")?),
                "--uid-offset" => {
                    options.uid_offset = value.parse().context("invalid uid offset")?
                }
                "--gid-offset" => {
                    options.gid_offset = value.parse().context("invalid gid offset")?
                }
                "--all-root" => {
                    options.force_uid = Some(0);
                    options.force_gid = Some(0);
                }
                "--ignore-mtime" => options.preserve_mtime = false,
                "--preserve-mtime" => options.preserve_mtime = true,
                "-d" => {
                    ensure!(
                        value.parse::<u8>().is_ok_and(|v| v <= 9),
                        "invalid debug level"
                    );
                }
                "-x" => {
                    let limit: i32 = value.parse().context("invalid xattr tolerance")?;
                    ensure!(limit >= -1, "xattr tolerance must be at least -1");
                    options.no_xattrs = limit == -1;
                    if limit >= 0 {
                        options.xattr_tolerance = limit as u32;
                    }
                }
                "-E" => {
                    for feature in value.split(',') {
                        match feature {
                            "legacy-compress" => options.compact_indexes = false,
                            "noinline_data" => options.inline_data = false,
                            "nosbcrc" => options.checksum = false,
                            _ => bail!("unsupported mkfs.erofs extended option {feature:?}"),
                        }
                    }
                }
                _ => bail!("unsupported native mkfs.erofs option {arg:?}"),
            }
            index += 1;
        }
        options.validate()?;
        Ok(options)
    }

    pub fn validate(&self) -> Result<()> {
        if let Compression::Lz4Hc { level } = self.compression {
            ensure!(
                (1..=12).contains(&level),
                "LZ4HC level must be 1 through 12"
            );
        }
        ensure!(
            self.block_size.is_power_of_two() && (512..=4096).contains(&self.block_size),
            "block size must be a power of two between 512 and 4096"
        );
        ensure!(
            self.cluster_size >= self.block_size
                && self.cluster_size <= 1024 * 1024
                && self.cluster_size.is_multiple_of(self.block_size)
                && self.cluster_size / self.block_size < 2048,
            "cluster size must be a multiple of block size, at most 1 MiB, and fewer than 2048 blocks"
        );
        ensure!(
            self.volume_label.len() <= 16 && !self.volume_label.contains('\0'),
            "volume label must be at most 16 bytes and contain no NUL"
        );
        ensure!(
            !self.mount_point.split('/').any(|p| matches!(p, "." | ".."))
                && !self.mount_point.contains(['\\', '\0']),
            "invalid mount point"
        );
        Ok(())
    }
}

fn parse_uuid(text: &str) -> Result<[u8; 16]> {
    ensure!(
        text.len() == 36 && [8, 13, 18, 23].iter().all(|&i| text.as_bytes()[i] == b'-'),
        "UUID must use the 8-4-4-4-12 hexadecimal format"
    );
    let bytes: Vec<_> = text.bytes().filter(|&b| b != b'-').collect();
    ensure!(
        bytes.len() == 32 && bytes.iter().all(u8::is_ascii_hexdigit),
        "invalid UUID"
    );
    let mut uuid = [0; 16];
    for (dst, pair) in uuid.iter_mut().zip(bytes.chunks_exact(2)) {
        *dst = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_options_support_separate_and_inline_values() {
        let args = shlex::split("-zlz4hc -C 16384 -T 123 -U 01234567-89ab-cdef-0123-456789abcdef --fs-config-file='config dir/fs_config' --all-root").unwrap();
        let options = BuildOptions::from_args(&args).unwrap();
        assert_eq!(options.cluster_size, 16384);
        assert_eq!(options.timestamp, Some(123));
        assert_eq!(options.uuid.unwrap()[15], 0xef);
        assert_eq!(
            options.fs_config.unwrap(),
            PathBuf::from("config dir/fs_config")
        );
        assert_eq!(options.force_uid, Some(0));
        assert!(options.compact_indexes);
        assert!(
            !BuildOptions::from_args(&["-Elegacy-compress".into()])
                .unwrap()
                .compact_indexes
        );
    }

    #[test]
    fn rejects_unsupported_modes_and_invalid_values() {
        for args in [
            "--tar",
            "-Efragments",
            "-zunknown",
            "-b17",
            "-C8193",
            "-U not-a-uuid",
            "-T",
            "--all-root=1",
            "-\u{e9}",
        ] {
            assert!(
                BuildOptions::from_args(&shlex::split(args).unwrap()).is_err(),
                "{args}"
            );
        }
    }
}
