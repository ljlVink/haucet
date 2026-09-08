//! Huawei UPDATE.APP inspection and streaming image extraction.
//!
//! Format reference: splituapp by SuperR. @XDA, based on the app_structure
//! file in split_updata.pl by McSpoon (https://github.com/superr/splituapp).
//! The fixed header is 98 bytes including magic; the remaining header bytes
//! hold variable-length CRC data. CRC verification is not implemented.

use crate::{bytes, fs_util};
use anyhow::{Context, Result, ensure};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use zip::result::ZipError;
use zip::{CompressionMethod, ZipArchive};

pub const UPDATE_APP_MAGIC: [u8; 4] = [0x55, 0xaa, 0x5a, 0xa5];
const FIXED_HEADER_LEN: usize = 98;
const MAX_HEADER_LEN: u64 = 4 * 1024 * 1024;
const MAX_METADATA_LEN: u64 = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 16 * 1024;
const MAX_SCAN_LEN: u64 = 1024 * 1024;
const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;
const ZIP_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub output_name: String,
    pub index: usize,
    pub header_offset: u64,
    pub header_size: u64,
    pub data_offset: u64,
    pub size: u64,
    pub header: [u8; FIXED_HEADER_LEN],
    pub checksum: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub records: Vec<Record>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub record: Record,
    pub path: PathBuf,
}

/// Probe a raw file for aligned APP magic within the parser's initial scan limit.
/// This does not validate records or decompress ZIP entries.
pub fn probe_file(input: &Path) -> Result<bool> {
    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let mut reader = BufReader::new(file);
    let mut word = [0_u8; 4];
    for _ in 0..=MAX_SCAN_LEN / 4 {
        if read_up_to(&mut reader, &mut word)? != word.len() {
            return Ok(false);
        }
        if word == UPDATE_APP_MAGIC {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn inspect(input: &Path) -> Result<Index> {
    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    match ZipArchive::new(BufReader::with_capacity(ZIP_BUFFER_SIZE, file)) {
        Ok(mut archive) => inspect_archive(&mut archive),
        Err(ZipError::InvalidArchive(_)) => {
            let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
            inspect_seekable_reader(file)
        }
        Err(error) => Err(error).context("probing UPDATE.APP as ZIP/ZIP64 archive"),
    }
}

fn inspect_archive<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<Index> {
    let entry_index = find_update_app(archive)?;
    let directory_start = archive.central_directory_start();
    let mut entry = archive.by_index(entry_index)?;
    let size = entry.size();
    if entry.compression() == CompressionMethod::Stored {
        ensure!(
            entry.compressed_size() == size,
            "invalid stored UPDATE.APP size"
        );
        let end = entry
            .data_start()
            .checked_add(size)
            .context("UPDATE.APP ZIP offset overflow")?;
        ensure!(
            end <= directory_start,
            "stored UPDATE.APP extends into the ZIP directory"
        );
        drop(entry);
        inspect_seekable_reader(archive.by_index_seek(entry_index)?)
    } else {
        // APP headers are interleaved with payloads. A compressed entry
        // must still be decoded sequentially to reach every header.
        inspect_reader(&mut entry, Some(size))
    }
}

pub fn inspect_reader<R: Read>(reader: R, total_length: Option<u64>) -> Result<Index> {
    walk_records(reader, total_length, |_, _| Ok(()))
}

fn inspect_seekable_reader<R: Read + Seek>(mut reader: R) -> Result<Index> {
    let length = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    walk_records_with_skip(
        BufReader::new(reader),
        Some(length),
        |reader, remaining| {
            // walk_records_with_skip checks each payload end against length
            // before seeking, since seeking past EOF itself would succeed.
            reader.seek_relative(i64::try_from(remaining)?)?;
            Ok(())
        },
        |_, _| Ok(()),
    )
}

pub fn unpack(input: &Path, out: &Path, force: bool) -> Result<Vec<Extracted>> {
    unpack_selected(input, out, None, force)
}

pub fn unpack_selected(
    input: &Path,
    out: &Path,
    names: Option<&[String]>,
    force: bool,
) -> Result<Vec<Extracted>> {
    fs_util::ensure_output_does_not_contain(input, out)?;
    with_input(input, |reader, length| {
        unpack_reader(reader, Some(length), out, names, force)
    })
}

pub fn unpack_reader<R: Read>(
    reader: R,
    total_length: Option<u64>,
    out: &Path,
    names: Option<&[String]>,
    force: bool,
) -> Result<Vec<Extracted>> {
    let requested = normalize_selection(names.unwrap_or_default())?;
    let mut extracted = Vec::new();
    let index = walk_records(reader, total_length, |record, payload| {
        if !requested.is_empty() && !requested.contains(&record.name) {
            return Ok(());
        }
        let path = fs_util::safe_join(out, &record.output_name)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(force, "output already exists: {}", path.display());
                ensure!(
                    metadata.file_type().is_file(),
                    "output is not a regular file: {}",
                    path.display()
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
        if force {
            // A cancelled GUI worker can leave this image's atomic-write temporary.
            let temporary = fs_util::sibling_temporary(&path, "splituapp")?;
            match fs::symlink_metadata(&temporary) {
                Ok(metadata) => {
                    ensure!(
                        metadata.file_type().is_file(),
                        "temporary output is not a regular file: {}",
                        temporary.display()
                    );
                    fs::remove_file(&temporary).with_context(|| {
                        format!("removing incomplete image {}", temporary.display())
                    })?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("checking {}", temporary.display()));
                }
            }
        }
        eprintln!("extracting {} ({} bytes)", record.output_name, record.size);
        fs_util::atomic_write(&path, "splituapp", |writer| {
            let copied = io::copy(payload, writer)?;
            ensure!(
                copied == record.size,
                "UPDATE.APP record {} is truncated: expected {} bytes, read {copied}",
                record.name,
                record.size
            );
            Ok(())
        })?;
        extracted.push(Extracted {
            record: record.clone(),
            path,
        });
        Ok(())
    })?;
    for name in requested {
        ensure!(
            index.records.iter().any(|record| record.name == name),
            "UPDATE.APP does not contain requested image {name:?}"
        );
    }
    Ok(extracted)
}

fn with_input<T>(input: &Path, read: impl FnOnce(&mut dyn Read, u64) -> Result<T>) -> Result<T> {
    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let length = file.metadata()?.len();
    match ZipArchive::new(BufReader::with_capacity(ZIP_BUFFER_SIZE, file)) {
        Ok(mut archive) => {
            let entry_index = find_update_app(&mut archive)?;
            let mut entry = archive.by_index(entry_index)?;
            let size = entry.size();
            read(&mut entry, size)
        }
        Err(ZipError::InvalidArchive(_)) => {
            let mut file =
                File::open(input).with_context(|| format!("opening {}", input.display()))?;
            read(&mut file, length)
        }
        Err(error) => Err(error).context("probing UPDATE.APP as ZIP/ZIP64 archive"),
    }
}

fn find_update_app<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<usize> {
    let mut found = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let enclosed = entry
            .enclosed_name()
            .with_context(|| format!("unsafe ZIP entry name {:?}", entry.name()))?;
        if !entry.is_dir()
            && enclosed
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("UPDATE.APP"))
        {
            ensure!(
                found.is_none(),
                "archive contains multiple UPDATE.APP entries"
            );
            found = Some(index);
        }
    }
    found.context("archive does not contain UPDATE.APP")
}

fn walk_records<R: Read>(
    reader: R,
    total_length: Option<u64>,
    visit: impl FnMut(&Record, &mut dyn Read) -> Result<()>,
) -> Result<Index> {
    walk_records_with_skip(
        BufReader::with_capacity(IO_BUFFER_SIZE, reader),
        total_length,
        |reader, remaining| {
            let skipped = io::copy(&mut reader.take(remaining), &mut io::sink())?;
            ensure!(skipped == remaining, "UPDATE.APP record is truncated");
            Ok(())
        },
        visit,
    )
}

fn walk_records_with_skip<R: Read>(
    mut reader: R,
    total_length: Option<u64>,
    mut skip: impl FnMut(&mut R, u64) -> Result<()>,
    mut visit: impl FnMut(&Record, &mut dyn Read) -> Result<()>,
) -> Result<Index> {
    let mut offset = 0_u64;
    let mut metadata_size = 0_u64;
    let mut occurrences = HashMap::<String, usize>::new();
    let mut output_names = HashSet::new();
    let mut records = Vec::new();
    while scan_magic(&mut reader, &mut offset)? {
        ensure!(
            records.len() < MAX_RECORDS,
            "UPDATE.APP contains too many records"
        );
        let header_offset = offset - 4;
        let mut header = [0_u8; FIXED_HEADER_LEN];
        header[..4].copy_from_slice(&UPDATE_APP_MAGIC);
        reader
            .read_exact(&mut header[4..8])
            .context("reading UPDATE.APP header size")?;
        let header_size = u64::from(bytes::read_u32(&header, 4)?);
        ensure!(
            (FIXED_HEADER_LEN as u64..=MAX_HEADER_LEN).contains(&header_size),
            "invalid UPDATE.APP header size {header_size} at {header_offset:#x}"
        );
        metadata_size += header_size;
        ensure!(
            metadata_size <= MAX_METADATA_LEN,
            "UPDATE.APP metadata exceeds 64 MiB"
        );
        let data_offset = header_offset
            .checked_add(header_size)
            .context("UPDATE.APP header offset overflow")?;
        check_length(data_offset, total_length)?;
        reader
            .read_exact(&mut header[8..])
            .context("reading UPDATE.APP fixed header")?;
        let size = u64::from(bytes::read_u32(&header, 24)?);
        let payload_end = data_offset
            .checked_add(size)
            .context("UPDATE.APP payload offset overflow")?;
        check_length(payload_end, total_length)?;

        let raw_name = &header[60..76];
        let end = raw_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(raw_name.len());
        let name = std::str::from_utf8(&raw_name[..end])
            .context("UPDATE.APP name is not UTF-8")?
            .to_ascii_lowercase();
        validate_name(&name)?;
        let mut checksum = vec![0; header_size as usize - FIXED_HEADER_LEN];
        reader
            .read_exact(&mut checksum)
            .context("reading UPDATE.APP CRC data")?;
        let output_name = unique_output_name(&name, &mut occurrences, &mut output_names);
        let record = Record {
            name,
            output_name,
            index: records.len(),
            header_offset,
            header_size,
            data_offset,
            size,
            header,
            checksum,
        };
        let mut payload = reader.by_ref().take(size);
        visit(&record, &mut payload)
            .with_context(|| format!("extracting {}", record.output_name))?;
        let remaining = payload.limit();
        skip(&mut reader, remaining)
            .with_context(|| format!("skipping UPDATE.APP record {}", record.name))?;
        offset = payload_end;
        let mut padding = [0_u8; 3];
        let padding_len = ((4 - offset % 4) % 4) as usize;
        offset += read_up_to(&mut reader, &mut padding[..padding_len])? as u64;
        check_length(offset, total_length)?;
        records.push(record);
    }
    ensure!(!records.is_empty(), "no UPDATE.APP records found");
    if let Some(length) = total_length {
        ensure!(
            offset == length,
            "UPDATE.APP length mismatch: expected {length} bytes, read {offset}"
        );
    }
    Ok(Index { records })
}

fn check_length(end: u64, total_length: Option<u64>) -> Result<()> {
    if let Some(length) = total_length {
        ensure!(
            end <= length,
            "UPDATE.APP is truncated: record ends at {end}, stream length is {length}"
        );
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        fs_util::is_simple_name(name)
            && name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !b"<>:\"|?*".contains(&byte))
            && !name.ends_with('.'),
        "unsafe UPDATE.APP record name {name:?}"
    );
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let numbered_device = ["com", "lpt"].iter().any(|prefix| {
        stem.strip_prefix(prefix)
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    });
    ensure!(
        !matches!(stem.as_str(), "con" | "prn" | "aux" | "nul") && !numbered_device,
        "reserved UPDATE.APP record name {name:?}"
    );
    Ok(())
}

fn normalize_selection(names: &[String]) -> Result<HashSet<String>> {
    names
        .iter()
        .map(|name| {
            let lowercase = name.to_ascii_lowercase();
            let name = lowercase.strip_suffix(".img").unwrap_or(&lowercase);
            validate_name(name)?;
            Ok(name.to_owned())
        })
        .collect()
}

fn unique_output_name(
    name: &str,
    occurrences: &mut HashMap<String, usize>,
    used: &mut HashSet<String>,
) -> String {
    let occurrence = occurrences.entry(name.to_owned()).or_default();
    loop {
        *occurrence += 1;
        let candidate = if *occurrence == 1 {
            format!("{name}.img")
        } else {
            format!("{name}_{occurrence}.img")
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
}

fn scan_magic<R: Read>(reader: &mut R, offset: &mut u64) -> Result<bool> {
    let start = *offset;
    let mut word = [0_u8; 4];
    loop {
        let count = read_up_to(reader, &mut word)?;
        *offset = offset
            .checked_add(count as u64)
            .context("UPDATE.APP offset overflow")?;
        if count == 0 {
            return Ok(false);
        }
        ensure!(
            count == 4,
            "truncated UPDATE.APP record magic at {:#x}",
            *offset - count as u64
        );
        if word == UPDATE_APP_MAGIC {
            return Ok(true);
        }
        ensure!(
            *offset - start <= MAX_SCAN_LEN,
            "no UPDATE.APP magic within 1 MiB at {start:#x}"
        );
    }
}

fn read_up_to<R: Read>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}
