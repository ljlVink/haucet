use std::ffi::{c_char, c_int, c_void};
use std::fmt;
use std::io::{Read, Seek, Write};
use std::ptr::NonNull;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};

use crate::erofs_fs::{
    Z_EROFS_ADVISE_BIG_PCLUSTER_1, Z_EROFS_LCLUSTER_TYPE_HEAD1, Z_EROFS_LCLUSTER_TYPE_NONHEAD,
    Z_EROFS_LCLUSTER_TYPE_PLAIN, Z_EROFS_LI_D0_CBLKCNT, Z_EROFS_PCLUSTER_MAX_DSIZE,
    Z_EROFS_PCLUSTER_MAX_SIZE,
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
            // SAFETY: liblz4 allocates and initializes its own opaque state.
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
        // SAFETY: both slices are valid for their checked lengths. HC state is
        // initialized, exclusively borrowed, and released by Drop.
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
            // SAFETY: this is the unique state returned by LZ4_createStreamHC.
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
                // The first NONHEAD holds physical length for big pclusters.
                // Longer lookbacks stop below the flag bit and continue from
                // an earlier NONHEAD when the decompressed extent exceeds 8 MiB.
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

/// Write block-aligned EROFS data using bounded streaming LZ4 compression.
/// The caller selects the final inode layout and can overwrite the result with
/// flat data when the returned metadata would outweigh the compression gain.
/// Compressed images must enable LZ4_0PADDING and, for larger physical clusters,
/// BIG_PCLUSTER with the corresponding superblock compression configuration.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, SeekFrom};
    use std::sync::Arc;

    use crate::data::inode_pread;
    use crate::erofs_fs::{
        EROFS_FEATURE_INCOMPAT_BIG_PCLUSTER, EROFS_FEATURE_INCOMPAT_LZ4_0PADDING,
        EROFS_INODE_COMPRESSED_FULL, EROFS_SUPER_MAGIC_V1,
    };
    use crate::inode::Inode;
    use crate::io::Device;
    use crate::sb::erofs_read_superblock;

    fn pseudorandom(count: usize, state: &mut u32) -> Vec<u8> {
        (0..count)
            .map(|_| {
                *state ^= *state << 13;
                *state ^= *state >> 17;
                *state ^= *state << 5;
                *state as u8
            })
            .collect()
    }

    fn round_trip(input: &[u8], algorithm: Compression, cluster: u32) -> CompressedFile {
        round_trip_blocks(input, algorithm, 4096, cluster)
    }

    fn round_trip_blocks(
        input: &[u8],
        algorithm: Compression,
        block_size: u32,
        cluster: u32,
    ) -> CompressedFile {
        let block = block_size as u64;
        let mut image = tempfile::NamedTempFile::new().unwrap();
        let mut superblock = vec![0; 4096];
        superblock[1024..1028].copy_from_slice(&EROFS_SUPER_MAGIC_V1.to_le_bytes());
        superblock[1036] = block_size.trailing_zeros() as u8;
        superblock[1104..1108].copy_from_slice(&EROFS_FEATURE_INCOMPAT_LZ4_0PADDING.to_le_bytes());
        superblock[1108..1110].copy_from_slice(&u16::MAX.to_le_bytes());
        image.write_all(&superblock).unwrap();
        let result = compress_file(
            &mut Cursor::new(input),
            &mut image,
            block_size,
            cluster,
            algorithm,
        )
        .unwrap();
        assert_eq!(result.original_size, input.len() as u64);
        assert_eq!(
            result.compressed_blocks as u64 * block,
            result.physical_size
        );
        assert_eq!(
            result.metadata.len(),
            16 + input.len().div_ceil(block_size as usize) * 8
        );
        let meta_block = image.stream_position().unwrap() / block;
        image.write_all(&[0; 64]).unwrap();
        image.write_all(&result.metadata).unwrap();
        let padded = image.stream_position().unwrap().div_ceil(block) * block;
        image.as_file_mut().set_len(padded).unwrap();
        image.seek(SeekFrom::Start(1064)).unwrap();
        image.write_all(&(meta_block as u32).to_le_bytes()).unwrap();
        image.seek(SeekFrom::Start(1060)).unwrap();
        image
            .write_all(&((padded / block) as u32).to_le_bytes())
            .unwrap();
        image.flush().unwrap();
        let device = Device::open(image.path().to_str().unwrap(), 0).unwrap();
        let mut sbi = erofs_read_superblock(device).unwrap();
        if cluster > block_size {
            // Superblock config emission is tested by the filesystem builder;
            // these tests exercise the payload and inode map independently.
            sbi.feature_incompat |= EROFS_FEATURE_INCOMPAT_BIG_PCLUSTER;
            sbi.lz4_max_pclusterblks = (cluster / block_size) as u16;
        }
        let mut inode = Inode::new(Arc::new(sbi), 0);
        inode.i_size = result.original_size;
        inode.inode_isize = 64;
        inode.datalayout = EROFS_INODE_COMPRESSED_FULL;
        let mut restored = vec![0; input.len()];
        inode_pread(&mut inode, &mut restored, 0).unwrap();
        assert_eq!(restored, input);
        for offset in (0..input.len()).step_by(3997.max(input.len() / 37)) {
            let length = (input.len() - offset).min(6137);
            let mut part = vec![0; length];
            inode_pread(&mut inode, &mut part, offset as u64).unwrap();
            assert_eq!(part, input[offset..offset + length]);
        }
        result
    }

    #[test]
    fn compression_options_are_validated() {
        assert_eq!(
            "lz4hc".parse::<Compression>().unwrap(),
            Compression::default()
        );
        assert_eq!("lz4".parse::<Compression>().unwrap(), Compression::Lz4);
        assert!("lz4hc,13".parse::<Compression>().is_err());
        assert!("lz4,9".parse::<Compression>().is_err());
        let mut output = Cursor::new(Vec::new());
        assert!(
            compress_file(
                &mut Cursor::new([]),
                &mut output,
                4096,
                6000,
                Compression::Lz4
            )
            .is_err()
        );
        output.set_position(1);
        assert!(
            compress_file(
                &mut Cursor::new([]),
                &mut output,
                4096,
                4096,
                Compression::Lz4
            )
            .is_err()
        );
    }

    #[test]
    fn small_and_incompressible_files_use_plain_blocks() {
        let mut state = 12345;
        for size in [0, 1, 4095, 4096, 4097, 17123] {
            let input = pseudorandom(size, &mut state);
            let result = round_trip(&input, Compression::default(), 16384);
            assert!(!result.used_compression);
            assert_eq!(result.physical_size, size.div_ceil(4096) as u64 * 4096);
        }
    }

    #[test]
    fn variable_length_clusters_and_raw_tails_round_trip() {
        let mut input = Vec::new();
        let mut state = 678901;
        for _ in 0..500 {
            let pattern = pseudorandom(1000, &mut state);
            for _ in 0..3 {
                input.extend_from_slice(&pattern);
            }
        }
        input.extend(pseudorandom(6017, &mut state));
        for algorithm in [Compression::Lz4, Compression::default()] {
            for cluster in [4096, 16384] {
                let result = round_trip(&input, algorithm, cluster);
                assert!(result.used_compression);
                assert!(result.physical_size < input.len() as u64 / 2);
                assert!(result.metadata[16..].chunks_exact(8).any(|index| {
                    index[0] == Z_EROFS_LCLUSTER_TYPE_HEAD1
                        && u16::from_le_bytes([index[2], index[3]]) != 0
                }));
                if cluster > 4096 {
                    assert!(result.metadata[16..].chunks_exact(8).any(|index| {
                        let delta = u16::from_le_bytes([index[4], index[5]]);
                        index[0] == Z_EROFS_LCLUSTER_TYPE_NONHEAD
                            && delta & Z_EROFS_LI_D0_CBLKCNT != 0
                            && delta & !Z_EROFS_LI_D0_CBLKCNT > 1
                    }));
                }
            }
        }
    }

    #[test]
    fn long_extent_lookbacks_do_not_overlap_the_physical_length_flag() {
        let input = vec![0x62; Z_EROFS_PCLUSTER_MAX_DSIZE as usize + 321];
        let result = round_trip(&input, Compression::Lz4, 65536);
        assert!(result.physical_size < 65536);
        assert!(result.metadata[16..].chunks_exact(8).any(|index| {
            index[0] == Z_EROFS_LCLUSTER_TYPE_NONHEAD
                && u16::from_le_bytes([index[4], index[5]]) == Z_EROFS_LI_D0_CBLKCNT - 1
        }));
    }

    #[test]
    fn small_filesystem_blocks_round_trip() {
        let mut state = 37123;
        let pattern = pseudorandom(701, &mut state);
        let mut input = pattern.repeat(100);
        input.extend(pseudorandom(1234, &mut state));
        for block in [512, 1024, 2048] {
            for cluster in [block, block * 4] {
                round_trip_blocks(&input, Compression::Lz4, block, cluster);
            }
        }
    }
}
