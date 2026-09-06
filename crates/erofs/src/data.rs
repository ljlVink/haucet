use std::sync::Arc;

use crate::decompress::z_erofs_decompress;
use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::fragments::erofs_packedfile_read;
use crate::inode::Inode;
use crate::sb::SbInfo;
use crate::zmap::z_erofs_map_blocks_iter;

pub type Sb = Arc<SbInfo>;

#[derive(Clone, Copy, Default)]
pub struct MapBlocks {
    pub m_pa: u64,
    pub m_la: u64,
    pub m_plen: u64,
    pub m_llen: u64,
    pub m_flags: u32,
    pub m_deviceid: u32,
    pub m_algorithmformat: u8,
}

pub fn erofs_map_blocks(inode: &mut Inode, map: &mut MapBlocks, flags: u32) -> Result<()> {
    if erofs_inode_is_data_compressed(inode.datalayout) {
        z_erofs_map_blocks_iter(inode, map, flags)
    } else {
        __erofs_map_blocks(inode, map)
    }
}

pub fn __erofs_map_blocks(inode: &Inode, map: &mut MapBlocks) -> Result<()> {
    let sbi = &inode.sbi;
    let blksz = sbi.blksiz() as u64;

    map.m_deviceid = 0;
    map.m_flags = 0;
    if map.m_la >= inode.i_size {
        map.m_plen = 0;
        map.m_llen = 0;
        return Ok(());
    }

    if inode.datalayout != EROFS_INODE_CHUNK_BASED {
        let tailpacking = inode.datalayout == EROFS_INODE_FLAT_INLINE;
        if !tailpacking && inode.u_blkaddr == EROFS_NULL_ADDR {
            map.m_llen = inode.i_size - map.m_la;
            map.m_plen = map.m_llen;
            return Ok(());
        }
        let nblocks = blk_round_up(sbi.blkszbits, inode.i_size);
        let pos = (nblocks - if tailpacking { 1 } else { 0 }) << sbi.blkszbits;

        map.m_flags = EROFS_MAP_MAPPED;
        if map.m_la < pos {
            map.m_pa = inode.u_blkaddr * blksz + map.m_la;
            map.m_llen = pos - map.m_la;
        } else {
            map.m_pa = inode.iloc()
                + inode.inode_isize as u64
                + inode.xattr_isize as u64
                + sbi.blkoff(map.m_la) as u64;
            map.m_llen = inode.i_size - map.m_la;
            map.m_flags |= EROFS_MAP_META;
        }
        map.m_plen = map.m_llen;
        if map.m_flags & EROFS_MAP_META != 0 && sbi.blkoff(map.m_pa) as u64 + map.m_plen > blksz {
            return Err(Error::efscorrupted());
        }
        return Ok(());
    }

    let unit: u64 = if inode.chunkformat & EROFS_CHUNK_FORMAT_INDEXES != 0 {
        8
    } else {
        EROFS_BLOCK_MAP_ENTRY_SIZE as u64
    };

    let chunknr = map.m_la >> inode.chunkbits;
    let pos = round_up(
        inode.iloc() + inode.inode_isize as u64 + inode.xattr_isize as u64,
        unit,
    ) + unit * chunknr;

    let blk = inode.read_meta_cached(pos)?;
    let idx = &blk[sbi.blkoff(pos) as usize..];

    map.m_la = chunknr << inode.chunkbits;
    map.m_llen = std::cmp::min(
        1u64 << inode.chunkbits,
        round_up(inode.i_size - map.m_la, blksz),
    );
    if inode.chunkformat & EROFS_CHUNK_FORMAT_INDEXES != 0 {
        let addrmask: u64 = if inode.chunkformat & EROFS_CHUNK_FORMAT_48BIT != 0 {
            (1u64 << 48) - 1
        } else {
            (1u64 << 32) - 1
        };
        let startblk = (((get_unaligned_le16(idx, 0) as u64) << 32)
            | get_unaligned_le32(idx, 2) as u64)
            & addrmask;
        if (startblk ^ EROFS_NULL_ADDR) & addrmask != 0 {
            map.m_pa = sbi.pos(startblk);
            map.m_flags = EROFS_MAP_MAPPED;
        }
    } else {
        let startblk = get_unaligned_le32(idx, 0) as u64;
        if startblk != EROFS_NULL_ADDR {
            map.m_pa = sbi.pos(startblk);
            map.m_flags = EROFS_MAP_MAPPED;
        }
    }
    map.m_plen = map.m_llen;
    Ok(())
}

pub fn erofs_read_one_data(
    inode: &Inode,
    map: &MapBlocks,
    buffer: &mut [u8],
    offset: u64,
) -> Result<()> {
    inode
        .sbi
        .dev
        .read_at(buffer, map.m_pa + offset)
        .map_err(|_| Error::eio())
}

pub fn erofs_read_raw_data(inode: &mut Inode, buffer: &mut [u8], offset: u64) -> Result<()> {
    let size = buffer.len() as u64;
    let mut ptr = offset;
    while ptr < offset + size {
        let estart = (ptr - offset) as usize;
        let mut map = MapBlocks {
            m_la: ptr,
            ..MapBlocks::default()
        };
        erofs_map_blocks(inode, &mut map, 0)?;

        let eend = std::cmp::min(offset + size, map.m_la + map.m_llen);
        let elen = (eend - ptr) as usize;
        if !(map.m_flags & EROFS_MAP_MAPPED != 0) {
            if map.m_llen == 0 {
                // reached EOF
                buffer[estart..].fill(0);
                return Ok(());
            }
            buffer[estart..estart + elen].fill(0);
            ptr = eend;
            continue;
        }

        let moff = ptr - map.m_la;
        erofs_read_one_data(inode, &map, &mut buffer[estart..estart + elen], moff)?;
        ptr = eend;
    }
    Ok(())
}

pub fn inode_pread(inode: &mut Inode, buf: &mut [u8], offset: u64) -> Result<()> {
    if erofs_inode_is_data_compressed(inode.datalayout) {
        z_erofs_read_data(inode, buf, offset)
    } else {
        erofs_read_raw_data(inode, buf, offset)
    }
}

pub fn read_metadata(sbi: &Sb, nid: u64, offset: &mut u64) -> Result<Vec<u8>> {
    if nid != 0 {
        return read_metadata_nid(sbi, nid, offset);
    }
    read_metadata_bdi(sbi, offset)
}

pub fn read_metadata_bdi(sbi: &SbInfo, offset: &mut u64) -> Result<Vec<u8>> {
    let blksz = sbi.blksiz();
    *offset = round_up(*offset, 4);
    let data = sbi.read_meta(*offset)?;
    let len = get_unaligned_le16(&data, sbi.blkoff(*offset) as usize) as usize;
    if len == 0 {
        return Err(Error::efscorrupted());
    }
    *offset += 2;
    let mut buffer = vec![0u8; len];
    let mut i = 0usize;
    while i < len {
        let cnt = std::cmp::min(blksz as usize - sbi.blkoff(*offset) as usize, len - i);
        let blk = sbi.read_meta(*offset)?;
        buffer[i..i + cnt].copy_from_slice(
            &blk[sbi.blkoff(*offset) as usize..sbi.blkoff(*offset) as usize + cnt],
        );
        *offset += cnt as u64;
        i += cnt;
    }
    Ok(buffer)
}

pub fn read_metadata_nid(sbi: &Sb, nid: u64, offset: &mut u64) -> Result<Vec<u8>> {
    let mut vi = Inode::new(sbi.clone(), nid);
    vi.read_from_disk()?;

    *offset = round_up(*offset, 4);
    let mut lenbuf = [0u8; 2];
    inode_pread(&mut vi, &mut lenbuf, *offset)?;
    let len = u16::from_le_bytes(lenbuf) as usize;
    if len == 0 {
        return Err(Error::efscorrupted());
    }
    *offset += 2;
    let mut buffer = vec![0u8; len];
    inode_pread(&mut vi, &mut buffer, *offset)?;
    *offset += len as u64;
    Ok(buffer)
}

pub fn z_erofs_read_one_data(
    inode: &mut Inode,
    map: &mut MapBlocks,
    raw: &mut [u8],
    buffer: &mut [u8],
    skip: u64,
    length: u64,
    trimmed: bool,
) -> Result<()> {
    let sbi = inode.sbi.clone();
    if map.m_flags & EROFS_MAP_FRAGMENT_BIT != 0 {
        if inode.nid == sbi.packed_nid {
            return Err(Error::efscorrupted());
        }
        return erofs_packedfile_read(
            &sbi,
            &mut buffer[..(length - skip) as usize],
            inode.z_fragmentoff + skip,
        );
    }
    sbi.dev.read_at(&mut raw[..map.m_plen as usize], map.m_pa)?;
    let partial_decoding = trimmed
        || !(map.m_flags & EROFS_MAP_FULL_MAPPED != 0)
        || map.m_flags & EROFS_MAP_PARTIAL_REF != 0;
    let interlaced_offset = if map.m_algorithmformat == Z_EROFS_COMPRESSION_INTERLACED {
        sbi.blkoff(map.m_la) as u64
    } else {
        0
    };
    z_erofs_decompress(
        &sbi,
        &raw[..map.m_plen as usize],
        buffer,
        skip,
        length,
        interlaced_offset,
        map.m_algorithmformat,
        partial_decoding,
    )
}

pub fn z_erofs_read_data(inode: &mut Inode, buffer: &mut [u8], offset: u64) -> Result<()> {
    let size = buffer.len() as u64;
    let mut end = offset + size;
    let mut raw: Vec<u8> = Vec::new();

    while end > offset {
        let mut map = MapBlocks {
            m_la: end - 1,
            ..MapBlocks::default()
        };

        z_erofs_map_blocks_iter(inode, &mut map, 0)?;

        let (length, trimmed) = if end < map.m_la + map.m_llen {
            (end - map.m_la, true)
        } else {
            (map.m_llen, false)
        };

        let skip;
        if map.m_la < offset {
            skip = offset - map.m_la;
            end = offset;
        } else {
            skip = 0;
            end = map.m_la;
        }

        if !(map.m_flags & EROFS_MAP_MAPPED != 0) {
            let dst_start = (end - offset) as usize;
            buffer[dst_start..dst_start + (length - skip) as usize].fill(0);
            end = map.m_la;
            continue;
        }

        if map.m_plen as usize > raw.len() {
            raw.resize(map.m_plen as usize, 0);
        }

        let dst_start = (end - offset) as usize;
        let output_len = (length - skip) as usize;
        z_erofs_read_one_data(
            inode,
            &mut map,
            &mut raw,
            &mut buffer[dst_start..dst_start + output_len],
            skip,
            length,
            trimmed,
        )?;
    }
    Ok(())
}
