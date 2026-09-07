use super::hvb::{HvbCert, HvbFooter, HvbWrapper};
use crate::fs_util;
use crate::tools::ToolPaths;
use anyhow::{Context, Result, ensure};
use erofs::{BuildOptions, ExtractOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const EROFS_MAGIC: [u8; 4] = [0xe2, 0xe1, 0xf5, 0xe0];
const EROFS_MAGIC_OFFSET: u64 = 1024;
const MANIFEST_NAME: &str = "haucet-erofs.json";
const MANIFEST_VERSION: u32 = 1;
const CERTIFICATE_NAME: &str = "hvb-certificate.bin";
const HASH_BUFFER_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErofsManifest {
    pub version: u32,
    pub partition: String,
    pub original_file_name: String,
    pub original_size: u64,
    pub original_sha256: String,
    pub source_dir: String,
    pub config_dir: String,
    pub fs_options_file: String,
    pub extract_erofs_version: String,
    pub mkfs_erofs_version: String,
    pub hvb: Option<HvbManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HvbManifest {
    pub footer: HvbFooter,
    pub certificate_file: String,
}

pub fn is_erofs(path: &Path) -> Result<bool> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    if file.metadata()?.len() < EROFS_MAGIC_OFFSET + EROFS_MAGIC.len() as u64 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(EROFS_MAGIC_OFFSET))?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)?;
    Ok(magic == EROFS_MAGIC)
}

pub fn unpack(image: &Path, out: &Path, force: bool) -> Result<()> {
    fs_util::ensure_output_does_not_contain(image, out)?;
    ensure!(
        is_erofs(image)?,
        "{} is not an EROFS image",
        image.display()
    );
    fs_util::prepare_dir_excluding(out, "EROFS workspace", force, &[image])?;

    eprintln!("extracting EROFS image {}", image.display());
    erofs::extract(image, out, ExtractOptions::default())
        .context("embedded EROFS extraction failed")?;

    let extracted_source_dir = find_source_dir(out)?;
    let extracted_name = extracted_source_dir
        .file_name()
        .and_then(OsStr::to_str)
        .context("extracted EROFS root has a non-UTF-8 name")?
        .to_owned();
    let config_dir = out.join("config");

    let wrapper = HvbWrapper::read_from(image)?;
    let partition = wrapper
        .as_ref()
        .and_then(HvbWrapper::partition_name)
        .filter(|name| fs_util::is_simple_name(name))
        .unwrap_or(&extracted_name)
        .to_owned();
    let source_dir = normalize_extraction(
        out,
        &config_dir,
        extracted_source_dir,
        &extracted_name,
        &partition,
    )?;
    let fs_options = find_file_with_suffix(&config_dir, "_fs_options")?;

    let hvb = if let Some(wrapper) = wrapper {
        let certificate_path = config_dir.join(CERTIFICATE_NAME);
        fs::write(&certificate_path, &wrapper.certificate)?;
        Some(HvbManifest {
            footer: wrapper.footer,
            certificate_file: relative_string(out, &certificate_path)?,
        })
    } else {
        None
    };

    let manifest = ErofsManifest {
        version: MANIFEST_VERSION,
        partition,
        original_file_name: image
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("partition.img")
            .to_owned(),
        original_size: fs::metadata(image)?.len(),
        original_sha256: sha256_file(image)?,
        source_dir: relative_string(out, &source_dir)?,
        config_dir: relative_string(out, &config_dir)?,
        fs_options_file: relative_string(out, &fs_options)?,
        extract_erofs_version: erofs::VERSION.to_owned(),
        mkfs_erofs_version: erofs::MKFS_VERSION.to_owned(),
        hvb,
    };
    write_manifest(out, &manifest)?;
    eprintln!("wrote {}", out.join(MANIFEST_NAME).display());
    Ok(())
}

/// Compatibility entry point; repacking uses the embedded Rust writer.
pub fn repack_with_tools(
    workspace: &Path,
    output: &Path,
    _tools: &ToolPaths,
    allow_grow: bool,
) -> Result<()> {
    repack(workspace, output, allow_grow)
}

pub fn repack(workspace: &Path, output: &Path, allow_grow: bool) -> Result<()> {
    let workspace = fs_util::absolute_path(workspace)?;
    let output = fs_util::absolute_path(output)?;
    ensure!(
        !output.exists(),
        "output already exists: {}",
        output.display()
    );
    let manifest = read_manifest(&workspace)?;
    ensure!(
        manifest.version == MANIFEST_VERSION,
        "unsupported EROFS workspace version {}",
        manifest.version
    );
    let source_dir = fs_util::safe_join(&workspace, &manifest.source_dir)?;
    let config_dir = fs_util::safe_join(&workspace, &manifest.config_dir)?;
    let fs_options_path = fs_util::safe_join(&workspace, &manifest.fs_options_file)?;
    ensure!(
        source_dir.is_dir(),
        "missing source tree: {}",
        source_dir.display()
    );
    ensure!(
        config_dir.is_dir(),
        "missing config directory: {}",
        config_dir.display()
    );

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let raw_path = fs_util::sibling_temporary(&output, "raw-erofs")?;
    let wrapped_path = fs_util::sibling_temporary(&output, "wrapped")?;
    ensure!(
        !raw_path.exists(),
        "temporary file exists: {}",
        raw_path.display()
    );
    ensure!(
        !wrapped_path.exists(),
        "temporary file exists: {}",
        wrapped_path.display()
    );

    let result = (|| -> Result<()> {
        let options = parse_mkfs_options(&fs_options_path, &config_dir)?;
        eprintln!(
            "rebuilding {} with embedded Rust mkfs.erofs",
            manifest.partition
        );
        erofs::build(&source_dir, &raw_path, &options).context("embedded EROFS building failed")?;
        ensure!(
            is_erofs(&raw_path)?,
            "EROFS writer produced an invalid image"
        );

        let raw_size = fs::metadata(&raw_path)?.len();
        if let Some(hvb) = &manifest.hvb {
            let certificate_path = fs_util::safe_join(&workspace, &hvb.certificate_file)?;
            let wrapper = HvbWrapper {
                footer: hvb.footer.clone(),
                certificate: fs::read(&certificate_path).with_context(|| {
                    format!("reading HVB certificate {}", certificate_path.display())
                })?,
            };
            ensure!(
                wrapper.certificate.len() as u64 == wrapper.footer.cert_size,
                "HVB certificate size changed"
            );
            validate_hvb_image_size(raw_size, &wrapper)?;
            wrapper.write_repacked(&raw_path, &wrapped_path)?;
            eprintln!(
                "warning: the original HVB certificate was preserved, not cryptographically re-signed"
            );
        } else {
            ensure!(
                allow_grow || raw_size <= manifest.original_size,
                "rebuilt image is {raw_size} bytes, larger than original size {}; use --allow-grow to override",
                manifest.original_size
            );
            copy_raw_partition(
                &raw_path,
                &wrapped_path,
                if allow_grow {
                    raw_size.max(manifest.original_size)
                } else {
                    manifest.original_size
                },
            )?;
        }

        ensure!(is_erofs(&wrapped_path)?, "wrapped output is not EROFS");
        erofs::verify_image(&wrapped_path).context("embedded EROFS validation failed")?;
        fs::rename(&wrapped_path, &output)
            .with_context(|| format!("moving rebuilt image to {}", output.display()))?;
        Ok(())
    })();

    let _ = fs::remove_file(&raw_path);
    if result.is_err() {
        let _ = fs::remove_file(&wrapped_path);
    }
    result?;
    eprintln!("wrote {}", output.display());
    Ok(())
}

fn validate_hvb_image_size(raw_size: u64, wrapper: &HvbWrapper) -> Result<()> {
    let certificate =
        HvbCert::parse(&wrapper.certificate).context("parsing preserved HVB certificate")?;
    let limit = if certificate.image_len != 0 {
        certificate.image_len
    } else {
        wrapper.footer.image_size
    };
    ensure!(
        raw_size <= limit,
        "rebuilt EROFS image is {raw_size} bytes, exceeding the preserved HVB image length {limit}; \
         the filesystem would extend past the device mapping. Reduce its size; --allow-grow cannot \
         enlarge the mapping recorded in an unchanged HVB certificate"
    );
    Ok(())
}

fn find_source_dir(workspace: &Path) -> Result<PathBuf> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.file_name() != OsStr::new("config") {
            directories.push(entry.path());
        }
    }
    ensure!(
        directories.len() == 1,
        "expected one extracted filesystem root in {}, found {}",
        workspace.display(),
        directories.len()
    );
    Ok(directories.remove(0))
}

fn find_file_with_suffix(directory: &Path, suffix: &str) -> Result<PathBuf> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("reading config directory {}", directory.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_string_lossy().ends_with(suffix) {
            matches.push(entry.path());
        }
    }
    ensure!(
        matches.len() == 1,
        "expected one *{suffix} file in {}, found {}",
        directory.display(),
        matches.len()
    );
    Ok(matches.remove(0))
}

fn normalize_extraction(
    workspace: &Path,
    config_dir: &Path,
    source_dir: PathBuf,
    extracted_name: &str,
    partition: &str,
) -> Result<PathBuf> {
    if extracted_name == partition {
        return Ok(source_dir);
    }
    ensure!(
        fs_util::is_simple_name(partition),
        "unsafe partition name in HVB certificate: {partition:?}"
    );
    let normalized_source = workspace.join(partition);
    ensure!(
        !normalized_source.exists(),
        "normalized source path already exists: {}",
        normalized_source.display()
    );
    fs::rename(&source_dir, &normalized_source)?;

    for suffix in ["_fs_config", "_file_contexts", "_fs_options"] {
        let old_path = find_file_with_suffix(config_dir, suffix)?;
        let new_path = config_dir.join(format!("{partition}{suffix}"));
        fs::rename(old_path, new_path)?;
    }
    Ok(normalized_source)
}

fn parse_mkfs_options(path: &Path, config_dir: &Path) -> Result<BuildOptions> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading mkfs options from {}", path.display()))?;
    let line = text
        .lines()
        .find_map(|line| {
            line.split_once("mkfs.erofs options:")
                .map(|(_, value)| value.trim())
        })
        .context("fs_options does not contain a mkfs.erofs command")?;
    let mut words = shlex::split(line).context("invalid shell quoting in mkfs.erofs options")?;
    ensure!(
        words.len() >= 2,
        "mkfs.erofs options are missing output/source paths"
    );
    words.truncate(words.len() - 2);

    let mut options =
        BuildOptions::from_args(&words).context("parsing recorded mkfs.erofs options")?;
    for (recorded_path, suffix) in [
        (&mut options.fs_config, "_fs_config"),
        (&mut options.file_contexts, "_file_contexts"),
    ] {
        if let Some(original) = recorded_path {
            let original = original.to_string_lossy();
            let basename = original
                .rsplit(['/', '\\'])
                .next()
                .filter(|name| !name.is_empty())
                .with_context(|| format!("invalid {suffix} path"))?;
            let original_path =
                fs_util::is_simple_name(basename).then(|| config_dir.join(basename));
            let has_windows_drive = original.as_bytes().get(1) == Some(&b':')
                && original
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic);
            ensure!(
                original_path.is_some() || has_windows_drive,
                "invalid {suffix} path"
            );
            let path = if let Some(original_path) = original_path.filter(|path| path.is_file()) {
                original_path
            } else {
                // Resolve renamed configs and legacy unquoted Windows paths,
                // whose backslashes shlex interprets as shell escapes.
                find_file_with_suffix(config_dir, suffix)?
            };
            ensure!(path.is_file(), "missing {suffix} file: {}", path.display());
            *recorded_path = Some(path);
        }
    }
    Ok(options)
}

fn copy_raw_partition(source: &Path, destination: &Path, final_size: u64) -> Result<()> {
    let mut source = BufReader::new(File::open(source)?);
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    destination_file.set_len(final_size)?;
    let mut destination = BufWriter::new(destination_file);
    io::copy(&mut source, &mut destination)?;
    destination.flush()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::with_capacity(HASH_BUFFER_SIZE, File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_SIZE];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn write_manifest(workspace: &Path, manifest: &ErofsManifest) -> Result<()> {
    let path = workspace.join(MANIFEST_NAME);
    let json = serde_json::to_vec_pretty(manifest)?;
    fs_util::atomic_write(&path, "manifest", |writer| {
        writer.write_all(&json)?;
        Ok(())
    })
}

pub fn read_manifest(workspace: &Path) -> Result<ErofsManifest> {
    let path = workspace.join(MANIFEST_NAME);
    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).context("parsing EROFS workspace manifest")
}

fn relative_string(base: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(base)
        .with_context(|| format!("{} is outside {}", path.display(), base.display()))?
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_image(directory: &Path) -> PathBuf {
        let source = directory.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("system-note.txt"),
            b"original firmware payload\n",
        )
        .unwrap();
        let image = directory.join("system.img");
        erofs::build(&source, &image, &BuildOptions::default()).unwrap();
        image
    }

    #[test]
    fn repacks_without_external_tools_and_preserves_config_and_size() {
        let temp = tempfile::tempdir().unwrap();
        let image = fixture_image(temp.path());
        let original_size = 256 * 1024;
        OpenOptions::new()
            .write(true)
            .open(&image)
            .unwrap()
            .set_len(original_size)
            .unwrap();
        let workspace = temp.path().join("workspace with spaces");
        unpack(&image, &workspace, false).unwrap();
        let manifest = read_manifest(&workspace).unwrap();
        assert_eq!(manifest.mkfs_erofs_version, erofs::MKFS_VERSION);
        let source = workspace.join(&manifest.source_dir);
        let payload = b"edited firmware payload\n";
        fs::write(source.join("system-note.txt"), payload).unwrap();
        let config = workspace.join(&manifest.config_dir);
        assert!(!config.join("erofs-metadata.json").exists());
        fs::write(
            find_file_with_suffix(&config, "_fs_config").unwrap(),
            "/ 0 0 0755\n/system-note.txt 123 456 4750\n",
        )
        .unwrap();
        fs::write(
            find_file_with_suffix(&config, "_file_contexts").unwrap(),
            "/system-note\\.txt u:object_r:system_file:s0\n",
        )
        .unwrap();

        let output = temp.path().join("rebuilt.img");
        repack(&workspace, &output, false).unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), original_size);
        let extracted = temp.path().join("checked");
        unpack(&output, &extracted, false).unwrap();
        let rebuilt = read_manifest(&extracted).unwrap();
        assert_eq!(
            fs::read(extracted.join(&rebuilt.source_dir).join("system-note.txt")).unwrap(),
            payload
        );
        let rebuilt_config = extracted.join(&rebuilt.config_dir);
        let fs_config =
            fs::read_to_string(find_file_with_suffix(&rebuilt_config, "_fs_config").unwrap())
                .unwrap();
        assert!(
            fs_config
                .lines()
                .any(|line| line == "/system-note.txt 123 456 4750")
        );
        let contexts =
            fs::read_to_string(find_file_with_suffix(&rebuilt_config, "_file_contexts").unwrap())
                .unwrap();
        assert!(contexts.contains("u:object_r:system_file:s0"));
    }

    #[test]
    fn repack_preserves_hvb_certificate_and_config_paths_after_normalization() {
        let temp = tempfile::tempdir().unwrap();
        let raw = fixture_image(temp.path());
        let mut certificate = vec![0_u8; 240];
        certificate[..4].copy_from_slice(b"HVB\0");
        certificate[64..70].copy_from_slice(b"vendor");
        let wrapper = HvbWrapper {
            footer: HvbFooter {
                cert_offset: 128 * 1024,
                cert_size: certificate.len() as u64,
                image_size: fs::metadata(&raw).unwrap().len(),
                partition_size: 256 * 1024,
            },
            certificate,
        };
        let wrapped_dir = temp.path().join("wrapped");
        fs::create_dir(&wrapped_dir).unwrap();
        let image = wrapped_dir.join("system.img");
        wrapper.write_repacked(&raw, &image).unwrap();
        let workspace = temp.path().join("work");
        unpack(&image, &workspace, false).unwrap();
        let manifest = read_manifest(&workspace).unwrap();
        assert_eq!(manifest.source_dir, "vendor");
        assert!(!workspace.join("config/erofs-metadata.json").exists());
        let fs_config = fs::read_to_string(workspace.join("config/vendor_fs_config")).unwrap();
        assert!(fs_config.contains("/system-note.txt "));
        assert!(!fs_config.contains("/vendor-note.txt "));
        let output = temp.path().join("rebuilt.img");
        repack(&workspace, &output, true).unwrap();
        let rebuilt = HvbWrapper::read_from(&output).unwrap().unwrap();
        assert_eq!(rebuilt.certificate, wrapper.certificate);
        assert_eq!(rebuilt.footer.cert_offset, wrapper.footer.cert_offset);
        assert_eq!(rebuilt.footer.partition_size, wrapper.footer.partition_size);
        assert_eq!(
            fs::metadata(&output).unwrap().len(),
            wrapper.footer.partition_size
        );

        let certificate_path = workspace.join("config/hvb-certificate.bin");
        let mut certificate = fs::read(&certificate_path).unwrap();
        certificate[56..64].copy_from_slice(&4096u64.to_le_bytes());
        fs::write(&certificate_path, certificate).unwrap();
        for allow_grow in [false, true] {
            let rejected = temp.path().join(format!("oversized-{allow_grow}.img"));
            let error = repack(&workspace, &rejected, allow_grow).unwrap_err();
            assert!(format!("{error:#}").contains("exceeding the preserved HVB image length"));
            assert!(!rejected.exists());
            assert!(
                !fs_util::sibling_temporary(&rejected, "raw-erofs")
                    .unwrap()
                    .exists()
            );
            assert!(
                !fs_util::sibling_temporary(&rejected, "wrapped")
                    .unwrap()
                    .exists()
            );
        }
    }

    #[test]
    fn rejected_growth_leaves_no_output_and_can_be_explicitly_allowed() {
        let temp = tempfile::tempdir().unwrap();
        let image = fixture_image(temp.path());
        let workspace = temp.path().join("work");
        unpack(&image, &workspace, false).unwrap();
        let mut manifest = read_manifest(&workspace).unwrap();
        manifest.original_size = 1;
        write_manifest(&workspace, &manifest).unwrap();
        let output = temp.path().join("rebuilt.img");
        let error = repack(&workspace, &output, false).unwrap_err();
        assert!(format!("{error:#}").contains("larger than original size"));
        assert!(!output.exists());
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.contains("raw-erofs") && !name.contains("wrapped")
        }));
        repack(&workspace, &output, true).unwrap();
        assert!(fs::metadata(&output).unwrap().len() > manifest.original_size);
    }

    #[test]
    fn parses_split_cluster_size_and_quoted_relocated_paths() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config with spaces");
        fs::create_dir(&config_dir).unwrap();
        fs::write(config_dir.join("system_fs_config"), []).unwrap();
        fs::write(config_dir.join("system_file_contexts"), []).unwrap();
        let options = temp.path().join("system_fs_options");
        fs::write(
            &options,
            "mkfs.erofs options: -zlz4hc -C 16384 --fs-config-file '/old work/config/system_fs_config' --file-contexts='/old work/config/system_file_contexts' 'new system.img' '/old work/system'\n",
        )
        .unwrap();
        let parsed = parse_mkfs_options(&options, &config_dir).unwrap();
        assert_eq!(parsed.cluster_size, 16384);
        assert_eq!(parsed.compression, erofs::Compression::Lz4Hc { level: 9 });
        assert_eq!(parsed.fs_config, Some(config_dir.join("system_fs_config")));
        assert_eq!(
            parsed.file_contexts,
            Some(config_dir.join("system_file_contexts"))
        );
    }

    #[test]
    fn refuses_unsupported_recorded_options() {
        let temp = tempfile::tempdir().unwrap();
        let options = temp.path().join("system_fs_options");
        fs::write(
            &options,
            "mkfs.erofs options: --not-a-real-option out.img source\n",
        )
        .unwrap();
        assert!(parse_mkfs_options(&options, temp.path()).is_err());
    }

    #[test]
    fn preserves_quoted_backslashes_in_volume_labels() {
        let temp = tempfile::tempdir().unwrap();
        let options = temp.path().join("system_fs_options");
        fs::write(
            &options,
            "mkfs.erofs options: -L 'stock\\image' out.img source\n",
        )
        .unwrap();
        let parsed = parse_mkfs_options(&options, temp.path()).unwrap();
        assert_eq!(parsed.volume_label, "stock\\image");
    }

    #[test]
    fn preserves_option_like_volume_labels() {
        let temp = tempfile::tempdir().unwrap();
        let options = temp.path().join("system_fs_options");
        fs::write(
            &options,
            "mkfs.erofs options: -L '--file-contexts' out.img source\n",
        )
        .unwrap();
        let parsed = parse_mkfs_options(&options, temp.path()).unwrap();
        assert_eq!(parsed.volume_label, "--file-contexts");
        assert!(parsed.file_contexts.is_none());
    }

    #[test]
    fn parses_unquoted_windows_paths_from_fs_options() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("system_fs_config"), []).unwrap();
        fs::write(config_dir.join("system_file_contexts"), []).unwrap();
        let options = temp.path().join("system_fs_options");
        fs::write(
            &options,
            "mkfs.erofs options: -zlz4hc --fs-config-file=C:\\old\\config\\system_fs_config --file-contexts=C:\\old\\config\\system_file_contexts system_repack.img C:\\old\\system\n",
        )
        .unwrap();

        let parsed = parse_mkfs_options(&options, &config_dir).unwrap();
        assert_eq!(parsed.fs_config, Some(config_dir.join("system_fs_config")));
        assert_eq!(
            parsed.file_contexts,
            Some(config_dir.join("system_file_contexts"))
        );
    }
}
