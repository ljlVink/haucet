// Compact index encoding follows erofs-utils lib/compress.c (GPL-2.0+ OR MIT),
// Copyright (C) 2018-2019 HUAWEI, Inc., by Gao Xiang.
use std::ffi::{c_char, c_int, c_void};
use std::fmt;
use std::io::{Read, Seek, Write};
use std::ptr::NonNull;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};

use crate::erofs_fs::{
    Z_EROFS_ADVISE_BIG_PCLUSTER_1, Z_EROFS_ADVISE_BIG_PCLUSTER_2, Z_EROFS_ADVISE_COMPACTED_2B,
    Z_EROFS_LCLUSTER_TYPE_HEAD1, Z_EROFS_LCLUSTER_TYPE_NONHEAD, Z_EROFS_LCLUSTER_TYPE_PLAIN,
    Z_EROFS_LI_D0_CBLKCNT, Z_EROFS_PCLUSTER_MAX_DSIZE, Z_EROFS_PCLUSTER_MAX_SIZE,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Compression {
    None,
    Lz4,
    Lz4Hc { level: i32 },
}

impl Default for Compression {
    fn default() -> Self {
        Self::Lz4Hc { level: 9 }
    }
}

impl FromStr for Compression {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (name, level) = value
            .split_once(',')
            .map_or((value, None), |(name, level)| (name, Some(level)));
        match (name, level) {
            ("none", None) => Ok(Self::None),
            ("lz4", None) => Ok(Self::Lz4),
            ("lz4hc", level) => {
                let level = level
                    .map(str::parse::<i32>)
                    .transpose()
                    .context("invalid LZ4HC compression level")?
                    .unwrap_or(9);
                ensure!(
                    (1..=12).contains(&level),
                    "LZ4HC level must be 1 through 12"
                );
                Ok(Self::Lz4Hc { level })
            }
            _ => bail!("unsupported compression {value:?}; use none, lz4, or lz4hc[,1-12]"),
        }
    }
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Lz4 => f.write_str("lz4"),
            Self::Lz4Hc { level } => write!(f, "lz4hc,{level}"),
        }
    }
}

pub struct CompressedFile {
    pub metadata: Vec<u8>,
    pub original_size: u64,
    pub physical_size: u64,
    pub compressed_blocks: u32,
    pub used_compression: bool,
}

impl CompressedFile {
    pub fn compact_indexes(&mut self, inode_header_size: usize, block_size: u32) -> Result<()> {
        let indexes = &self.metadata[16..];
        let total = indexes.len() / 8;
        let big = u16::from_le_bytes(self.metadata[4..6].try_into().unwrap())
            & Z_EROFS_ADVISE_BIG_PCLUSTER_1
            != 0;
        let advise = Z_EROFS_ADVISE_COMPACTED_2B
            | if big {
                Z_EROFS_ADVISE_BIG_PCLUSTER_1 | Z_EROFS_ADVISE_BIG_PCLUSTER_2
            } else {
                0
            };
        let mut output = self.metadata[..8].to_vec();
        output[4..6].copy_from_slice(&advise.to_le_bytes());
        let mut block = u32::from_le_bytes(indexes[4..8].try_into().unwrap());
        let mut dummy_head = !big;
        if !big {
            block = block.wrapping_sub(1);
        }
        let lobits = block_size.trailing_zeros().max(12);
        for (index, count) in compact_index_packs(total, inode_header_size) {
            let pack_size = if count == 16 { 32 } else { 8 };
            let encodebits = (pack_size - 4) * 8 / count;
            let mut pack = vec![0u8; pack_size];
            let mut base_block = block;
            let mut update_base = big;
            for slot in 0..count {
                let zero = [0; 8];
                let record = indexes
                    .get((index + slot) * 8..(index + slot + 1) * 8)
                    .unwrap_or(&zero);
                let kind = u16::from_le_bytes(record[..2].try_into().unwrap()) as u8;
                let offset = if kind == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
                    let back = u16::from_le_bytes(record[4..6].try_into().unwrap());
                    if back & Z_EROFS_LI_D0_CBLKCNT != 0 {
                        block = block
                            .checked_add((back & !Z_EROFS_LI_D0_CBLKCNT) as u32)
                            .context("compact EROFS block address overflow")?;
                        dummy_head = false;
                        back
                    } else if slot + 1 == count {
                        u16::from_le_bytes(record[6..8].try_into().unwrap())
                            .min(Z_EROFS_LI_D0_CBLKCNT - 1)
                    } else {
                        back
                    }
                } else {
                    if dummy_head {
                        block = block.wrapping_add(1);
                        if update_base {
                            base_block = block;
                        }
                    }
                    dummy_head = true;
                    update_base = false;
                    let stored = u32::from_le_bytes(record[4..8].try_into().unwrap());
                    ensure!(
                        stored == block || (stored == 0 && index + slot + 1 >= total),
                        "non-contiguous physical clusters in compact EROFS indexes"
                    );
                    u16::from_le_bytes(record[2..4].try_into().unwrap())
                };
                ensure!(
                    (offset as u32) < 1 << lobits,
                    "compact EROFS index offset overflow"
                );
                let value = ((kind as u32) << lobits) | offset as u32;
                let bit = slot * encodebits;
                for n in 0..encodebits {
                    pack[(bit + n) / 8] |= (((value >> n) & 1) as u8) << ((bit + n) % 8);
                }
            }
            pack[pack_size - 4..].copy_from_slice(&base_block.to_le_bytes());
            output.extend_from_slice(&pack);
        }
        self.metadata = output;
        Ok(())
    }
}

fn compact_index_packs(
    total: usize,
    inode_header_size: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let map_start = inode_header_size.div_ceil(8) * 8 + 8;
    let initial = ((32 - map_start % 32) / 4) & 7;
    let initial = if initial > total { 0 } else { initial };
    let compact = (total - initial) / 16 * 16;
    let mut index = 0;
    std::iter::from_fn(move || {
        if index >= total {
            return None;
        }
        let count = if index >= initial && index < initial + compact {
            16
        } else {
            2
        };
        let start = index;
        index += count;
        Some((start, count))
    })
}

pub fn relocate_indexes(
    metadata: &mut [u8],
    inode_header_size: usize,
    total_indexes: usize,
    compact: bool,
    shift: u32,
) -> Result<()> {
    let relocate = |bytes: &mut [u8]| -> Result<()> {
        let block = u32::from_le_bytes(bytes.try_into().unwrap());
        bytes.copy_from_slice(
            &block
                .checked_add(shift)
                .context("EROFS index address overflow")?
                .to_le_bytes(),
        );
        Ok(())
    };
    if !compact {
        for index in metadata[16..].chunks_exact_mut(8) {
            if index[0] != Z_EROFS_LCLUSTER_TYPE_NONHEAD && index[4..8] != [0; 4] {
                relocate(&mut index[4..8])?;
            }
        }
    } else {
        let mut offset = 8;
        for (_, count) in compact_index_packs(total_indexes, inode_header_size) {
            let size = if count == 16 { 32 } else { 8 };
            relocate(&mut metadata[offset + size - 4..offset + size])?;
            offset += size;
        }
        ensure!(
            offset == metadata.len(),
            "invalid compact EROFS index length"
        );
    }
    Ok(())
}

unsafe extern "C" {
    fn LZ4_compress_destSize(
        src: *const c_char,
        dst: *mut c_char,
        src_size: *mut c_int,
        dst_capacity: c_int,
    ) -> c_int;
    fn LZ4_createStreamHC() -> *mut c_void;
    fn LZ4_freeStreamHC(stream: *mut c_void) -> c_int;
    fn LZ4_compress_HC_destSize(
        state: *mut c_void,
        src: *const c_char,
        dst: *mut c_char,
        src_size: *mut c_int,
        dst_capacity: c_int,
        level: c_int,
    ) -> c_int;
}

struct Compressor {
    algorithm: Compression,
    hc_state: Option<NonNull<c_void>>,
}

impl Compressor {
    fn new(algorithm: Compression) -> Result<Self> {
        let hc_state = if let Compression::Lz4Hc { level } = algorithm {
            ensure!(
                (1..=12).contains(&level),
                "LZ4HC level must be 1 through 12"
            );
            Some(NonNull::new(unsafe { LZ4_createStreamHC() }).context("allocating LZ4HC state")?)
        } else {
            None
        };
        Ok(Self {
            algorithm,
            hc_state,
        })
    }

    fn compress(&mut self, input: &[u8], output: &mut [u8]) -> Result<(usize, usize)> {
        let mut consumed = c_int::try_from(input.len()).context("LZ4 input is too large")?;
        let capacity = c_int::try_from(output.len()).context("LZ4 output is too large")?;
        let written = unsafe {
            match self.algorithm {
                Compression::None => return Ok((0, 0)),
                Compression::Lz4 => LZ4_compress_destSize(
                    input.as_ptr().cast(),
                    output.as_mut_ptr().cast(),
                    &mut consumed,
                    capacity,
                ),
                Compression::Lz4Hc { level } => LZ4_compress_HC_destSize(
                    self.hc_state.context("missing LZ4HC state")?.as_ptr(),
                    input.as_ptr().cast(),
                    output.as_mut_ptr().cast(),
                    &mut consumed,
                    capacity,
                    level,
                ),
            }
        };
        ensure!(
            written > 0 && written <= capacity && consumed > 0 && consumed as usize <= input.len(),
            "LZ4 compression failed"
        );
        Ok((consumed as usize, written as usize))
    }
}

impl Drop for Compressor {
    fn drop(&mut self) {
        if let Some(state) = self.hc_state {
            unsafe { LZ4_freeStreamHC(state.as_ptr()) };
        }
    }
}

struct InputQueue {
    bytes: Vec<u8>,
    head: usize,
    tail: usize,
    limit: usize,
    eof: bool,
}

impl InputQueue {
    fn new(limit: usize) -> Self {
        Self {
            bytes: vec![0; limit * 2],
            head: 0,
            tail: 0,
            limit,
            eof: false,
        }
    }

    fn fill(&mut self, input: &mut impl Read) -> Result<&[u8]> {
        if !self.eof && self.tail - self.head < self.limit {
            self.bytes.copy_within(self.head..self.tail, 0);
            self.tail -= self.head;
            self.head = 0;
            while self.tail < self.bytes.len() {
                match input.read(&mut self.bytes[self.tail..]) {
                    Ok(0) => {
                        self.eof = true;
                        break;
                    }
                    Ok(count) => self.tail += count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("reading input for EROFS compression"),
                }
            }
        }
        Ok(&self.bytes[self.head..self.tail.min(self.head + self.limit)])
    }
}

struct FullIndexes {
    bytes: Vec<u8>,
    cluster_offset: usize,
    block_size: usize,
    big_pcluster: bool,
}

impl FullIndexes {
    fn new(block_size: usize, big_pcluster: bool) -> Self {
        let mut bytes = vec![0; 16];
        if big_pcluster {
            bytes[4..6].copy_from_slice(&Z_EROFS_ADVISE_BIG_PCLUSTER_1.to_le_bytes());
        }
        Self {
            bytes,
            cluster_offset: 0,
            block_size,
            big_pcluster,
        }
    }

    fn push(&mut self, kind: u8, offset: usize, value: [u8; 4]) {
        self.bytes.extend_from_slice(&(kind as u16).to_le_bytes());
        self.bytes.extend_from_slice(&(offset as u16).to_le_bytes());
        self.bytes.extend_from_slice(&value);
    }

    fn append(&mut self, mut length: usize, block: u32, blocks: u16, raw: bool) {
        let mut forward = (self.cluster_offset + length) / self.block_size;
        let kind = if raw {
            Z_EROFS_LCLUSTER_TYPE_PLAIN
        } else {
            Z_EROFS_LCLUSTER_TYPE_HEAD1
        };
        let offset = self.cluster_offset;
        if forward == 0 {
            self.push(kind, offset, block.to_le_bytes());
            self.cluster_offset = 0;
            return;
        }

        let mut backward = 0usize;
        while self.cluster_offset + length >= self.block_size {
            if backward == 0 {
                self.push(kind, offset, block.to_le_bytes());
            } else {
                let delta = if backward == 1 && self.big_pcluster {
                    blocks | Z_EROFS_LI_D0_CBLKCNT
                } else {
                    backward.min(Z_EROFS_LI_D0_CBLKCNT as usize - 1) as u16
                };
                let mut value = [0; 4];
                value[..2].copy_from_slice(&delta.to_le_bytes());
                value[2..].copy_from_slice(&(forward as u16).to_le_bytes());
                self.push(Z_EROFS_LCLUSTER_TYPE_NONHEAD, offset, value);
            }
            length -= self.block_size - self.cluster_offset;
            self.cluster_offset = 0;
            backward += 1;
            forward -= 1;
        }
        self.cluster_offset += length;
    }

    fn finish(mut self) -> Vec<u8> {
        if self.cluster_offset != 0 {
            self.push(Z_EROFS_LCLUSTER_TYPE_PLAIN, self.cluster_offset, [0; 4]);
        }
        self.bytes
    }
}

pub fn compress_file<R: Read, W: Write + Seek>(
    input: &mut R,
    output: &mut W,
    block_size: u32,
    cluster_size: u32,
    algorithm: Compression,
) -> Result<CompressedFile> {
    ensure!(
        (512..=4096).contains(&block_size) && block_size.is_power_of_two(),
        "EROFS block size must be a power of two from 512 through 4096"
    );
    ensure!(
        cluster_size >= block_size
            && cluster_size.is_multiple_of(block_size)
            && cluster_size as u64 <= Z_EROFS_PCLUSTER_MAX_SIZE
            && cluster_size / block_size < Z_EROFS_LI_D0_CBLKCNT as u32,
        "invalid EROFS physical cluster size {cluster_size}"
    );
    let start = output
        .stream_position()
        .context("locating EROFS data output")?;
    ensure!(
        start.is_multiple_of(block_size as u64),
        "EROFS data output is not block aligned"
    );
    let block_size = block_size as usize;
    let cluster_size = cluster_size as usize;
    let source_limit = (cluster_size * 256).min(Z_EROFS_PCLUSTER_MAX_DSIZE as usize);
    let mut queue = InputQueue::new(source_limit);
    let mut compressor = Compressor::new(algorithm)?;
    let mut compressed = vec![0; cluster_size];
    let zeros = vec![0; block_size];
    let mut indexes = FullIndexes::new(block_size, cluster_size > block_size);
    let mut original_size = 0u64;
    let mut physical_size = 0u64;
    let mut used_compression = false;

    loop {
        let source = queue.fill(input)?;
        if source.is_empty() {
            break;
        }
        let (consumed, written) = if algorithm == Compression::None || source.len() <= block_size {
            (0, 0)
        } else {
            compressor.compress(source, &mut compressed)?
        };
        let padded = written.div_ceil(block_size) * block_size;
        let raw = written == 0 || padded >= consumed;
        let position = start
            .checked_add(physical_size)
            .context("EROFS physical address overflow")?;
        let block = u32::try_from(position / block_size as u64)
            .context("EROFS data exceeds 32-bit block addresses")?;
        let (consumed, physical) = if raw {
            let count = source.len().min(block_size);
            output
                .write_all(&source[..count])
                .context("writing plain EROFS data")?;
            output
                .write_all(&zeros[..block_size - count])
                .context("padding plain EROFS data")?;
            (count, block_size)
        } else {
            used_compression = true;
            output
                .write_all(&zeros[..padded - written])
                .context("padding compressed EROFS data")?;
            output
                .write_all(&compressed[..written])
                .context("writing compressed EROFS data")?;
            (consumed, padded)
        };
        indexes.append(consumed, block, (physical / block_size) as u16, raw);
        original_size = original_size
            .checked_add(consumed as u64)
            .context("EROFS file size overflow")?;
        physical_size = physical_size
            .checked_add(physical as u64)
            .context("EROFS physical size overflow")?;
        queue.head += consumed;
    }

    Ok(CompressedFile {
        metadata: indexes.finish(),
        original_size,
        physical_size,
        compressed_blocks: u32::try_from(physical_size / block_size as u64)
            .context("too many EROFS physical blocks")?,
        used_compression,
    })
}
