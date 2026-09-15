use bytes::{Buf, BufMut};
use log::trace;
use strum::FromRepr;
use thiserror::Error;

pub const FILE_HEADER_BYTES_LEN: usize = 28;
pub const CHUNK_HEADER_BYTES_LEN: usize = 12;
pub const HEADER_MAGIC: u32 = 0xed26ff3a;
pub const DEFAULT_BLOCKSIZE: u32 = 4096;

#[derive(Clone, Debug, Error)]
pub enum ParseError {
    #[error("Header has an unknown magic value")]
    UnknownMagic,
    #[error("Header has an unknown version")]
    UnknownVersion,
    #[error("Header has an unexpected header or chunk size")]
    UnexpectedSize,
    #[error("Header has an unknown chunk type")]
    UnknownChunkType,
}

pub type FileHeaderBytes = [u8; FILE_HEADER_BYTES_LEN];
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHeader {
    pub block_size: u32,
    pub blocks: u32,
    pub chunks: u32,
    pub checksum: u32,
}

impl FileHeader {
    pub fn from_bytes(bytes: &FileHeaderBytes) -> Result<FileHeader, ParseError> {
        let mut bytes = &bytes[..];

        let magic = bytes.get_u32_le();
        if magic != HEADER_MAGIC {
            trace!("Unrecognized header magic: {:x}", magic);
            return Err(ParseError::UnknownMagic);
        }

        let major = bytes.get_u16_le();
        if major != 0x1 {
            trace!("Unrecognized major versions: {:x}", major);
            return Err(ParseError::UnknownVersion);
        }

        let minor = bytes.get_u16_le();
        if minor != 0x0 {
            trace!("Unrecognized minor versions: {:x}", minor);
            return Err(ParseError::UnknownVersion);
        }

        let header_len = bytes.get_u16_le();
        if FILE_HEADER_BYTES_LEN != header_len.into() {
            trace!("Unexpected header size: {}", header_len);
            return Err(ParseError::UnexpectedSize);
        }

        let chunk_header_len = bytes.get_u16_le();
        if CHUNK_HEADER_BYTES_LEN != chunk_header_len.into() {
            trace!("Unexpected chunk header size: {}", chunk_header_len);
            return Err(ParseError::UnexpectedSize);
        }

        let block_size = bytes.get_u32_le();
        let blocks = bytes.get_u32_le();
        let chunks = bytes.get_u32_le();
        let checksum = bytes.get_u32_le();

        Ok(FileHeader {
            block_size,
            blocks,
            chunks,
            checksum,
        })
    }

    pub fn to_bytes(&self) -> FileHeaderBytes {
        let mut bytes = [0; FILE_HEADER_BYTES_LEN];
        let mut w = &mut bytes[..];
        w.put_u32_le(HEADER_MAGIC);
        // Version 1.0
        w.put_u16_le(0x1);
        w.put_u16_le(0x0);
        w.put_u16_le(FILE_HEADER_BYTES_LEN as u16);
        w.put_u16_le(CHUNK_HEADER_BYTES_LEN as u16);
        w.put_u32_le(self.block_size);
        w.put_u32_le(self.blocks);
        w.put_u32_le(self.chunks);
        w.put_u32_le(self.checksum);

        bytes
    }

    pub fn total_size(&self) -> usize {
        self.blocks as usize * self.block_size as usize
    }
}

#[derive(Copy, Clone, Debug, FromRepr, Eq, PartialEq)]
pub enum ChunkType {
    Raw = 0xcac1,
    Fill = 0xcac2,
    DontCare = 0xcac3,
    Crc32 = 0xcac4,
}

pub type ChunkHeaderBytes = [u8; CHUNK_HEADER_BYTES_LEN];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkHeader {
    pub chunk_type: ChunkType,
    pub chunk_size: u32,
    pub total_size: u32,
}

impl ChunkHeader {
    pub fn new_dontcare(blocks: u32) -> Self {
        ChunkHeader {
            chunk_type: ChunkType::DontCare,
            total_size: CHUNK_HEADER_BYTES_LEN as u32,
            chunk_size: blocks,
        }
    }

    pub fn new_raw(blocks: u32, block_size: u32) -> Self {
        ChunkHeader {
            chunk_type: ChunkType::Raw,
            chunk_size: blocks,
            total_size: (CHUNK_HEADER_BYTES_LEN as u32)
                .saturating_add(blocks.saturating_mul(block_size)),
        }
    }

    pub fn new_fill(blocks: u32) -> Self {
        ChunkHeader {
            chunk_type: ChunkType::Fill,
            chunk_size: blocks,
            total_size: CHUNK_HEADER_BYTES_LEN as u32 + 4,
        }
    }

    pub fn from_bytes(bytes: &ChunkHeaderBytes) -> Result<ChunkHeader, ParseError> {
        let mut bytes = &bytes[..];
        let chunk_type = bytes.get_u16_le();
        let Some(chunk_type) = ChunkType::from_repr(chunk_type.into()) else {
            trace!("Unknown chunk type: {}", chunk_type);
            return Err(ParseError::UnknownChunkType);
        };
        bytes.advance(2);
        let chunk_size = bytes.get_u32_le();
        let total_size = bytes.get_u32_le();

        Ok(ChunkHeader {
            chunk_type,
            chunk_size,
            total_size,
        })
    }

    pub fn to_bytes(&self) -> ChunkHeaderBytes {
        let mut bytes = [0; CHUNK_HEADER_BYTES_LEN];
        let mut w = &mut bytes[..];
        w.put_u16_le(self.chunk_type as u16);
        w.put_u16_le(0x0);
        w.put_u32_le(self.chunk_size);
        w.put_u32_le(self.total_size);
        bytes
    }

    pub fn out_size(&self, header: &FileHeader) -> usize {
        self.chunk_size as usize * header.block_size as usize
    }

    pub fn data_size(&self) -> usize {
        (self.total_size as usize).saturating_sub(CHUNK_HEADER_BYTES_LEN)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitChunk {
    pub header: ChunkHeader,
    pub offset: usize,
    pub size: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub header: FileHeader,
    pub chunks: Vec<SplitChunk>,
}

impl Split {
    fn from_chunks(chunks: Vec<SplitChunk>, block_size: u32) -> Self {
        let n_chunks = chunks.len() as u32;
        let blocks = chunks.iter().map(|c| c.header.chunk_size).sum();

        let header = FileHeader {
            block_size,
            blocks,
            chunks: n_chunks,
            checksum: 0,
        };

        Split { header, chunks }
    }

    pub fn sparse_size(&self) -> usize {
        FILE_HEADER_BYTES_LEN
            + self
                .chunks
                .iter()
                .map(|c| c.header.total_size as usize)
                .sum::<usize>()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SplitBuilder {
    space: u32,
    block_size: u32,
    chunks: Vec<SplitChunk>,
}

impl SplitBuilder {
    fn new(block_size: u32, mut space: u32, blocks_offset: u32) -> Self {
        space -= FILE_HEADER_BYTES_LEN as u32;
        let chunks = if blocks_offset == 0 {
            vec![]
        } else {
            let header = ChunkHeader::new_dontcare(blocks_offset);
            space -= header.total_size;
            vec![SplitChunk {
                header,
                offset: 0,
                size: 0,
            }]
        };
        Self {
            space,
            block_size,
            chunks,
        }
    }

    fn try_add_chunk(&mut self, chunk: &ChunkHeader, image_offset: usize) -> bool {
        if self.space > chunk.total_size {
            let split = SplitChunk {
                header: chunk.clone(),
                offset: image_offset,
                size: chunk.data_size(),
            };
            self.chunks.push(split);
            self.space -= chunk.total_size;
            true
        } else {
            false
        }
    }

    fn add_raw(&mut self, image_offset: usize, blocks: u32) -> u32 {
        let left = self.space.saturating_sub(CHUNK_HEADER_BYTES_LEN as u32);
        let blocks_left = left / self.block_size;

        if blocks_left > 0 {
            let blocks = blocks.min(blocks_left);
            let header = ChunkHeader::new_raw(blocks, self.block_size);
            self.space -= header.total_size;

            self.chunks.push(SplitChunk {
                size: header.data_size(),
                offset: image_offset,
                header,
            });

            blocks
        } else {
            0
        }
    }

    fn finish(self) -> Split {
        Split::from_chunks(self.chunks, self.block_size)
    }
}

#[derive(Debug, Error)]
pub enum SplitError {
    #[error("Size is too small to fit chunks")]
    TooSmall,
}

fn check_minimal_size(size: u32, block_size: u32) -> Result<(), SplitError> {
    if size < FILE_HEADER_BYTES_LEN as u32 + 2 * CHUNK_HEADER_BYTES_LEN as u32 + block_size {
        return Err(SplitError::TooSmall);
    }
    Ok(())
}

pub fn usable_download_size(size: u32) -> bool {
    check_minimal_size(size, DEFAULT_BLOCKSIZE).is_ok()
}

pub fn split_image(
    header: &FileHeader,
    chunks: &[ChunkHeader],
    size: u32,
) -> Result<Vec<Split>, SplitError> {
    check_minimal_size(size, header.block_size)?;
    let (_, _, builder, mut splits) = chunks.iter().try_fold(
        (
            0,
            FILE_HEADER_BYTES_LEN + CHUNK_HEADER_BYTES_LEN,
            SplitBuilder::new(header.block_size, size, 0),
            vec![],
        ),
        |(block_offset, image_offset, mut builder, mut splits), chunk| {
            if !builder.try_add_chunk(chunk, image_offset) {
                if chunk.chunk_type == ChunkType::Raw {
                    let mut blocks = 0;
                    loop {
                        blocks += builder.add_raw(
                            image_offset + (blocks * header.block_size) as usize,
                            chunk.chunk_size - blocks,
                        );

                        if blocks >= chunk.chunk_size {
                            break;
                        } else {
                            splits.push(builder.finish());
                            builder =
                                SplitBuilder::new(header.block_size, size, block_offset + blocks);
                        }
                    }
                } else {
                    splits.push(builder.finish());
                    builder = SplitBuilder::new(header.block_size, size, block_offset);
                    if !builder.try_add_chunk(chunk, image_offset) {
                        return Err(SplitError::TooSmall);
                    }
                }
            }
            Ok((
                block_offset + chunk.chunk_size,
                image_offset + chunk.total_size as usize,
                builder,
                splits,
            ))
        },
    )?;
    splits.push(builder.finish());
    Ok(splits)
}

pub fn split_raw(raw_size: usize, size: u32) -> Result<Vec<Split>, SplitError> {
    check_minimal_size(size, DEFAULT_BLOCKSIZE)?;
    let raw_blocks = raw_size.div_ceil(DEFAULT_BLOCKSIZE as usize) as u32;

    let mut block_offset = 0;
    let mut splits = vec![];

    while raw_blocks > block_offset {
        let mut builder = SplitBuilder::new(DEFAULT_BLOCKSIZE, size, block_offset);
        block_offset += builder.add_raw(
            (block_offset * DEFAULT_BLOCKSIZE) as usize,
            raw_blocks - block_offset,
        );
        splits.push(builder.finish());
    }
    Ok(splits)
}
