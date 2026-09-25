use anyhow::{Context, Result, bail, ensure};
use crc32c::{crc32c as calculate_crc32c, crc32c_append};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const NVE_BLOCK_SIZE: usize = 0x20_000;
pub const NVE_PARTITION_COUNT: usize = 8;
pub const NVE_COMMIT_SECTOR_SIZE: usize = 512;
pub const NVE_ITEM_SIZE: usize = 128;
pub const NVE_ITEMS_PER_BLOCK: usize = 1023;
pub const NVE_HEADER_SIZE: usize = 128;
pub const NVE_DATA_SIZE: usize = 104;
pub const NVE_NAME_SIZE: usize = 8;
pub const NVE_INVALID_AGE: u32 = 0;
pub const NVE_CRC_SUPPORT_VERSION: u32 = 2;
pub const NVE_HEADER_MAGIC: &[u8] = b"Hisi-NV-Partition";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NveImageSummary {
    pub file_size: u64,
    pub total_blocks: usize,
    pub active_blocks: usize,
    pub partition_name: String,
    pub version: u32,
    pub valid_items: usize,
    pub crc_supported: bool,
    pub crc_valid: usize,
    pub crc_invalid: usize,
    pub blocks: Vec<NveBlockSummary>,
    pub items: Vec<NveItemSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NveBlockSummary {
    pub block_index: usize,
    pub offset: u64,
    pub partition_name: String,
    pub version: u32,
    pub block_id: u32,
    pub block_count: u32,
    pub declared_valid_items: u32,
    pub valid_items: usize,
    pub age: u32,
    pub crc_supported: bool,
    pub crc_valid: usize,
    pub crc_invalid: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NveItemSummary {
    pub block_index: usize,
    pub slot: usize,
    pub number: u32,
    pub name: String,
    pub property: u32,
    pub valid_size: usize,
    pub crc: u32,
    pub crc_supported: bool,
    pub crc_valid: bool,
    pub value_hex: String,
    pub value_text: String,
    pub kernel_protected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NveEditResult {
    pub backup_path: String,
    pub updated_items: usize,
    pub source_block: usize,
    pub committed_block: usize,
    pub age: u32,
    pub value_size: usize,
    pub value_hex: String,
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NveCommitResult {
    pub updated_items: usize,
    pub source_block: usize,
    pub committed_block: usize,
    pub age: u32,
    original_target: Vec<u8>,
}

#[derive(Debug, Clone)]
struct NveItem {
    slot: usize,
    raw: [u8; NVE_ITEM_SIZE],
}

impl NveItem {
    fn number(&self) -> u32 {
        read_u32(&self.raw, 0)
    }

    fn name(&self) -> String {
        decode_name(&self.raw[4..12])
    }

    fn property(&self) -> u32 {
        read_u32(&self.raw, 12)
    }

    fn valid_size(&self) -> usize {
        (read_u32(&self.raw, 16) as usize).min(NVE_DATA_SIZE)
    }

    fn declared_size(&self) -> usize {
        read_u32(&self.raw, 16) as usize
    }

    fn crc(&self) -> u32 {
        read_u32(&self.raw, 20)
    }

    fn value(&self) -> &[u8] {
        &self.raw[24..24 + self.valid_size()]
    }

    fn crc_valid(&self) -> bool {
        self.crc() == compute_item_crc(&self.raw)
    }

    fn update_value(&mut self, value: &[u8], update_crc: bool) -> Result<()> {
        let declared_size = self.declared_size();
        ensure!(
            declared_size <= NVE_DATA_SIZE,
            "NVE item {} declares an invalid value size: {}",
            self.number(),
            declared_size
        );
        ensure!(
            value.len() <= declared_size,
            "NVE value is too long for item {}: {} bytes (maximum {})",
            self.name(),
            value.len(),
            declared_size
        );
        self.raw[24..].fill(0);
        self.raw[24..24 + value.len()].copy_from_slice(value);
        if update_crc {
            let crc = compute_item_crc(&self.raw);
            self.raw[20..24].copy_from_slice(&crc.to_le_bytes());
        }
        Ok(())
    }

    fn summary(&self, block_index: usize, crc_supported: bool) -> NveItemSummary {
        let name = self.name();
        let value = self.value();
        NveItemSummary {
            block_index,
            slot: self.slot,
            number: self.number(),
            name: name.clone(),
            property: self.property(),
            valid_size: self.valid_size(),
            crc: self.crc(),
            crc_supported,
            crc_valid: self.crc_valid(),
            value_hex: hex::encode(value),
            value_text: value_text(&name, value),
            kernel_protected: matches!(self.number(), 2 | 193 | 194 | 364),
        }
    }
}

#[derive(Debug, Clone)]
struct NveHeader {
    partition_name: String,
    version: u32,
    block_id: u32,
    block_count: u32,
    valid_items: u32,
    crc_support: u32,
    age: u32,
}

#[derive(Debug, Clone)]
struct NveBlock {
    index: usize,
    header: Option<NveHeader>,
    items: Vec<NveItem>,
}

impl NveBlock {
    fn crc_supported(&self) -> bool {
        self.header
            .as_ref()
            .is_some_and(|header| header.crc_support == NVE_CRC_SUPPORT_VERSION)
    }

    fn declared_items(&self) -> &[NveItem] {
        let count = self
            .header
            .as_ref()
            .map_or(0, |header| header.valid_items as usize)
            .min(NVE_ITEMS_PER_BLOCK);
        &self.items[..count]
    }

    fn numbered_prefix_items(&self) -> &[NveItem] {
        let items = self.declared_items();
        let count = items
            .iter()
            .enumerate()
            .take_while(|(slot, item)| item.number() == *slot as u32)
            .count();
        &items[..count]
    }

    fn kernel_items(&self) -> Result<Option<&[NveItem]>> {
        let Some(header) = self.header.as_ref() else {
            return Ok(None);
        };
        let count = header.valid_items as usize;
        ensure!(
            count <= NVE_ITEMS_PER_BLOCK,
            "NVE block {} declares an invalid item count: {}",
            self.index,
            count
        );
        let items = self.numbered_prefix_items();
        if header.crc_support == NVE_CRC_SUPPORT_VERSION
            && (items.is_empty() || items.iter().any(|item| !item.crc_valid()))
        {
            return Ok(None);
        }
        Ok(Some(items))
    }

    fn summary(&self) -> NveBlockSummary {
        let (partition_name, version, block_id, block_count, declared_valid_items, age) =
            match &self.header {
                Some(header) => (
                    header.partition_name.clone(),
                    header.version,
                    header.block_id,
                    header.block_count,
                    header.valid_items,
                    header.age,
                ),
                None => (String::new(), 0, 0, 0, 0, 0),
            };
        let mut crc_valid = 0;
        let mut crc_invalid = 0;
        let crc_supported = self.crc_supported();
        if crc_supported {
            for item in self.numbered_prefix_items() {
                if item.crc_valid() {
                    crc_valid += 1;
                } else {
                    crc_invalid += 1;
                }
            }
        }
        NveBlockSummary {
            block_index: self.index,
            offset: (self.index * NVE_BLOCK_SIZE) as u64,
            partition_name,
            version,
            block_id,
            block_count,
            declared_valid_items,
            valid_items: self.numbered_prefix_items().len(),
            age,
            crc_supported,
            crc_valid,
            crc_invalid,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NveImage {
    raw: Vec<u8>,
    blocks: Vec<NveBlock>,
}

impl NveImage {
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw =
            fs::read(path).with_context(|| format!("reading NVE image {}", path.display()))?;
        Self::from_bytes(raw)
    }

    pub fn from_bytes(raw: Vec<u8>) -> Result<Self> {
        ensure!(
            raw.len() >= NVE_BLOCK_SIZE,
            "NVE image is too small: {} bytes",
            raw.len()
        );
        ensure!(
            raw.chunks_exact(NVE_BLOCK_SIZE).remainder().is_empty(),
            "NVE image size must be a multiple of 0x{:X}: 0x{:X}",
            NVE_BLOCK_SIZE,
            raw.len()
        );

        let mut blocks = Vec::with_capacity(raw.len() / NVE_BLOCK_SIZE);
        for (index, block_raw) in raw.chunks_exact(NVE_BLOCK_SIZE).enumerate() {
            blocks.push(parse_block(index, block_raw));
        }

        ensure!(
            blocks
                .iter()
                .take(NVE_PARTITION_COUNT)
                .any(|block| block.header.is_some()),
            "NVE image contains no recognized partition headers"
        );
        Ok(Self { raw, blocks })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    pub fn summary(&self) -> NveImageSummary {
        let managed = self.managed_blocks();
        let active_blocks = self.valid_runtime_indices();
        let current = self
            .current_source_index()
            .and_then(|index| managed.get(index));
        let current_items = current.and_then(|block| block.kernel_items().ok().flatten());
        let current_index = current.map(|block| block.index);
        let current_crc_supported = current.is_some_and(NveBlock::crc_supported);
        let items: Vec<NveItemSummary> = current_items
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.summary(
                            current_index.expect("current block exists"),
                            current_crc_supported,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (partition_name, version) = current
            .and_then(|block| block.header.as_ref())
            .map(|header| (header.partition_name.clone(), header.version))
            .unwrap_or_default();
        let crc_supported = managed.iter().any(NveBlock::crc_supported);
        let (crc_valid, crc_invalid) = managed
            .iter()
            .filter(|block| block.crc_supported())
            .flat_map(NveBlock::numbered_prefix_items)
            .fold((0, 0), |(valid, invalid), item| {
                if item.crc_valid() {
                    (valid + 1, invalid)
                } else {
                    (valid, invalid + 1)
                }
            });
        let valid_items = items.len();

        NveImageSummary {
            file_size: self.raw.len() as u64,
            total_blocks: managed.len(),
            active_blocks: active_blocks.len(),
            partition_name,
            version,
            valid_items,
            crc_supported,
            crc_valid,
            crc_invalid,
            blocks: managed
                .iter()
                .filter(|block| block.header.is_some())
                .map(NveBlock::summary)
                .collect(),
            items,
        }
    }

    pub fn write_entry(&mut self, key: &str, value: &[u8]) -> Result<NveCommitResult> {
        let key = normalize_key(key)?;
        ensure!(
            self.blocks.len() >= NVE_PARTITION_COUNT,
            "NVE image must contain all {} managed blocks before it can be edited",
            NVE_PARTITION_COUNT
        );
        let source_index = self.current_source_index_checked()?.ok_or_else(|| {
            anyhow::anyhow!(
                "NVE image has no valid current or update block; header, item count, numbering, or CRC validation failed"
            )
        })?;
        let source = &self.blocks[source_index];
        let source_items = source
            .kernel_items()?
            .ok_or_else(|| anyhow::anyhow!("NVE source block {source_index} is invalid"))?;
        let matching_slots = source_items
            .iter()
            .filter(|item| item.name().eq_ignore_ascii_case(&key))
            .map(|item| item.slot)
            .collect::<Vec<_>>();
        ensure!(!matching_slots.is_empty(), "NVE entry not found: {key}");
        ensure!(
            matching_slots.len() == 1,
            "NVE entry name is ambiguous in block {source_index}: {key}"
        );
        let slot = matching_slots[0];
        if let Some(update_items) = self
            .managed_blocks()
            .first()
            .map(NveBlock::kernel_items)
            .transpose()?
            .flatten()
        {
            ensure!(
                slot < update_items.len() && source.items[slot].property() != 0,
                "NVE block 0 contains a pending update that would overwrite entry {key}"
            );
        }
        let source_header = source.header.as_ref().expect("validated NVE header");
        let age = source_header
            .age
            .checked_add(1)
            .filter(|age| *age != NVE_INVALID_AGE)
            .ok_or_else(|| anyhow::anyhow!("NVE generation counter overflow"))?;
        let committed_block = if source_index == 0 || source_index + 1 >= NVE_PARTITION_COUNT {
            1
        } else {
            source_index + 1
        };

        let mut next = source.clone();
        next.index = committed_block;
        next.items[slot].update_value(value, source.crc_supported())?;
        let next_header = next.header.as_mut().expect("validated NVE header");
        next_header.valid_items = source_items.len() as u32;
        next_header.age = age;

        let source_start = source_index * NVE_BLOCK_SIZE;
        let target_start = committed_block * NVE_BLOCK_SIZE;
        let original_target = self.raw[target_start..target_start + NVE_BLOCK_SIZE].to_vec();
        let block_raw = self.raw[source_start..source_start + NVE_BLOCK_SIZE].to_vec();
        self.raw[target_start..target_start + NVE_BLOCK_SIZE].copy_from_slice(&block_raw);
        let item_start = target_start + slot * NVE_ITEM_SIZE;
        self.raw[item_start..item_start + NVE_ITEM_SIZE].copy_from_slice(&next.items[slot].raw);
        let count_start = target_start + NVE_BLOCK_SIZE - NVE_HEADER_SIZE + 44;
        self.raw[count_start..count_start + 4]
            .copy_from_slice(&(source_items.len() as u32).to_le_bytes());
        let age_start = target_start + NVE_BLOCK_SIZE - std::mem::size_of::<u32>();
        self.raw[age_start..age_start + 4].copy_from_slice(&age.to_le_bytes());
        self.blocks[committed_block] = next;

        Ok(NveCommitResult {
            updated_items: 1,
            source_block: source_index,
            committed_block,
            age,
            original_target,
        })
    }

    fn managed_blocks(&self) -> &[NveBlock] {
        &self.blocks[..self.blocks.len().min(NVE_PARTITION_COUNT)]
    }

    fn valid_runtime_indices(&self) -> Vec<usize> {
        self.managed_blocks()
            .iter()
            .skip(1)
            .filter(|block| {
                block
                    .header
                    .as_ref()
                    .is_some_and(|header| header.age != NVE_INVALID_AGE)
                    && block.kernel_items().ok().flatten().is_some()
            })
            .map(|block| block.index)
            .collect()
    }

    fn current_source_index(&self) -> Option<usize> {
        self.current_source_index_checked().ok().flatten()
    }

    fn current_source_index_checked(&self) -> Result<Option<usize>> {
        let mut current = None;
        let mut current_age = NVE_INVALID_AGE;
        for block in self.managed_blocks().iter().skip(1) {
            let Some(_items) = block.kernel_items()? else {
                continue;
            };
            let age = block.header.as_ref().expect("validated NVE header").age;
            if age > current_age {
                current = Some(block.index);
                current_age = age;
            }
        }
        if current.is_some() {
            return Ok(current);
        }
        let update = self
            .managed_blocks()
            .first()
            .map(NveBlock::kernel_items)
            .transpose()?
            .flatten();
        Ok(update.map(|_| 0))
    }
}

pub fn inspect(path: &Path) -> Result<NveImageSummary> {
    Ok(NveImage::from_file(path)?.summary())
}

pub fn edit_file_in_place(
    path: &Path,
    key: &str,
    input_value: &str,
    value_format: &str,
) -> Result<NveEditResult> {
    let key = normalize_key(key)?;
    let (value, mode) = encode_value(&key, input_value, value_format)?;
    let value_hex = hex::encode(&value);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening NVE image for editing {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.file_type().is_file(),
        "NVE in-place editing only supports regular files; copy a live block device to an image first"
    );
    fs2::FileExt::lock_exclusive(&file)
        .with_context(|| format!("locking NVE image for editing {}", path.display()))?;

    file.seek(SeekFrom::Start(0))?;
    let mut original = Vec::new();
    file.read_to_end(&mut original)
        .with_context(|| format!("reading locked NVE image {}", path.display()))?;
    let mut image = NveImage::from_bytes(original.clone())?;
    let commit = image.write_entry(&key, &value)?;
    let backup_path = crate::fs_util::create_backup(path, &original, metadata.permissions(), "NVE")
        .with_context(|| format!("backing up NVE image {}", path.display()))?;
    write_committed_block(&mut file, path, &image, &commit)?;
    Ok(NveEditResult {
        backup_path: backup_path.display().to_string(),
        updated_items: commit.updated_items,
        source_block: commit.source_block,
        committed_block: commit.committed_block,
        age: commit.age,
        value_size: value.len(),
        value_hex,
        mode,
    })
}

fn encode_value(key: &str, input: &str, value_format: &str) -> Result<(Vec<u8>, String)> {
    let (value, mode) = match value_format.trim().to_ascii_lowercase().as_str() {
        "hex" => {
            let compact = input
                .strip_prefix("0x")
                .or_else(|| input.strip_prefix("0X"))
                .unwrap_or(input)
                .chars()
                .filter(|ch| !ch.is_ascii_whitespace())
                .collect::<String>();
            let value = hex::decode(&compact).with_context(|| "invalid hexadecimal NVE value")?;
            ensure!(
                value.len() <= NVE_DATA_SIZE,
                "NVE value is too long: {} bytes (maximum {})",
                value.len(),
                NVE_DATA_SIZE
            );
            (value, "Hex".to_owned())
        }
        "text" | "ascii" | "utf8" => {
            if key.eq_ignore_ascii_case("FBLOCK") {
                return match input.trim() {
                    "0" => Ok((vec![0], "FBLOCK (unlocked)".to_owned())),
                    "1" => Ok((vec![1], "FBLOCK (locked)".to_owned())),
                    _ => bail!("FBLOCK only accepts 0 or 1"),
                };
            }
            let value = input.as_bytes().to_vec();
            ensure!(
                value.len() <= NVE_DATA_SIZE,
                "NVE value is too long: {} bytes (maximum {})",
                value.len(),
                NVE_DATA_SIZE
            );
            (value, "Text".to_owned())
        }
        other => bail!("unsupported NVE value format: {other} (use text or hex)"),
    };
    if key.eq_ignore_ascii_case("FBLOCK") {
        ensure!(
            matches!(value.as_slice(), [0] | [1]),
            "FBLOCK only accepts exactly one byte with value 0 or 1"
        );
        let state = if value[0] == 0 { "unlocked" } else { "locked" };
        return Ok((value, format!("FBLOCK ({state})")));
    }
    Ok((value, mode))
}

fn normalize_key(key: &str) -> Result<String> {
    let key = key.trim();
    ensure!(!key.is_empty(), "NVE entry name cannot be empty");
    ensure!(
        key.len() <= NVE_NAME_SIZE,
        "NVE entry name is longer than 8 bytes"
    );
    ensure!(key.is_ascii(), "NVE entry name must be ASCII");
    Ok(key.to_ascii_uppercase())
}

fn parse_block(index: usize, raw: &[u8]) -> NveBlock {
    debug_assert_eq!(raw.len(), NVE_BLOCK_SIZE);
    let header = parse_header(&raw[NVE_BLOCK_SIZE - NVE_HEADER_SIZE..]);
    let mut items = Vec::with_capacity(NVE_ITEMS_PER_BLOCK);
    for slot in 0..NVE_ITEMS_PER_BLOCK {
        let start = slot * NVE_ITEM_SIZE;
        let mut item_raw = [0_u8; NVE_ITEM_SIZE];
        item_raw.copy_from_slice(&raw[start..start + NVE_ITEM_SIZE]);
        items.push(NveItem {
            slot,
            raw: item_raw,
        });
    }
    NveBlock {
        index,
        header,
        items,
    }
}

fn parse_header(raw: &[u8]) -> Option<NveHeader> {
    if raw.len() != NVE_HEADER_SIZE || !raw.starts_with(NVE_HEADER_MAGIC) {
        return None;
    }
    Some(NveHeader {
        partition_name: decode_name(&raw[..32]),
        version: read_u32(raw, 32),
        block_id: read_u32(raw, 36),
        block_count: read_u32(raw, 40),
        valid_items: read_u32(raw, 44),
        crc_support: read_u32(raw, 52),
        age: read_u32(raw, 124),
    })
}

fn decode_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_owned()
}

fn read_u32(raw: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        raw[offset],
        raw[offset + 1],
        raw[offset + 2],
        raw[offset + 3],
    ])
}

fn compute_item_crc(raw: &[u8; NVE_ITEM_SIZE]) -> u32 {
    let crc = calculate_crc32c(&raw[..20]);
    crc32c_append(crc, &raw[24..])
}

fn value_text(name: &str, value: &[u8]) -> String {
    // Special Judge for some irregular partitions.
    if name.eq_ignore_ascii_case("FBLOCK") && value.len() == 1 {
        return match value[0] {
            0 => "0".to_owned(),
            1 => "1".to_owned(),
            _ => String::new(),
        };
    }

    let trimmed = value
        .iter()
        .rposition(|byte| *byte != 0)
        .map(|last| &value[..=last])
        .unwrap_or(&[]);
    if trimmed
        .iter()
        .all(|byte| *byte == b'\t' || *byte == b'\n' || *byte == b'\r' || (32..=126).contains(byte))
    {
        String::from_utf8_lossy(trimmed).into_owned()
    } else {
        String::new()
    }
}

fn write_committed_block(
    file: &mut fs::File,
    path: &Path,
    image: &NveImage,
    commit: &NveCommitResult,
) -> Result<()> {
    let block_start = commit.committed_block * NVE_BLOCK_SIZE;
    let block_end = block_start + NVE_BLOCK_SIZE;
    let committed = &image.as_bytes()[block_start..block_end];
    image.blocks[commit.committed_block]
        .kernel_items()
        .context("validating the committed NVE block before writing")?
        .context("committed NVE block failed item or CRC validation")?;
    ensure!(
        file.metadata()?.len() == image.as_bytes().len() as u64,
        "NVE image size changed before writing {}",
        path.display()
    );
    let managed_len = NVE_PARTITION_COUNT * NVE_BLOCK_SIZE;
    let mut managed_on_disk = vec![0_u8; managed_len];
    read_at(file, 0, &mut managed_on_disk)?;
    for index in 0..NVE_PARTITION_COUNT {
        let start = index * NVE_BLOCK_SIZE;
        let expected = if index == commit.committed_block {
            commit.original_target.as_slice()
        } else {
            &image.as_bytes()[start..start + NVE_BLOCK_SIZE]
        };
        ensure!(
            &managed_on_disk[start..start + NVE_BLOCK_SIZE] == expected,
            "NVE managed block {index} changed before writing {}",
            path.display()
        );
    }

    let sector_offset = NVE_BLOCK_SIZE - NVE_COMMIT_SECTOR_SIZE;
    let mut invalid_sector = committed[sector_offset..].to_vec();
    invalid_sector[NVE_COMMIT_SECTOR_SIZE - 4..].copy_from_slice(&NVE_INVALID_AGE.to_le_bytes());
    let target_offset = block_start as u64;
    let sector_file_offset = (block_start + sector_offset) as u64;

    let result = (|| -> Result<()> {
        write_at(file, sector_file_offset, &invalid_sector)?;
        file.sync_all()
            .with_context(|| format!("flushing invalid NVE header in {}", path.display()))?;

        write_at(file, target_offset, &committed[..sector_offset])?;
        file.sync_all()
            .with_context(|| format!("flushing NVE block data in {}", path.display()))?;

        let mut staged = vec![0_u8; NVE_BLOCK_SIZE];
        read_at(file, target_offset, &mut staged)?;
        let mut expected_staged = committed.to_vec();
        expected_staged[NVE_BLOCK_SIZE - 4..].copy_from_slice(&NVE_INVALID_AGE.to_le_bytes());
        ensure!(
            staged == expected_staged,
            "NVE block {} failed staged write verification",
            commit.committed_block
        );
        parse_block(commit.committed_block, &staged)
            .kernel_items()
            .context("validating the staged NVE block")?
            .context("staged NVE block failed item or CRC validation")?;

        write_at(file, sector_file_offset, &committed[sector_offset..])?;
        file.sync_all()
            .with_context(|| format!("committing NVE generation in {}", path.display()))?;

        read_at(file, target_offset, &mut staged)?;
        ensure!(
            staged == committed,
            "NVE block {} failed final write verification",
            commit.committed_block
        );
        Ok(())
    })();

    if result.is_err() {
        let _ = write_at(file, sector_file_offset, &invalid_sector);
        let _ = file.sync_all();
    }
    result
}

fn write_at(file: &mut fs::File, offset: u64, data: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    Ok(())
}

fn read_at(file: &mut fs::File, offset: u64, data: &mut [u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(data)?;
    Ok(())
}
