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
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;
use zip::result::ZipError;

pub const UPDATE_APP_MAGIC: [u8; 4] = [0x55, 0xaa, 0x5a, 0xa5];
const FIXED_HEADER_LEN: usize = 98;
const MAX_HEADER_LEN: u64 = 4 * 1024 * 1024;
const MAX_METADATA_LEN: u64 = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 16 * 1024;
const MAX_SCAN_LEN: u64 = 1024 * 1024;
const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Lowercase name from the header, before adding the output suffix.
    pub name: String,
    /// Unique filename: name.img, name_2.img, name_3.img, etc.
    pub output_name: String,
    pub index: usize,
    pub header_offset: u64,
    pub header_size: u64,
    pub data_offset: u64,
    pub size: u64,
    /// Fixed header, including the magic. Preserves all vendor metadata.
    pub header: [u8; FIXED_HEADER_LEN],
    /// All bytes from offset 98 to header_size. Retained, not verified.
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

/// Inspect a raw APP file or the single UPDATE.APP entry in a ZIP/ZIP64 file.
/// Payloads are streamed and discarded, including when reading a compressed ZIP.
pub fn inspect(input: &Path) -> Result<Index> {
    with_input(input, |reader, length| inspect_reader(reader, Some(length)))
}

/// Inspect a raw APP stream, starting at offset zero.
/// When supplied, total_length must be the complete stream length.
pub fn inspect_reader<R: Read>(reader: R, total_length: Option<u64>) -> Result<Index> {
    walk_records(reader, total_length, |_, _| Ok(()))
}

/// Extract all images from a raw APP or a ZIP/ZIP64 containing UPDATE.APP.
pub fn unpack(input: &Path, out: &Path, force: bool) -> Result<Vec<Extracted>> {
    unpack_selected(input, out, None, force)
}

/// Extract matching header names (case-insensitive, optional .img suffix).
/// None or an empty list selects all records; a name selects all its duplicates.
/// Existing files require force. Unrelated files in out are left intact.
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

/// Extract from a raw APP stream using the same selection rules as unpack_selected.
/// Callers must ensure the output directory does not contain the stream's source.
/// Writes are atomic per image; completed images remain if a later record fails.
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
    match ZipArchive::new(file) {
        Ok(mut archive) => {
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
            let mut entry =
                archive.by_index(found.context("archive does not contain UPDATE.APP")?)?;
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

fn walk_records<R: Read>(
    reader: R,
    total_length: Option<u64>,
    mut visit: impl FnMut(&Record, &mut dyn Read) -> Result<()>,
) -> Result<Index> {
    let mut reader = BufReader::with_capacity(IO_BUFFER_SIZE, reader);
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

        // Bound the visitor to this payload so image data containing magic is
        // never mistaken for a record. Unselected payloads are drained as well.
        let mut payload = reader.by_ref().take(size);
        visit(&record, &mut payload)
            .with_context(|| format!("extracting {}", record.output_name))?;
        let remaining = payload.limit();
        let skipped = io::copy(&mut payload, &mut io::sink())?;
        ensure!(
            skipped == remaining,
            "UPDATE.APP record {} is truncated",
            record.name
        );
        offset = payload_end;
        let mut padding = [0_u8; 3];
        let padding_len = ((4 - offset % 4) % 4) as usize;
        // A final payload may end at EOF without its optional alignment bytes.
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
