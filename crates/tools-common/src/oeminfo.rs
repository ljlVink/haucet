use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const OEMINFO_MAGIC: &[u8; 8] = b"OEM_INFO";
pub const OEMINFO_HEADER_SIZE: usize = 0x20;
pub const OEMINFO_REUSED_HEADER_SIZE: usize = 0x40;
pub const OEMINFO_STANDARD_HEADER_SIZE: usize = 0x200;
pub const OEMINFO_STANDARD_ALIGNMENT: usize = 0x1000;
pub const OEMINFO_REUSED_ALIGNMENT: usize = 0x80;
pub const OEMINFO_MAX_EMBEDDED_IMAGE_SIZE: u64 = 256 * 1024 * 1024;
/// Largest age accepted by the device HAL; writers roll over with
/// `(age + 1) % 0x3B9ACA00`, so slots are valid only while `age <= 0x3B9AC9FF`.
pub const OEMINFO_MAX_AGE: u32 = 0x3B9AC9FF;

const PROBE_CHUNK_SIZE: usize = 2 * 1024 * 1024;
const PROBE_SCAN_LIMIT: u64 = 64 * 1024 * 1024;
const PROBE_OVERLAP: usize = OEMINFO_STANDARD_HEADER_SIZE - 1;
const MIN_INFERRED_REGION_SIZE: usize = 1024 * 1024;
const MAX_PLAUSIBLE_VERSION: u32 = 0x1_0000;
const IMAGE_DATA_OFFSET: usize = 0x1a;
const SIGNATURE_SIZE: usize = 256;
const HIGH_ENTROPY_THRESHOLD: f64 = 7.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OemInfoLayout {
    Standard,
    StandardCompact,
    Reused,
}

impl fmt::Display for OemInfoLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard => f.write_str("STANDARD"),
            Self::StandardCompact => f.write_str("STANDARD_COMPACT"),
            Self::Reused => f.write_str("REUSED"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OemInfoRegion {
    A,
    B,
    Unknown,
}

impl fmt::Display for OemInfoRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::A => f.write_str("A"),
            Self::B => f.write_str("B"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OemInfoPayloadKind {
    Raw,
    Ascii,
    Tlv,
    ImageGzip,
    ImageRaw,
    AsciiSigned,
    RawSigned,
    AsciiSignedRandom,
    RawSignedRandom,
}

impl fmt::Display for OemInfoPayloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw => f.write_str("RAW"),
            Self::Ascii => f.write_str("ASCII"),
            Self::Tlv => f.write_str("TLV"),
            Self::ImageGzip => f.write_str("IMAGE_GZIP"),
            Self::ImageRaw => f.write_str("IMAGE_RAW"),
            Self::AsciiSigned => f.write_str("ASCII_SIGNED"),
            Self::RawSigned => f.write_str("RAW_SIGNED"),
            Self::AsciiSignedRandom => f.write_str("ASCII_SIGNED_RANDOM"),
            Self::RawSignedRandom => f.write_str("RAW_SIGNED_RANDOM"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OemInfoImageSummary {
    pub file_size: u64,
    pub region_size: u64,
    pub candidate_headers: usize,
    pub discarded_headers: usize,
    pub total_blocks: usize,
    pub active_blocks: usize,
    pub inactive_blocks: usize,
    pub region_a_blocks: usize,
    pub region_b_blocks: usize,
    pub unknown_region_blocks: usize,
    pub standard_blocks: usize,
    pub compact_blocks: usize,
    pub reused_blocks: usize,
    pub blocks: Vec<OemInfoBlockSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OemInfoBlockSummary {
    pub offset: u64,
    pub version: u32,
    pub id: u32,
    pub sub_id: u32,
    pub length: u32,
    pub age: u32,
    pub header_size: u32,
    pub layout: OemInfoLayout,
    pub region: OemInfoRegion,
    pub active: bool,
    pub payload_kind: OemInfoPayloadKind,
    pub text_preview: Option<String>,
    pub tlv_parts: usize,
    pub tlv_description: Option<String>,
    pub image_version_hex: Option<String>,
    pub image_random_adjust: Option<u32>,
    pub header_padding_byte: u8,
    pub block_padding_byte: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OemInfoEmbeddedImage {
    pub kind: OemInfoPayloadKind,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct OemInfoImage {
    raw: Vec<u8>,
    candidate_headers: usize,
    discarded_headers: usize,
    region_size: usize,
    blocks: Vec<ParsedBlock>,
}

#[derive(Debug, Clone)]
struct ParsedBlock {
    offset: usize,
    version: u32,
    id: u32,
    sub_id: u32,
    length: u32,
    age: u32,
    header_size: usize,
    layout: OemInfoLayout,
    region: OemInfoRegion,
    active: bool,
    payload: PayloadDetails,
    header_padding_byte: u8,
    block_padding_byte: u8,
}

#[derive(Debug, Clone)]
struct PayloadDetails {
    kind: OemInfoPayloadKind,
    text_preview: Option<String>,
    tlv_parts: usize,
    tlv_description: Option<String>,
    image_version_hex: Option<String>,
    image_random_adjust: Option<u32>,
}

impl PayloadDetails {
    fn raw(preview: Option<String>) -> Self {
        Self {
            kind: OemInfoPayloadKind::Raw,
            text_preview: preview,
            tlv_parts: 0,
            tlv_description: None,
            image_version_hex: None,
            image_random_adjust: None,
        }
    }
}

impl OemInfoImage {
    pub fn from_file(path: &Path) -> Result<Self> {
        let raw =
            fs::read(path).with_context(|| format!("reading OEMINFO image {}", path.display()))?;
        Self::from_bytes(raw)
    }

    pub fn from_bytes(raw: Vec<u8>) -> Result<Self> {
        ensure!(
            raw.len() >= OEMINFO_REUSED_HEADER_SIZE,
            "OEMINFO image is too small: {} bytes",
            raw.len()
        );

        let magic_offsets = find_magic_offsets(&raw);
        let candidate_headers = magic_offsets.len();
        let mut discarded_headers = 0_usize;
        let mut candidates = Vec::with_capacity(candidate_headers);
        for offset in magic_offsets {
            match parse_header(&raw, offset) {
                Some(block) => candidates.push(block),
                None => discarded_headers += 1,
            }
        }

        ensure!(
            !candidates.is_empty(),
            "image contains no valid OEM_INFO block headers"
        );
        candidates.sort_by_key(|block| block.offset);

        let mut blocks = Vec::with_capacity(candidates.len());
        let mut previous_physical_end = 0_usize;
        for block in candidates {
            if block.offset < previous_physical_end {
                discarded_headers += 1;
                continue;
            }
            previous_physical_end = block
                .offset
                .saturating_add(block.header_size)
                .saturating_add(block.length as usize);
            blocks.push(block);
        }

        ensure!(
            !blocks.is_empty(),
            "image contains no non-overlapping OEM_INFO blocks"
        );

        resolve_compact_layouts(&mut blocks);
        let region_size = infer_region_size(raw.len(), &blocks);
        classify_regions_and_active(&mut blocks, region_size);
        classify_payloads(&raw, &mut blocks);

        Ok(Self {
            raw,
            candidate_headers,
            discarded_headers,
            region_size,
            blocks,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    pub fn summary(&self) -> OemInfoImageSummary {
        let blocks = self
            .blocks
            .iter()
            .map(ParsedBlock::summary)
            .collect::<Vec<_>>();
        OemInfoImageSummary {
            file_size: self.raw.len() as u64,
            region_size: self.region_size as u64,
            candidate_headers: self.candidate_headers,
            discarded_headers: self.discarded_headers,
            total_blocks: blocks.len(),
            active_blocks: blocks.iter().filter(|block| block.active).count(),
            inactive_blocks: blocks.iter().filter(|block| !block.active).count(),
            region_a_blocks: blocks
                .iter()
                .filter(|block| block.region == OemInfoRegion::A)
                .count(),
            region_b_blocks: blocks
                .iter()
                .filter(|block| block.region == OemInfoRegion::B)
                .count(),
            unknown_region_blocks: blocks
                .iter()
                .filter(|block| block.region == OemInfoRegion::Unknown)
                .count(),
            standard_blocks: blocks
                .iter()
                .filter(|block| block.layout == OemInfoLayout::Standard)
                .count(),
            compact_blocks: blocks
                .iter()
                .filter(|block| block.layout == OemInfoLayout::StandardCompact)
                .count(),
            reused_blocks: blocks
                .iter()
                .filter(|block| block.layout == OemInfoLayout::Reused)
                .count(),
            blocks,
        }
    }
}

impl ParsedBlock {
    fn summary(&self) -> OemInfoBlockSummary {
        OemInfoBlockSummary {
            offset: self.offset as u64,
            version: self.version,
            id: self.id,
            sub_id: self.sub_id,
            length: self.length,
            age: self.age,
            header_size: self.header_size as u32,
            layout: self.layout,
            region: self.region,
            active: self.active,
            payload_kind: self.payload.kind,
            text_preview: self.payload.text_preview.clone(),
            tlv_parts: self.payload.tlv_parts,
            tlv_description: self.payload.tlv_description.clone(),
            image_version_hex: self.payload.image_version_hex.clone(),
            image_random_adjust: self.payload.image_random_adjust,
            header_padding_byte: self.header_padding_byte,
            block_padding_byte: self.block_padding_byte,
        }
    }

    fn payload_range(&self) -> std::ops::Range<usize> {
        let start = self.offset + self.header_size;
        start..start + self.length as usize
    }
}

pub fn inspect(path: &Path) -> Result<OemInfoImageSummary> {
    Ok(OemInfoImage::from_file(path)?.summary())
}

pub fn read_embedded_image(
    path: &Path,
    block: &OemInfoBlockSummary,
) -> Result<OemInfoEmbeddedImage> {
    read_embedded_image_with_limit(path, block, OEMINFO_MAX_EMBEDDED_IMAGE_SIZE)
}

pub fn read_embedded_image_with_limit(
    path: &Path,
    block: &OemInfoBlockSummary,
    max_image_size: u64,
) -> Result<OemInfoEmbeddedImage> {
    ensure!(
        matches!(
            block.payload_kind,
            OemInfoPayloadKind::ImageRaw | OemInfoPayloadKind::ImageGzip
        ),
        "OEMINFO block {}:{} at 0x{:X} is not an embedded image",
        block.id,
        block.sub_id,
        block.offset
    );

    let expected_header_size = match block.layout {
        OemInfoLayout::Standard | OemInfoLayout::StandardCompact => OEMINFO_STANDARD_HEADER_SIZE,
        OemInfoLayout::Reused => OEMINFO_REUSED_HEADER_SIZE,
    };
    ensure!(
        block.header_size == expected_header_size as u32,
        "selected OEMINFO block layout/header size changed: {} requires 0x{:X}, got 0x{:X}",
        block.layout,
        expected_header_size,
        block.header_size
    );
    if block.layout == OemInfoLayout::Standard {
        ensure!(
            is_aligned_u64(block.offset, OEMINFO_STANDARD_ALIGNMENT as u64),
            "selected STANDARD OEMINFO block is not 0x{:X}-aligned",
            OEMINFO_STANDARD_ALIGNMENT
        );
    }

    let mut file =
        File::open(path).with_context(|| format!("opening OEMINFO image {}", path.display()))?;
    let file_size = file
        .metadata()
        .with_context(|| format!("reading size of OEMINFO image {}", path.display()))?
        .len();
    let payload_offset = block
        .offset
        .checked_add(expected_header_size as u64)
        .context("selected OEMINFO block header range overflows")?;
    ensure!(
        payload_offset <= file_size,
        "selected OEMINFO block header is truncated in {}",
        path.display()
    );

    file.seek(SeekFrom::Start(block.offset))
        .with_context(|| format!("seeking to OEMINFO block at 0x{:X}", block.offset))?;
    let mut header = vec![0_u8; expected_header_size];
    file.read_exact(&mut header)
        .with_context(|| format!("reading OEMINFO block header at 0x{:X}", block.offset))?;
    validate_selected_block_header(&header, block)?;

    ensure!(
        block.length as usize >= IMAGE_DATA_OFFSET + 2,
        "selected OEMINFO image payload is too short: {} bytes",
        block.length
    );
    let image_size = u64::from(block.length) - IMAGE_DATA_OFFSET as u64;
    ensure!(
        image_size <= max_image_size,
        "embedded OEMINFO image is too large: {image_size} bytes exceeds the {} byte limit",
        max_image_size
    );
    let payload_end = payload_offset
        .checked_add(u64::from(block.length))
        .context("selected OEMINFO block payload range overflows")?;
    ensure!(
        payload_end <= file_size,
        "selected OEMINFO block payload is truncated: ends at 0x{payload_end:X}, file is {file_size} bytes"
    );

    let mut image_header = [0_u8; IMAGE_DATA_OFFSET + 2];
    file.read_exact(&mut image_header).with_context(|| {
        format!(
            "reading embedded image header from OEMINFO block at 0x{:X}",
            block.offset
        )
    })?;
    validate_selected_image_header(&image_header, block)?;

    let image_size = usize::try_from(image_size).context("embedded image size is unsupported")?;
    file.seek(SeekFrom::Start(payload_offset + IMAGE_DATA_OFFSET as u64))
        .with_context(|| format!("seeking to embedded image in {}", path.display()))?;
    let mut data = vec![0_u8; image_size];
    file.read_exact(&mut data).with_context(|| {
        format!(
            "reading embedded image from OEMINFO block at 0x{:X}",
            block.offset
        )
    })?;

    Ok(OemInfoEmbeddedImage {
        kind: block.payload_kind,
        data,
    })
}

pub fn export_embedded_image(
    source: &Path,
    block: &OemInfoBlockSummary,
    output: &Path,
) -> Result<()> {
    crate::fs_util::ensure_output_does_not_contain(source, output)?;
    let image = read_embedded_image(source, block)?;
    crate::fs_util::atomic_write(output, "oeminfo-image", |writer| {
        writer
            .write_all(&image.data)
            .with_context(|| format!("writing embedded OEMINFO image to {}", output.display()))?;
        Ok(())
    })
    .with_context(|| format!("exporting embedded OEMINFO image to {}", output.display()))
}

fn validate_selected_block_header(header: &[u8], block: &OemInfoBlockSummary) -> Result<()> {
    ensure!(
        header.len() == block.header_size as usize,
        "selected OEMINFO block header size changed"
    );
    ensure!(
        header.starts_with(OEMINFO_MAGIC),
        "selected OEMINFO block magic changed at 0x{:X}",
        block.offset
    );

    for (name, offset, expected) in [
        ("version", 8, block.version),
        ("id", 12, block.id),
        ("sub-id", 16, block.sub_id),
        ("length", 20, block.length),
        ("age", 24, block.age),
    ] {
        let actual = read_u32(header, offset);
        ensure!(
            actual == expected,
            "selected OEMINFO block {name} changed at 0x{:X}: expected {expected}, got {actual}",
            block.offset
        );
    }
    ensure!(
        block.version != 0 && block.version <= MAX_PLAUSIBLE_VERSION,
        "selected OEMINFO block has invalid version {}",
        block.version
    );

    let padding = match block.layout {
        OemInfoLayout::Standard | OemInfoLayout::StandardCompact => standard_padding(header, 0)
            .context("selected OEMINFO standard block padding/layout changed")?,
        OemInfoLayout::Reused => {
            let field_padding = uniform_byte(&header[28..OEMINFO_HEADER_SIZE]);
            let tail_padding = uniform_byte(&header[OEMINFO_HEADER_SIZE..]);
            tail_padding.or(field_padding).unwrap_or(0)
        }
    };
    ensure!(
        padding == block.header_padding_byte,
        "selected OEMINFO block header padding changed: expected 0x{:02X}, got 0x{padding:02X}",
        block.header_padding_byte
    );
    Ok(())
}

fn validate_selected_image_header(
    image_header: &[u8; IMAGE_DATA_OFFSET + 2],
    block: &OemInfoBlockSummary,
) -> Result<()> {
    let data_offset = read_u32(image_header, 0);
    ensure!(
        data_offset == IMAGE_DATA_OFFSET as u32,
        "embedded OEMINFO image data offset changed: expected 0x{IMAGE_DATA_OFFSET:X}, got 0x{data_offset:X}"
    );
    let end_offset = read_u32(image_header, 4);
    let random_adjust = read_u32(image_header, 8);
    ensure!(
        end_offset.checked_sub(random_adjust) == Some(block.length),
        "embedded OEMINFO image length header is invalid"
    );
    ensure!(
        block.image_random_adjust == Some(random_adjust),
        "embedded OEMINFO image random adjustment changed"
    );
    let version_hex = hex::encode_upper(&image_header[12..24]);
    ensure!(
        block.image_version_hex.as_deref() == Some(version_hex.as_str()),
        "embedded OEMINFO image version changed"
    );

    let actual_kind = match &image_header[24..28] {
        [0, 0, 0x1f, 0x8b] => OemInfoPayloadKind::ImageGzip,
        [0, 0, b'B', b'M'] => OemInfoPayloadKind::ImageRaw,
        _ => anyhow::bail!("embedded OEMINFO image signature changed or is unsupported"),
    };
    ensure!(
        actual_kind == block.payload_kind,
        "embedded OEMINFO image kind changed: expected {}, got {}",
        block.payload_kind,
        actual_kind
    );
    Ok(())
}

pub fn probe_file(path: &Path) -> Result<bool> {
    let mut file = File::open(path)
        .with_context(|| format!("opening possible OEMINFO image {}", path.display()))?;
    let file_size = file
        .metadata()
        .with_context(|| format!("reading size of {}", path.display()))?
        .len();
    if file_size < OEMINFO_REUSED_HEADER_SIZE as u64 {
        return Ok(false);
    }

    let scan_length = file_size.min(PROBE_SCAN_LIMIT);
    let mut buffer = vec![0_u8; PROBE_CHUNK_SIZE + PROBE_OVERLAP];
    let mut consumed = 0_u64;
    let mut overlap = 0_usize;
    while consumed < scan_length {
        let read_length = (scan_length - consumed).min(PROBE_CHUNK_SIZE as u64) as usize;
        file.read_exact(&mut buffer[overlap..overlap + read_length])
            .with_context(|| format!("reading possible OEMINFO image {}", path.display()))?;
        let window_length = overlap + read_length;
        let window_offset = consumed.saturating_sub(overlap as u64);
        if find_magic_offsets(&buffer[..window_length])
            .into_iter()
            .any(|offset| {
                probe_header(
                    &buffer[..window_length],
                    offset,
                    window_offset + offset as u64,
                    file_size,
                )
            })
        {
            return Ok(true);
        }

        overlap = window_length.min(PROBE_OVERLAP);
        buffer.copy_within(window_length - overlap..window_length, 0);
        consumed += read_length as u64;
    }
    Ok(false)
}

fn find_magic_offsets(raw: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut cursor = 0_usize;
    while cursor + OEMINFO_MAGIC.len() <= raw.len() {
        let Some(relative) = raw[cursor..]
            .windows(OEMINFO_MAGIC.len())
            .position(|window| window == OEMINFO_MAGIC)
        else {
            break;
        };
        let offset = cursor + relative;
        offsets.push(offset);
        cursor = offset + OEMINFO_MAGIC.len();
    }
    offsets
}

fn probe_header(prefix: &[u8], offset: usize, absolute_offset: u64, file_size: u64) -> bool {
    if offset + OEMINFO_REUSED_HEADER_SIZE > prefix.len() {
        return false;
    }
    let version = read_u32(prefix, offset + 8);
    let length = read_u32(prefix, offset + 20);
    let age = read_u32(prefix, offset + 24);
    let field_padding = uniform_byte(&prefix[offset + 28..offset + 32]);
    let tail_padding =
        uniform_byte(&prefix[offset + OEMINFO_HEADER_SIZE..offset + OEMINFO_REUSED_HEADER_SIZE]);
    if version == 0
        || version > MAX_PLAUSIBLE_VERSION
        || length == 0
        || age > OEMINFO_MAX_AGE
        || !matches!((field_padding, tail_padding), (Some(field), Some(tail)) if field == tail)
    {
        return false;
    }
    let header_size = if offset + OEMINFO_STANDARD_HEADER_SIZE <= prefix.len()
        && standard_padding(prefix, offset).is_some()
    {
        OEMINFO_STANDARD_HEADER_SIZE
    } else {
        OEMINFO_REUSED_HEADER_SIZE
    };
    absolute_offset
        .checked_add(header_size as u64)
        .and_then(|end| end.checked_add(length as u64))
        .is_some_and(|end| end <= file_size)
}

fn parse_header(raw: &[u8], offset: usize) -> Option<ParsedBlock> {
    if offset.checked_add(OEMINFO_HEADER_SIZE)? > raw.len()
        || &raw[offset..offset + OEMINFO_MAGIC.len()] != OEMINFO_MAGIC
    {
        return None;
    }

    let version = read_u32(raw, offset + 8);
    if version == 0 || version > MAX_PLAUSIBLE_VERSION {
        return None;
    }
    let id = read_u32(raw, offset + 12);
    let sub_id = read_u32(raw, offset + 16);
    let length = read_u32(raw, offset + 20);
    let age = read_u32(raw, offset + 24);
    if length == 0 || age > OEMINFO_MAX_AGE {
        return None;
    }
    let field_padding = uniform_byte(&raw[offset + 28..offset + 32]);
    let short_tail_padding = offset
        .checked_add(OEMINFO_REUSED_HEADER_SIZE)
        .filter(|end| *end <= raw.len())
        .and_then(|end| uniform_byte(&raw[offset + OEMINFO_HEADER_SIZE..end]));
    let derived_padding = short_tail_padding.or(field_padding);

    let standard_padding = standard_padding(raw, offset);
    let (mut header_size, mut layout, header_padding_byte) = match standard_padding {
        Some(padding) => (
            OEMINFO_STANDARD_HEADER_SIZE,
            if is_aligned(offset, OEMINFO_STANDARD_ALIGNMENT) {
                OemInfoLayout::Standard
            } else {
                OemInfoLayout::StandardCompact
            },
            padding,
        ),
        None => (
            OEMINFO_REUSED_HEADER_SIZE,
            OemInfoLayout::Reused,
            derived_padding.unwrap_or(0),
        ),
    };
    let mut payload_end = offset
        .checked_add(header_size)?
        .checked_add(length as usize)?;
    if payload_end > raw.len() && layout != OemInfoLayout::Reused {
        header_size = OEMINFO_REUSED_HEADER_SIZE;
        layout = OemInfoLayout::Reused;
        payload_end = offset
            .checked_add(header_size)?
            .checked_add(length as usize)?;
    }
    if payload_end > raw.len() {
        return None;
    }

    Some(ParsedBlock {
        offset,
        version,
        id,
        sub_id,
        length,
        age,
        header_size,
        layout,
        region: OemInfoRegion::Unknown,
        active: true,
        payload: PayloadDetails::raw(None),
        header_padding_byte,
        block_padding_byte: header_padding_byte,
    })
}

fn standard_padding(raw: &[u8], offset: usize) -> Option<u8> {
    let end = offset.checked_add(OEMINFO_STANDARD_HEADER_SIZE)?;
    if end > raw.len() {
        return None;
    }
    let field_padding = uniform_byte(&raw[offset + 28..offset + 32]);
    let short_tail_padding =
        uniform_byte(&raw[offset + OEMINFO_HEADER_SIZE..offset + OEMINFO_REUSED_HEADER_SIZE]);
    let expected = short_tail_padding.or(field_padding).unwrap_or(0xff);
    raw[offset + OEMINFO_HEADER_SIZE..end]
        .iter()
        .all(|byte| *byte == expected)
        .then_some(expected)
}

fn uniform_byte(bytes: &[u8]) -> Option<u8> {
    let first = *bytes.first()?;
    bytes.iter().all(|byte| *byte == first).then_some(first)
}

fn resolve_compact_layouts(blocks: &mut [ParsedBlock]) {
    for index in 0..blocks.len().saturating_sub(1) {
        if blocks[index].layout != OemInfoLayout::Standard {
            continue;
        }
        let payload_end = blocks[index]
            .offset
            .saturating_add(OEMINFO_STANDARD_HEADER_SIZE)
            .saturating_add(blocks[index].length as usize);
        let aligned_end = align_up(payload_end, OEMINFO_STANDARD_ALIGNMENT);
        if blocks[index + 1].offset < aligned_end {
            blocks[index].layout = OemInfoLayout::StandardCompact;
        }
    }
}

/// Physical copy key the firmware groups generations by. REUSED items carry a
/// real sub-index at +0x10 and are keyed on `(id, sub_id)`. STANDARD items
/// instead store a payload block count (`ceil(length / 512)`) there, which
/// changes when the payload length changes between the A/B generations, so
/// their copies are keyed on `id` alone — otherwise a resized item would split
/// into two groups and both generations would look active.
fn copy_key(block: &ParsedBlock) -> (u32, u32) {
    match block.layout {
        OemInfoLayout::Reused => (block.id, block.sub_id),
        OemInfoLayout::Standard | OemInfoLayout::StandardCompact => (block.id, 0),
    }
}

fn infer_region_size(file_size: usize, blocks: &[ParsedBlock]) -> usize {
    let fallback = file_size.div_ceil(2);
    let minimum_distance = MIN_INFERRED_REGION_SIZE.max(fallback / 2);
    let mut groups: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for block in blocks {
        groups
            .entry(copy_key(block))
            .or_default()
            .push(block.offset);
    }

    let mut distances = HashMap::<usize, usize>::new();
    for offsets in groups.values() {
        for (index, left) in offsets.iter().enumerate() {
            for right in &offsets[index + 1..] {
                let distance = right - left;
                if distance >= minimum_distance
                    && is_aligned(distance, OEMINFO_STANDARD_ALIGNMENT)
                    && distance.saturating_mul(2) <= file_size
                {
                    *distances.entry(distance).or_default() += 1;
                }
            }
        }
    }

    distances
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .max_by_key(|(distance, count)| {
            (
                *count,
                std::cmp::Reverse(distance.abs_diff(fallback)),
                *distance,
            )
        })
        .map_or(fallback, |(distance, _)| distance)
}

fn classify_regions_and_active(blocks: &mut [ParsedBlock], region_size: usize) {
    for block in blocks.iter_mut() {
        let block_end = block
            .offset
            .saturating_add(block.header_size)
            .saturating_add(block.length as usize);
        block.region = if block.offset < region_size && block_end <= region_size {
            OemInfoRegion::A
        } else if block.offset >= region_size && block_end <= region_size.saturating_mul(2) {
            OemInfoRegion::B
        } else {
            OemInfoRegion::Unknown
        };
        block.active = true;
    }

    let mut groups: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (index, block) in blocks.iter().enumerate() {
        if block.region != OemInfoRegion::Unknown {
            groups.entry(copy_key(block)).or_default().push(index);
        }
    }

    for indices in groups.values() {
        let active_index = indices
            .iter()
            .copied()
            .max_by_key(|index| (blocks[*index].age, std::cmp::Reverse(blocks[*index].offset)))
            .expect("OEMINFO group is not empty");
        for index in indices {
            blocks[*index].active = *index == active_index;
        }
    }
}

fn classify_payloads(raw: &[u8], blocks: &mut [ParsedBlock]) {
    for block in blocks {
        let payload = &raw[block.payload_range()];
        block.payload = classify_payload(payload);
        let alignment = if block.layout == OemInfoLayout::Standard {
            OEMINFO_STANDARD_ALIGNMENT
        } else {
            OEMINFO_REUSED_ALIGNMENT
        };
        let payload_end = block.payload_range().end;
        let padding_end = align_up(payload_end, alignment).min(raw.len());
        block.block_padding_byte =
            uniform_byte(&raw[payload_end..padding_end]).unwrap_or(block.header_padding_byte);
    }
}

fn classify_payload(payload: &[u8]) -> PayloadDetails {
    if let Some(image) = classify_image(payload) {
        return image;
    }
    if let Some(parts) = parse_tlv(payload) {
        let first = parts.first().copied().unwrap_or_default();
        let preview = ascii_preview(first).map(|value| value.0);
        let description = parts
            .iter()
            .enumerate()
            .map(|(index, part)| {
                if index == 0 {
                    if is_ascii(part, true) { "ASCII" } else { "RAW" }
                } else if part.len() == SIGNATURE_SIZE
                    && !part.iter().all(|byte| *byte == 0)
                    && !part.iter().all(|byte| *byte == 0xff)
                {
                    "SIGN"
                } else if index + 1 == parts.len() {
                    "RANDOM"
                } else {
                    "PART"
                }
            })
            .collect::<Vec<_>>()
            .join("+");
        return PayloadDetails {
            kind: OemInfoPayloadKind::Tlv,
            text_preview: preview,
            tlv_parts: parts.len(),
            tlv_description: Some(description),
            image_version_hex: None,
            image_random_adjust: None,
        };
    }

    let full_preview = ascii_preview(payload);
    if full_preview.as_ref().is_some_and(|value| value.1) {
        return PayloadDetails {
            kind: OemInfoPayloadKind::Ascii,
            text_preview: full_preview.map(|value| value.0),
            tlv_parts: 0,
            tlv_description: None,
            image_version_hex: None,
            image_random_adjust: None,
        };
    }

    let tail = find_tail_tlv(payload);
    let (mut remaining, mut random_present) = match tail {
        Some((start, _value)) => (&payload[..start], true),
        None => (payload, false),
    };
    if random_present && remaining.len() <= SIGNATURE_SIZE {
        remaining = payload;
        random_present = false;
    }

    if remaining.len() > SIGNATURE_SIZE {
        let (data, signature) = remaining.split_at(remaining.len() - SIGNATURE_SIZE);
        let data_preview = ascii_preview(data);
        let data_is_ascii = data_preview.is_some();
        let data_is_strict_ascii = data_preview.as_ref().is_some_and(|value| value.1);
        let data_is_high_entropy = !data_is_ascii && high_entropy(data);
        let signature_is_high_entropy = high_entropy(signature);
        let has_signature = if random_present {
            true
        } else if !data_is_ascii && data_is_high_entropy {
            false
        } else {
            signature_is_high_entropy
        };

        if has_signature {
            let kind = match (data_is_strict_ascii, random_present) {
                (true, true) => OemInfoPayloadKind::AsciiSignedRandom,
                (true, false) => OemInfoPayloadKind::AsciiSigned,
                (false, true) => OemInfoPayloadKind::RawSignedRandom,
                (false, false) => OemInfoPayloadKind::RawSigned,
            };
            return PayloadDetails {
                kind,
                text_preview: data_preview.map(|value| value.0),
                tlv_parts: 0,
                tlv_description: None,
                image_version_hex: None,
                image_random_adjust: None,
            };
        }
    }

    let preview = full_preview
        .map(|value| value.0)
        .or_else(|| ascii_with_padding_only(payload).then(|| sanitize_preview(payload)));
    PayloadDetails::raw(preview)
}

fn classify_image(payload: &[u8]) -> Option<PayloadDetails> {
    if payload.len() < 28 || read_u32(payload, 0) as usize != IMAGE_DATA_OFFSET {
        return None;
    }
    let end_offset = read_u32(payload, 4) as usize;
    let random_adjust = read_u32(payload, 8) as usize;
    if end_offset.checked_sub(random_adjust)? != payload.len() {
        return None;
    }
    let kind = match &payload[24..28] {
        [0, 0, 0x1f, 0x8b] => OemInfoPayloadKind::ImageGzip,
        [0, 0, b'B', b'M'] => OemInfoPayloadKind::ImageRaw,
        _ => return None,
    };
    Some(PayloadDetails {
        kind,
        text_preview: None,
        tlv_parts: 0,
        tlv_description: None,
        image_version_hex: Some(hex::encode_upper(&payload[12..24])),
        image_random_adjust: Some(random_adjust as u32),
    })
}

fn parse_tlv(data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut parts = Vec::new();
    let mut cursor = 0_usize;
    while cursor < data.len() {
        if matches!(data[cursor], 0 | 0xff)
            && data[cursor..].iter().all(|byte| matches!(*byte, 0 | 0xff))
        {
            break;
        }
        let search_end = (cursor + 4).min(data.len());
        let null = data[cursor..search_end]
            .iter()
            .position(|byte| *byte == 0)
            .map(|relative| cursor + relative)?;
        let digits = &data[cursor..null];
        if digits.is_empty() || digits.len() > 3 || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let length = parse_decimal(digits)?;
        let start = null + 1;
        let end = start.checked_add(length)?;
        if end > data.len() {
            return None;
        }
        parts.push(&data[start..end]);
        cursor = end;
    }
    if parts.is_empty() || !data[cursor..].iter().all(|byte| matches!(*byte, 0 | 0xff)) {
        None
    } else {
        Some(parts)
    }
}

fn find_tail_tlv(data: &[u8]) -> Option<(usize, &[u8])> {
    if data.len() < 3 {
        return None;
    }
    for null in (1..data.len()).rev().filter(|index| data[*index] == 0) {
        let mut start = null;
        while start > 0 && null - start < 3 && data[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == null || (start > 0 && data[start - 1].is_ascii_digit()) {
            continue;
        }
        let length = parse_decimal(&data[start..null])?;
        let value_start = null + 1;
        if length == data.len() - value_start {
            return Some((start, &data[value_start..]));
        }
    }
    None
}

fn parse_decimal(digits: &[u8]) -> Option<usize> {
    digits.iter().try_fold(0_usize, |value, digit| {
        value.checked_mul(10)?.checked_add((digit - b'0') as usize)
    })
}

fn ascii_preview(data: &[u8]) -> Option<(String, bool)> {
    if !is_ascii(data, false) {
        return None;
    }
    Some((sanitize_preview(data), is_ascii(data, true)))
}

fn is_ascii(data: &[u8], strict: bool) -> bool {
    if data.is_empty() {
        return false;
    }
    let invalid = data
        .iter()
        .filter(|byte| {
            let allowed = matches!(**byte, 0x20..=0x7e | b'\t' | b'\n' | b'\r');
            !(allowed || (!strict && matches!(**byte, 0 | 0xff)))
        })
        .count();
    invalid * 100 <= data.len() * 5
}

fn ascii_with_padding_only(data: &[u8]) -> bool {
    !data.is_empty()
        && data
            .iter()
            .all(|byte| matches!(*byte, 0x20..=0x7e | b'\t' | b'\n' | b'\r' | 0 | 0xff))
}

fn sanitize_preview(data: &[u8]) -> String {
    let mut output = String::with_capacity(data.len());
    for byte in data {
        if matches!(*byte, 0x20..=0x7e | b'\t') {
            output.push(*byte as char);
        } else {
            use fmt::Write as _;
            let _ = write!(output, "\\x{byte:02x}");
        }
    }
    output
}

fn high_entropy(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let mut counts = [0_usize; 256];
    for byte in data {
        counts[*byte as usize] += 1;
    }
    let length = data.len() as f64;
    let entropy = counts
        .into_iter()
        .filter(|count| *count != 0)
        .map(|count| {
            let probability = count as f64 / length;
            -probability * probability.log2()
        })
        .sum::<f64>();
    entropy >= HIGH_ENTROPY_THRESHOLD
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .expect("u32 bounds checked"),
    )
}

fn align_up(value: usize, alignment: usize) -> usize {
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded / alignment * alignment)
        .unwrap_or(usize::MAX)
}

#[allow(clippy::manual_is_multiple_of)]
fn is_aligned(value: usize, alignment: usize) -> bool {
    value % alignment == 0
}

#[allow(clippy::manual_is_multiple_of)]
fn is_aligned_u64(value: u64, alignment: u64) -> bool {
    value % alignment == 0
}
