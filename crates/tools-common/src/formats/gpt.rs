use crate::bytes::{read_u32, read_u64};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

pub const GPT_HEADER_OFFSET: u64 = 512;
pub const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";

const GPT_HEADER_SIZE: usize = 92;
const GPT_ENTRY_MIN_SIZE: u32 = 128;
const GPT_ENTRY_MAX_SIZE: u32 = 4096;
const GPT_ENTRY_MAX_COUNT: u32 = 16_384;
const LOGICAL_BLOCK_SIZE: u64 = 512;
const MAX_LOGICAL_BLOCK_SIZE: u64 = 16_384;
const STORAGE_HEAD_SCAN_LIMIT: u64 = 1024 * 1024;
const PTABLE_SCAN_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GptHeader {
    pub revision: u32,
    pub header_size: u32,
    pub current_lba: u64,
    pub backup_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub disk_guid: String,
    pub partition_entry_lba: u64,
    pub partition_entry_count: u32,
    pub partition_entry_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GptPartition {
    pub index: u32,
    pub type_guid: String,
    pub unique_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub attributes: u64,
    pub name: String,
}

impl GptPartition {
    pub fn sector_count(&self) -> u64 {
        self.last_lba
            .saturating_sub(self.first_lba)
            .saturating_add(1)
    }

    pub fn byte_len(&self, block_size: u64) -> u64 {
        self.sector_count().saturating_mul(block_size)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GptTable {
    pub image_offset: u64,
    pub entry_array_offset: u64,
    pub block_size: u64,
    pub header: GptHeader,
    pub partitions: Vec<GptPartition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GptInfo {
    pub tables: Vec<GptTable>,
}

impl GptInfo {
    pub fn partition_count(&self) -> usize {
        self.tables.iter().map(|table| table.partitions.len()).sum()
    }
}

/// Parse a GPT from a raw storage head captured with `upload-storage`.
pub fn parse_storage_head(head: &[u8]) -> io::Result<GptInfo> {
    if head.len() < LOGICAL_BLOCK_SIZE as usize + GPT_HEADER_SIZE {
        return Err(invalid("storage head is too small for a GPT header"));
    }

    let header_offset = find_storage_header_offset(head)
        .ok_or_else(|| invalid("not an EFI GPT storage head (EFI PART signature missing)"))?;
    let header = parse_header_bytes(&head[header_offset as usize..])?;
    let block_size = header_offset / header.current_lba.max(1);
    if !(LOGICAL_BLOCK_SIZE..=MAX_LOGICAL_BLOCK_SIZE).contains(&block_size) {
        return Err(invalid(
            "GPT header offset implies an invalid logical block size",
        ));
    }

    let standard_entry_offset = header
        .partition_entry_lba
        .checked_mul(block_size)
        .ok_or_else(|| invalid("GPT partition entry offset overflows"))?;
    let adjacent_entry_offset = header_offset
        .checked_add(block_size)
        .ok_or_else(|| invalid("GPT adjacent partition entry offset overflows"))?;
    let entry_offsets = if standard_entry_offset == adjacent_entry_offset {
        vec![standard_entry_offset]
    } else {
        vec![standard_entry_offset, adjacent_entry_offset]
    };

    let mut best: Option<(u64, Vec<GptPartition>)> = None;
    for entry_offset in entry_offsets {
        if let Ok(partitions) = parse_partitions_in(head, entry_offset, &header)
            && best
                .as_ref()
                .is_none_or(|(_, current)| partitions.len() > current.len())
        {
            best = Some((entry_offset, partitions));
        }
    }
    let (entry_array_offset, partitions) =
        best.ok_or_else(|| invalid("GPT partition entry array missing from storage head"))?;

    Ok(GptInfo {
        tables: vec![GptTable {
            image_offset: header_offset,
            entry_array_offset,
            block_size,
            header,
            partitions,
        }],
    })
}

fn find_storage_header_offset(head: &[u8]) -> Option<u64> {
    let limit = head.len().min(STORAGE_HEAD_SCAN_LIMIT as usize);
    if limit < GPT_SIGNATURE.len() {
        return None;
    }
    (LOGICAL_BLOCK_SIZE as usize..=limit - GPT_SIGNATURE.len())
        .step_by(LOGICAL_BLOCK_SIZE as usize)
        .find(|&offset| &head[offset..offset + GPT_SIGNATURE.len()] == GPT_SIGNATURE)
        .map(|offset| offset as u64)
}

pub fn looks_like_storage_head(path: &Path) -> io::Result<bool> {
    let length = std::fs::metadata(path)?.len();
    if length < LOGICAL_BLOCK_SIZE + GPT_HEADER_SIZE as u64 {
        return Ok(false);
    }
    let head = std::fs::read(path)?;
    let limit = head.len().min(STORAGE_HEAD_SCAN_LIMIT as usize);
    Ok(
        find_storage_header_offset(&head[..limit])
            .is_some_and(|offset| offset != GPT_HEADER_OFFSET),
    )
}

pub fn parse_image(path: &Path) -> io::Result<GptInfo> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    if length < GPT_HEADER_OFFSET + GPT_HEADER_SIZE as u64 {
        return Err(invalid("image is too small for a GPT header"));
    }

    let header_offsets = find_header_offsets(&mut file, length)?;
    if header_offsets.is_empty() {
        return Err(invalid("not an EFI GPT image (EFI PART signature missing)"));
    }

    let mut tables = Vec::new();
    let mut empty_tables = Vec::new();
    for header_offset in header_offsets {
        let header = match parse_header_at(&mut file, length, header_offset) {
            Ok(header) => header,
            Err(_) if header_offset != GPT_HEADER_OFFSET => continue,
            Err(error) => return Err(error),
        };

        let block_size = infer_image_block_size(&header, header_offset);
        let standard_entry_offset = header
            .partition_entry_lba
            .checked_mul(block_size)
            .ok_or_else(|| invalid("GPT partition entry offset overflows"))?;
        let adjacent_entry_offset = header_offset
            .checked_add(block_size)
            .ok_or_else(|| invalid("GPT adjacent partition entry offset overflows"))?;
        let entry_offsets = if standard_entry_offset == adjacent_entry_offset {
            vec![standard_entry_offset]
        } else {
            vec![standard_entry_offset, adjacent_entry_offset]
        };

        let mut best_candidate: Option<(u64, Vec<GptPartition>)> = None;
        for entry_offset in entry_offsets {
            let Ok(partitions) = parse_partitions_at(&mut file, length, entry_offset, &header)
            else {
                continue;
            };
            if best_candidate
                .as_ref()
                .is_none_or(|(_, current)| partitions.len() > current.len())
            {
                best_candidate = Some((entry_offset, partitions));
            }
        }
        let Some((entry_array_offset, partitions)) = best_candidate else {
            if header_offset == GPT_HEADER_OFFSET {
                return Err(invalid(
                    "GPT partition entry array extends beyond the image",
                ));
            }
            continue;
        };

        let table = GptTable {
            image_offset: header_offset,
            entry_array_offset,
            block_size,
            header,
            partitions,
        };
        if table.partitions.is_empty() {
            empty_tables.push(table);
        } else {
            tables.push(table);
        }
    }

    if tables.is_empty()
        && let Some(table) = empty_tables.into_iter().next()
    {
        tables.push(table);
    }
    Ok(GptInfo { tables })
}

fn infer_image_block_size(header: &GptHeader, header_offset: u64) -> u64 {
    if header.current_lba == 1
        && let Some(block_size) = header_offset.checked_div(header.current_lba)
        && (LOGICAL_BLOCK_SIZE..=MAX_LOGICAL_BLOCK_SIZE).contains(&block_size)
        && block_size % LOGICAL_BLOCK_SIZE == 0
    {
        return block_size;
    }
    LOGICAL_BLOCK_SIZE
}

fn find_header_offsets(file: &mut File, length: u64) -> io::Result<Vec<u64>> {
    let scan_length = length.min(PTABLE_SCAN_LIMIT);
    let scan_length = usize::try_from(scan_length)
        .map_err(|_| invalid("GPT scan region does not fit in memory"))?;
    let mut bytes = vec![0_u8; scan_length];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut bytes)?;

    let mut offsets = Vec::new();
    for offset in (GPT_HEADER_OFFSET as usize..=scan_length - GPT_SIGNATURE.len())
        .step_by(LOGICAL_BLOCK_SIZE as usize)
    {
        if &bytes[offset..offset + GPT_SIGNATURE.len()] == GPT_SIGNATURE {
            offsets.push(offset as u64);
        }
    }
    Ok(offsets)
}

fn parse_header_at(file: &mut File, length: u64, offset: u64) -> io::Result<GptHeader> {
    let header_end = offset
        .checked_add(GPT_HEADER_SIZE as u64)
        .ok_or_else(|| invalid("GPT header offset overflows"))?;
    if header_end > length {
        return Err(invalid("GPT header extends beyond the image"));
    }
    let mut header_bytes = vec![0_u8; GPT_HEADER_SIZE];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut header_bytes)?;
    parse_header_bytes(&header_bytes)
}

fn parse_header_bytes(buf: &[u8]) -> io::Result<GptHeader> {
    if buf.len() < GPT_HEADER_SIZE {
        return Err(invalid("GPT header is truncated"));
    }
    if &buf[..GPT_SIGNATURE.len()] != GPT_SIGNATURE {
        return Err(invalid("not an EFI GPT image (EFI PART signature missing)"));
    }

    let header_size = read_u32(buf, 12)?;
    if !(GPT_HEADER_SIZE as u32..=MAX_LOGICAL_BLOCK_SIZE as u32).contains(&header_size) {
        return Err(invalid("GPT header size is outside the logical block"));
    }

    let entry_count = read_u32(buf, 80)?;
    let entry_size = read_u32(buf, 84)?;
    if entry_count > GPT_ENTRY_MAX_COUNT {
        return Err(invalid("GPT partition entry count is unreasonably large"));
    }
    if !(GPT_ENTRY_MIN_SIZE..=GPT_ENTRY_MAX_SIZE).contains(&entry_size) {
        return Err(invalid("GPT partition entry size is unsupported"));
    }

    let entry_lba = read_u64(buf, 72)?;
    Ok(GptHeader {
        revision: read_u32(buf, 8)?,
        header_size,
        current_lba: read_u64(buf, 24)?,
        backup_lba: read_u64(buf, 32)?,
        first_usable_lba: read_u64(buf, 40)?,
        last_usable_lba: read_u64(buf, 48)?,
        disk_guid: format_guid(&buf[56..72]),
        partition_entry_lba: entry_lba,
        partition_entry_count: entry_count,
        partition_entry_size: entry_size,
    })
}

fn parse_partitions_at(
    file: &mut File,
    length: u64,
    entry_offset: u64,
    header: &GptHeader,
) -> io::Result<Vec<GptPartition>> {
    let entry_bytes = u64::from(header.partition_entry_count)
        .checked_mul(u64::from(header.partition_entry_size))
        .ok_or_else(|| invalid("GPT partition entry array size overflows"))?;
    let entry_end = entry_offset
        .checked_add(entry_bytes)
        .ok_or_else(|| invalid("GPT partition entry array end overflows"))?;
    if entry_end > length {
        return Err(invalid(
            "GPT partition entry array extends beyond the image",
        ));
    }

    file.seek(SeekFrom::Start(entry_offset))?;
    let mut entry = vec![0_u8; header.partition_entry_size as usize];
    let mut partitions = Vec::new();
    for index in 0..header.partition_entry_count {
        file.read_exact(&mut entry)?;
        if let Some(partition) = parse_entry(&entry, index) {
            partitions.push(partition);
        }
    }

    Ok(partitions)
}

fn parse_partitions_in(
    buf: &[u8],
    entry_offset: u64,
    header: &GptHeader,
) -> io::Result<Vec<GptPartition>> {
    let entry_bytes = u64::from(header.partition_entry_count)
        .checked_mul(u64::from(header.partition_entry_size))
        .ok_or_else(|| invalid("GPT partition entry array size overflows"))?;
    let entry_end = entry_offset
        .checked_add(entry_bytes)
        .ok_or_else(|| invalid("GPT partition entry array end overflows"))?;
    if entry_end > buf.len() as u64 {
        return Err(invalid(
            "GPT partition entry array extends beyond the storage head",
        ));
    }

    let mut partitions = Vec::new();
    for index in 0..header.partition_entry_count {
        let start = entry_offset as usize + index as usize * header.partition_entry_size as usize;
        let entry = &buf[start..start + header.partition_entry_size as usize];
        if let Some(partition) = parse_entry(entry, index) {
            partitions.push(partition);
        }
    }

    Ok(partitions)
}

fn parse_entry(entry: &[u8], index: u32) -> Option<GptPartition> {
    if entry[..16].iter().all(|&byte| byte == 0) {
        return None;
    }

    let first_lba = read_u64(entry, 32).ok()?;
    let last_lba = read_u64(entry, 40).ok()?;
    if last_lba < first_lba {
        return None;
    }
    Some(GptPartition {
        index,
        type_guid: format_guid(&entry[..16]),
        unique_guid: format_guid(&entry[16..32]),
        first_lba,
        last_lba,
        attributes: read_u64(entry, 48).ok()?,
        name: parse_name(&entry[56..128]),
    })
}

fn parse_name(bytes: &[u8]) -> String {
    let code_units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&code_units)
}

fn format_guid(bytes: &[u8]) -> String {
    debug_assert_eq!(bytes.len(), 16);
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[3],
        bytes[2],
        bytes[1],
        bytes[0],
        bytes[5],
        bytes[4],
        bytes[7],
        bytes[6],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
