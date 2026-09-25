use anyhow::{Context, Result, ensure};
use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const OEMINFO_MAGIC: &[u8; 8] = b"OEM_INFO";
pub const OEMINFO_HEADER_SIZE: usize = 0x20;
pub const OEMINFO_REUSED_HEADER_SIZE: usize = 0x40;
pub const OEMINFO_STANDARD_HEADER_SIZE: usize = 0x200;
pub const OEMINFO_STANDARD_ALIGNMENT: usize = 0x1000;
pub const OEMINFO_REUSED_ALIGNMENT: usize = 0x80;
pub const OEMINFO_MAX_EMBEDDED_IMAGE_SIZE: u64 = 256 * 1024 * 1024;
pub const OEMINFO_MAX_AGE: u32 = 0x3B9AC9FF;
pub const OEMINFO_BOOT_LOGO_ID: u32 = 4501;
const IMAGE_END_ALIGNMENT: u32 = 16;
const MAX_LOGO_SIDE: u32 = 8192;
const MAX_LOGO_DECODE_ALLOCATION: u64 = 384 * 1024 * 1024;

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OemInfoOverview {
    pub base_version: Option<String>,
    pub full_version: Option<String>,
    pub product_model: Option<String>,
    pub cust_version: Option<String>,
    pub preload_version: Option<String>,
    pub base_component: Option<String>,
    pub device_certificate: Option<bool>,
    pub other_versions: Vec<(u32, u32, String)>,
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

/// Distills well-known identity/version blocks from a parsed image summary.
///
/// Only active copies are considered. Every field stays `None` when its
/// block is absent or carries no usable text, so callers can always render
/// every row. Component strings keep their full recorded form, e.g.
/// `TAS-AN00-CUST 104.2.0.136(C00)` and `TAS-LGRP3-CHN 104.2.0.136`.
pub fn overview(summary: &OemInfoImageSummary) -> OemInfoOverview {
    let mut result = OemInfoOverview::default();
    let mut other_versions = Vec::new();
    for block in summary.blocks.iter().filter(|block| block.active) {
        let text = block
            .text_preview
            .as_deref()
            .map(truncate_at_escape)
            .map(str::trim)
            .filter(|text| !text.is_empty());
        match (block.id, block.sub_id) {
            (2601 | 2603, _) => result.device_certificate = Some(true),
            (1516, 1) if text.is_some() => {
                result.base_version = text.map(str::to_owned);
            }
            (1518, 1) if text.is_some() => {
                result.product_model = text.map(str::to_owned);
            }
            (1101, 4) if text.is_some() && result.full_version.is_none() => {
                result.full_version = text.map(str::to_owned);
            }
            (80, 1) if text.is_some() => {
                result.cust_version = text.map(str::to_owned);
            }
            (82, 1) if text.is_some() => {
                result.preload_version = text.map(str::to_owned);
            }
            (86, 1) if text.is_some() => {
                result.base_component = text.map(str::to_owned);
            }
            _ => {
                if let Some(text) = text.filter(|text| looks_like_version(text)) {
                    other_versions.push((block.id, block.sub_id, text.to_owned()));
                }
            }
        }
    }
    result.other_versions = other_versions;
    result
}

/// Cuts a sanitized payload preview at the first non-printable escape
/// (`\x00`, `\xff`, …) so trailing padding does not reach the overview.
fn truncate_at_escape(preview: &str) -> &str {
    match preview.find("\\x") {
        Some(index) => &preview[..index],
        None => preview,
    }
}

fn looks_like_version(text: &str) -> bool {
    let trimmed = text.trim();
    (3..=64).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|char| char.is_ascii_graphic() || char == ' ')
        && trimmed.split(['(', ')', ' ', ';', '|', ',']).any(|token| {
            let parts = token.split('.').collect::<Vec<_>>();
            parts.len() >= 2
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.chars().all(|char| char.is_ascii_digit()))
        })
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OemInfoBootLogoReplacement {
    pub backup_path: String,
    pub target_offset: u64,
    pub age: u32,
    pub payload_bytes: u64,
    pub payload_kind: OemInfoPayloadKind,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmpInfo {
    pub width: u32,
    pub height: u32,
    pub top_down: bool,
    pub bits_per_pixel: u16,
    pub compression: u32,
    pub data_offset: usize,
}

pub fn bmp_info(data: &[u8]) -> Option<BmpInfo> {
    if data.len() < 34 || !data.starts_with(b"BM") || read_u32(data, 14) < 40 {
        return None;
    }
    let width = read_u32(data, 18);
    let signed_height = read_u32(data, 22) as i32;
    if width == 0 || width > MAX_LOGO_SIDE || signed_height == 0 {
        return None;
    }
    let height = signed_height.unsigned_abs();
    if height > MAX_LOGO_SIDE {
        return None;
    }
    Some(BmpInfo {
        width,
        height,
        top_down: signed_height < 0,
        bits_per_pixel: u16::from_le_bytes([data[28], data[29]]),
        compression: read_u32(data, 30),
        data_offset: read_u32(data, 10) as usize,
    })
}

pub fn decode_rgb565_bmp(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let info = bmp_info(data).context("parsing BMP header of embedded 16-bpp image")?;
    ensure!(
        info.bits_per_pixel == 16 && info.compression == 0,
        "embedded image is not an uncompressed 16-bpp BMP"
    );
    let width = info.width as usize;
    let height = info.height as usize;
    let stride = width
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_next_multiple_of(4))
        .context("16-bpp BMP row size overflows")?;
    let pixel_bytes = stride
        .checked_mul(height)
        .context("16-bpp BMP pixel data size overflows")?;
    let data_end = info
        .data_offset
        .checked_add(pixel_bytes)
        .context("16-bpp BMP data range overflows")?;
    ensure!(
        data_end <= data.len(),
        "16-bpp BMP pixel data is truncated: needs 0x{data_end:X} bytes, file has {}",
        data.len()
    );
    let allocation = u64::try_from(width * height * 4).expect("bounded by MAX_LOGO_SIDE");
    ensure!(
        allocation <= MAX_LOGO_DECODE_ALLOCATION,
        "16-bpp BMP decode would allocate {allocation} bytes"
    );

    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let source_row = if info.top_down { row } else { height - 1 - row };
        let row_start = info.data_offset + source_row * stride;
        for column in 0..width {
            let pixel = u16::from_le_bytes([
                data[row_start + column * 2],
                data[row_start + column * 2 + 1],
            ]);
            let red = (pixel >> 11) & 0x1f;
            let green = (pixel >> 5) & 0x3f;
            let blue = pixel & 0x1f;
            rgba.extend_from_slice(&[
                ((red << 3) | (red >> 2)) as u8,
                ((green << 2) | (green >> 4)) as u8,
                ((blue << 3) | (blue >> 2)) as u8,
                0xff,
            ]);
        }
    }
    Ok((rgba, info.width, info.height))
}

fn encode_rgb565_bmp(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        rgba.len() as u64 == u64::from(width) * u64::from(height) * 4,
        "pixel buffer does not match {width}x{height} RGBA"
    );
    ensure!(
        width <= MAX_LOGO_SIDE && height <= MAX_LOGO_SIDE,
        "replacement logo exceeds {MAX_LOGO_SIDE}x{MAX_LOGO_SIDE} pixels"
    );
    let stride = (width as usize * 2).next_multiple_of(4);
    let pixel_bytes = stride
        .checked_mul(height as usize)
        .context("encoded BMP pixel data size overflows")?;
    let file_size = 0x36 + pixel_bytes;
    ensure!(
        file_size <= u32::MAX as usize,
        "encoded BMP exceeds the 4 GiB BMP size limit"
    );

    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0x36u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&(-(height as i32)).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&16u16.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    bmp.extend_from_slice(&[0u8; 16]);

    let mut row = vec![0_u8; stride];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let pixel_offset = (y * width as usize + x) * 4;
            let pixel = (u16::from(rgba[pixel_offset]) >> 3) << 11
                | (u16::from(rgba[pixel_offset + 1]) >> 2) << 5
                | u16::from(rgba[pixel_offset + 2]) >> 3;
            row[x * 2..x * 2 + 2].copy_from_slice(&pixel.to_le_bytes());
        }
        bmp.extend_from_slice(&row);
    }
    Ok(bmp)
}

fn transcode_replacement_bmp(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    ensure!(
        bmp_info(data).is_some(),
        "replacement file is not a supported BMP"
    );
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_LOGO_SIDE);
    limits.max_image_height = Some(MAX_LOGO_SIDE);
    limits.max_alloc = Some(MAX_LOGO_DECODE_ALLOCATION);
    let mut reader = image::ImageReader::with_format(Cursor::new(data), image::ImageFormat::Bmp);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .context("decoding replacement BMP image data")?;
    let (width, height) = (decoded.width(), decoded.height());
    let rgba = decoded.into_rgba8().into_raw();
    let bmp = encode_rgb565_bmp(width, height, &rgba)?;
    Ok((bmp, width, height))
}

fn decompress_logo_gzip(data: Vec<u8>, limit: u64) -> Result<Vec<u8>> {
    let decoder = MultiGzDecoder::new(Cursor::new(data));
    let mut limited = decoder.take(limit.saturating_add(1));
    let mut decoded = Vec::new();
    limited
        .read_to_end(&mut decoded)
        .context("decompressing embedded OEMINFO image")?;
    ensure!(
        decoded.len() as u64 <= limit,
        "decompressed embedded OEMINFO image exceeds {limit} bytes"
    );
    Ok(decoded)
}

fn read_logo_dimensions(raw: &[u8], block: &ParsedBlock) -> Result<(u32, u32)> {
    let payload = &raw[block.payload_range()];
    let bmp = match block.payload.kind {
        OemInfoPayloadKind::ImageGzip => decompress_logo_gzip(
            payload[IMAGE_DATA_OFFSET..].to_vec(),
            OEMINFO_MAX_EMBEDDED_IMAGE_SIZE,
        )?,
        OemInfoPayloadKind::ImageRaw => payload[IMAGE_DATA_OFFSET..].to_vec(),
        kind => anyhow::bail!(
            "OEMINFO block 4501 at 0x{:X} is not an image (found {kind})",
            block.offset
        ),
    };
    let info = bmp_info(&bmp).ok_or_else(|| {
        anyhow::anyhow!(
            "embedded boot logo at 0x{:X} is not a BITMAPINFOHEADER BMP",
            block.offset
        )
    })?;
    Ok((info.width, info.height))
}

pub fn replace_boot_logo(
    path: &Path,
    replacement_bmp: &Path,
) -> Result<OemInfoBootLogoReplacement> {
    let replacement_raw = fs::read(replacement_bmp)
        .with_context(|| format!("reading replacement BMP {}", replacement_bmp.display()))?;
    let (replacement_encoded, width, height) = transcode_replacement_bmp(&replacement_raw)?;

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening OEMINFO image for editing {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading metadata of {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "OEMINFO logo replacement only supports regular files; dump the partition to an image file first"
    );
    fs2::FileExt::lock_exclusive(&file)
        .with_context(|| format!("locking OEMINFO image for editing {}", path.display()))?;

    file.seek(SeekFrom::Start(0))?;
    let mut original = Vec::new();
    file.read_to_end(&mut original)
        .with_context(|| format!("reading locked OEMINFO image {}", path.display()))?;
    let image = OemInfoImage::from_bytes(original.clone())?;
    let raw = image.as_bytes();
    let region_size = image.region_size;

    let logo_blocks = image
        .blocks
        .iter()
        .filter(|block| block.id == OEMINFO_BOOT_LOGO_ID)
        .collect::<Vec<_>>();
    ensure!(
        logo_blocks.len() <= 2,
        "OEMINFO image contains {} blocks with id {OEMINFO_BOOT_LOGO_ID}; at most two (one per bank) are expected",
        logo_blocks.len()
    );
    ensure!(
        !logo_blocks.is_empty(),
        "OEMINFO image contains no boot logo block {OEMINFO_BOOT_LOGO_ID}"
    );
    for block in &logo_blocks {
        ensure!(
            block.header_size == OEMINFO_STANDARD_HEADER_SIZE
                && matches!(
                    block.payload.kind,
                    OemInfoPayloadKind::ImageRaw | OemInfoPayloadKind::ImageGzip
                ),
            "boot logo block 4501 at 0x{:X} has an unexpected layout ({}, {})",
            block.offset,
            block.layout,
            block.payload.kind
        );
    }
    let source = logo_blocks
        .iter()
        .find(|block| block.active)
        .context("OEMINFO boot logo block has no active copy")?;
    ensure!(
        logo_blocks.iter().filter(|block| block.active).count() == 1,
        "OEMINFO boot logo block has multiple active copies; the image layout is ambiguous"
    );

    let (logo_width, logo_height) = read_logo_dimensions(raw, source)?;
    ensure!(
        (logo_width, logo_height) == (width, height),
        "replacement BMP is {width}x{height}, but the active boot logo is {logo_width}x{logo_height}; the pixel dimensions must match"
    );

    let source_payload = &raw[source.payload_range()];
    let image_version = source_payload[12..24].to_vec();
    let payload_data = match source.payload.kind {
        OemInfoPayloadKind::ImageGzip => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
            encoder
                .write_all(&replacement_encoded)
                .context("compressing replacement boot logo")?;
            encoder
                .finish()
                .context("finishing replacement boot logo compression")?
        }
        _ => replacement_encoded,
    };
    let payload_len = IMAGE_DATA_OFFSET + payload_data.len();
    ensure!(
        payload_len <= u32::MAX as usize,
        "replacement logo payload exceeds 4 GiB"
    );
    let end_offset = (payload_len as u32).next_multiple_of(IMAGE_END_ALIGNMENT);
    let random_adjust = end_offset - payload_len as u32;
    let sub_id = payload_len.div_ceil(OEMINFO_HEADER_SIZE * 16) as u32;
    let new_age = image
        .blocks
        .iter()
        .filter(|block| block.id == OEMINFO_BOOT_LOGO_ID)
        .map(|block| block.age)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .filter(|age| *age <= OEMINFO_MAX_AGE)
        .context("OEMINFO boot logo generation counter overflow")?;
    let target_offset = match logo_blocks.iter().find(|block| !block.active) {
        Some(block) => block.offset,
        None => {
            let mirror = if source.offset < region_size {
                source.offset + region_size
            } else {
                source
                    .offset
                    .checked_sub(region_size)
                    .context("computing the mirrored OEMINFO bank offset")?
            };
            ensure!(
                mirror != source.offset,
                "OEMINFO banks overlap; cannot compute a rotation slot for the boot logo"
            );
            ensure!(
                is_aligned(mirror, OEMINFO_STANDARD_ALIGNMENT),
                "mirrored boot logo slot 0x{mirror:X} is not 0x{:X}-aligned",
                OEMINFO_STANDARD_ALIGNMENT
            );
            mirror
        }
    };

    let target = target_offset as usize;
    let write_end_payload = align_up(
        target + OEMINFO_STANDARD_HEADER_SIZE + payload_len,
        OEMINFO_STANDARD_ALIGNMENT,
    );
    let mut write_end = write_end_payload;
    if let Some(previous) = image
        .blocks
        .iter()
        .find(|block| block.offset == target_offset)
    {
        write_end = write_end.max(align_up(
            target + previous.header_size + previous.length as usize,
            OEMINFO_STANDARD_ALIGNMENT,
        ));
    }
    let bank_end = if target < region_size {
        region_size
    } else {
        region_size.saturating_mul(2).min(original.len())
    };
    let next_block = image
        .blocks
        .iter()
        .filter(|block| block.offset > target)
        .map(|block| block.offset)
        .min()
        .unwrap_or(usize::MAX);
    let available_end = next_block.min(bank_end).min(original.len());
    ensure!(
        write_end <= available_end,
        "replacement boot logo needs 0x{:X} bytes at 0x{target:X} but only 0x{:X} are free before the next block",
        write_end - target,
        available_end.saturating_sub(target)
    );
    for block in &image.blocks {
        if block.offset == target {
            continue;
        }
        let block_end = block.offset + block.header_size + block.length as usize;
        ensure!(
            block.offset >= write_end || block_end <= target,
            "rotation slot 0x{target:X} overlaps OEMINFO block {}:{} at 0x{:X}",
            block.id,
            block.sub_id,
            block.offset
        );
    }

    let mut block_bytes = vec![0xff_u8; write_end - target];
    block_bytes[0..8].copy_from_slice(OEMINFO_MAGIC);
    block_bytes[8..12].copy_from_slice(&source.version.to_le_bytes());
    block_bytes[12..16].copy_from_slice(&OEMINFO_BOOT_LOGO_ID.to_le_bytes());
    block_bytes[16..20].copy_from_slice(&sub_id.to_le_bytes());
    block_bytes[20..24].copy_from_slice(&(payload_len as u32).to_le_bytes());
    block_bytes[24..28].copy_from_slice(&new_age.to_le_bytes());
    let payload_start = OEMINFO_STANDARD_HEADER_SIZE;
    block_bytes[payload_start..payload_start + 4]
        .copy_from_slice(&(IMAGE_DATA_OFFSET as u32).to_le_bytes());
    block_bytes[payload_start + 4..payload_start + 8].copy_from_slice(&end_offset.to_le_bytes());
    block_bytes[payload_start + 8..payload_start + 12]
        .copy_from_slice(&random_adjust.to_le_bytes());
    block_bytes[payload_start + 12..payload_start + 24].copy_from_slice(&image_version);
    block_bytes[payload_start + 24..payload_start + IMAGE_DATA_OFFSET].fill(0);
    block_bytes[payload_start + IMAGE_DATA_OFFSET..payload_start + payload_len]
        .copy_from_slice(&payload_data);

    let backup_path =
        crate::fs_util::create_backup(path, &original, metadata.permissions(), "OEMINFO")
            .with_context(|| format!("backing up OEMINFO image {}", path.display()))?;

    let header_end = (target + OEMINFO_STANDARD_HEADER_SIZE) as u64;
    let write_result = (|| -> Result<()> {
        write_at(
            &mut file,
            header_end,
            &block_bytes[OEMINFO_STANDARD_HEADER_SIZE..],
        )?;
        file.sync_all().with_context(|| {
            format!(
                "flushing replacement boot logo payload in {}",
                path.display()
            )
        })?;
        write_at(
            &mut file,
            target as u64,
            &block_bytes[..OEMINFO_STANDARD_HEADER_SIZE],
        )?;
        file.sync_all().with_context(|| {
            format!(
                "committing replacement boot logo header in {}",
                path.display()
            )
        })?;

        let mut on_disk = vec![0_u8; block_bytes.len()];
        read_at(&mut file, target as u64, &mut on_disk)?;
        ensure!(
            on_disk == block_bytes,
            "replacement boot logo block failed write verification"
        );
        Ok(())
    })();
    if let Err(error) = write_result {
        let restore_end = write_end.min(original.len());
        let _ = write_at(&mut file, target as u64, &original[target..restore_end]);
        let _ = file.sync_all();
        return Err(error);
    }

    let mut committed = vec![0_u8; original.len()];
    read_at(&mut file, 0, &mut committed)?;
    let reparsed = OemInfoImage::from_bytes(committed)?;
    let active = reparsed
        .blocks
        .iter()
        .find(|block| block.id == OEMINFO_BOOT_LOGO_ID && block.active)
        .context("replacement boot logo block did not become the active generation")?;
    ensure!(
        active.offset == target && active.age == new_age,
        "replacement boot logo verification mismatch at 0x{:X} (age {})",
        active.offset,
        active.age
    );
    let (active_width, active_height) = read_logo_dimensions(reparsed.as_bytes(), active)?;
    ensure!(
        (active_width, active_height) == (width, height),
        "replacement boot logo changed pixel dimensions"
    );

    Ok(OemInfoBootLogoReplacement {
        backup_path: backup_path.display().to_string(),
        target_offset: target as u64,
        age: new_age,
        payload_bytes: payload_len as u64,
        payload_kind: active.payload.kind,
        width,
        height,
    })
}

fn write_at(file: &mut File, offset: u64, data: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    Ok(())
}

fn read_at(file: &mut File, offset: u64, data: &mut [u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(data)?;
    Ok(())
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
    let tail = &raw[offset + OEMINFO_HEADER_SIZE..end];
    if tail.iter().all(|byte| *byte == expected) {
        return Some(expected);
    }
    // HarmonyOS NEXT writers zero the reserved fields (+0x20..+0x2c) and pad
    // the long tail with 0xff, producing a two-segment uniform pattern that
    // the single-byte check above rejects. Accept it as STANDARD padding; a
    // genuine REUSED record carries payload data past +0x40 instead.
    let zero_prefix = tail
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(tail.len());
    let padding_tail = &tail[zero_prefix..];
    (!padding_tail.is_empty() && zero_prefix <= 0x10)
        .then(|| uniform_byte(padding_tail).filter(|byte| *byte == 0xff))
        .flatten()
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
