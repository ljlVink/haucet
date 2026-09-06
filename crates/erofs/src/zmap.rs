use crate::data::MapBlocks;
use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::inode::Inode;

struct Maprecorder {
    lcn: u64,
    r#type: u8,
    headtype: u8,
    clusterofs: u16,
    delta: [u16; 2],
    pblk: u64,
    compressedblks: u64,
    nextpackoff: u64,
    partialref: bool,
}

impl Maprecorder {
    fn new() -> Maprecorder {
        Maprecorder {
            lcn: 0,
            r#type: 0,
            headtype: 0,
            clusterofs: 0,
            delta: [0; 2],
            pblk: 0,
            compressedblks: 0,
            nextpackoff: 0,
            partialref: false,
        }
    }
}

fn z_erofs_load_full_lcluster(vi: &Inode, m: &mut Maprecorder, lcn: u64) -> Result<()> {
    let pos = z_erofs_full_index_start(vi.iloc() + vi.inode_isize as u64 + vi.xattr_isize as u64)
        + lcn * 8;

    let blk = vi.read_meta_cached(pos)?;
    let di = &blk[vi.sbi.blkoff(pos) as usize..];

    m.lcn = lcn;
    m.nextpackoff = pos + 8;

    let advise = get_unaligned_le16(di, 0);
    m.r#type = (advise & Z_EROFS_LI_LCLUSTER_TYPE_MASK) as u8;
    if m.r#type == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
        m.clusterofs = 1 << vi.z_lclusterbits;
        m.delta[0] = get_unaligned_le16(di, 4);
        if m.delta[0] & Z_EROFS_LI_D0_CBLKCNT != 0 {
            if vi.z_advise & (Z_EROFS_ADVISE_BIG_PCLUSTER_1 | Z_EROFS_ADVISE_BIG_PCLUSTER_2) == 0 {
                return Err(Error::efscorrupted());
            }
            m.compressedblks = (m.delta[0] & !Z_EROFS_LI_D0_CBLKCNT) as u64;
            m.delta[0] = 1;
        }
        m.delta[1] = get_unaligned_le16(di, 6);
    } else {
        m.partialref = advise & Z_EROFS_LI_PARTIAL_REF != 0;
        m.clusterofs = get_unaligned_le16(di, 2);
        m.pblk = get_unaligned_le32(di, 4) as u64;
    }
    Ok(())
}

fn decode_compactedbits(lobits: u32, pack: &[u8], pos: u32) -> (u32, u8) {
    let v = get_unaligned_le32(pack, (pos / 8) as usize) >> (pos & 7);
    let lo = v & ((1 << lobits) - 1);
    let t = ((v >> lobits) & 3) as u8;
    (lo, t)
}

fn get_compacted_la_distance(lobits: u32, encodebits: u32, vcnt: u32, pack: &[u8], i: u32) -> u32 {
    let mut i = i;
    let mut d1 = 0u32;

    loop {
        let (_lo, t) = decode_compactedbits(lobits, pack, encodebits * i);
        if t != Z_EROFS_LCLUSTER_TYPE_NONHEAD {
            return d1;
        }
        d1 += 1;
        i += 1;
        if i >= vcnt {
            break;
        }
    }

    let (lo, _) = decode_compactedbits(lobits, pack, encodebits * (vcnt - 1));
    if lo & (Z_EROFS_LI_D0_CBLKCNT as u32) == 0 {
        d1 += lo - 1;
    }
    d1
}

fn z_erofs_load_compact_lcluster(
    vi: &Inode,
    m: &mut Maprecorder,
    lcn: u64,
    lookahead: bool,
) -> Result<()> {
    let sbi = &vi.sbi;
    let lclusterbits = vi.z_lclusterbits;
    let totalidx = blk_round_up(sbi.blkszbits, vi.i_size);
    let mut lcn = lcn;
    let big_pcluster = vi.z_advise & Z_EROFS_ADVISE_BIG_PCLUSTER_1 != 0;

    if lcn >= totalidx || lclusterbits > 14 {
        return Err(Error::errno(-libc::EINVAL));
    }
    m.lcn = lcn;
    let ebase = 8 + round_up(vi.iloc() + vi.inode_isize as u64 + vi.xattr_isize as u64, 8);
    let compacted_4b_initial = ((32 - ebase % 32) / 4) & 7;
    let mut compacted_2b = 0u64;
    if vi.z_advise & Z_EROFS_ADVISE_COMPACTED_2B != 0 && compacted_4b_initial < totalidx {
        compacted_2b = round_down(totalidx - compacted_4b_initial, 16);
    }

    let mut pos = ebase;
    let mut amortizedshift = 2u32;
    if lcn >= compacted_4b_initial {
        pos += compacted_4b_initial * 4;
        lcn -= compacted_4b_initial;
        if lcn < compacted_2b {
            amortizedshift = 1;
        } else {
            pos += compacted_2b * 2;
            lcn -= compacted_2b;
        }
    }
    pos += lcn * (1 << amortizedshift);

    let vcnt: u32 = if (1 << amortizedshift) == 4 && lclusterbits <= 14 {
        2
    } else if (1 << amortizedshift) == 2 && lclusterbits <= 12 {
        16
    } else {
        return Err(Error::eopnotsupp());
    };

    let blk = vi.read_meta_cached(pos)?;
    let block = &blk[..];

    m.nextpackoff =
        round_down(pos, (vcnt << amortizedshift) as u64) + ((vcnt << amortizedshift) as u64);
    let lobits = std::cmp::max(lclusterbits, ilog2(Z_EROFS_LI_D0_CBLKCNT as u32) + 1);
    let encodebits: u32 = (((vcnt << amortizedshift) - 4) * 8) >> ilog2(vcnt);
    let pack_size = (vcnt << amortizedshift) as usize;
    let bytes = pos & (pack_size as u64 - 1);
    let block_pos = sbi.blkoff(pos) as u64;
    if bytes > block_pos {
        return Err(Error::efscorrupted());
    }
    let pack_start = (block_pos - bytes) as usize;
    let pack_end = pack_start + pack_size;
    if pack_end > block.len() {
        return Err(Error::efscorrupted());
    }
    let pack = &block[pack_start..pack_end];
    let i = (bytes >> amortizedshift) as i64;

    let (mut lo, t) = decode_compactedbits(lobits, pack, encodebits * i as u32);
    m.r#type = t;
    if t == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
        m.clusterofs = 1 << lclusterbits;

        if lookahead {
            m.delta[1] = get_compacted_la_distance(lobits, encodebits, vcnt, pack, i as u32) as u16;
        }
        if lo & (Z_EROFS_LI_D0_CBLKCNT as u32) != 0 {
            if !big_pcluster {
                return Err(Error::efscorrupted());
            }
            m.compressedblks = (lo & !(Z_EROFS_LI_D0_CBLKCNT as u32)) as u64;
            m.delta[0] = 1;
            return Ok(());
        } else if i + 1 != vcnt as i64 {
            m.delta[0] = lo as u16;
            return Ok(());
        }
        let (prev_lo, prev_t) = decode_compactedbits(lobits, pack, encodebits * (i - 1) as u32);
        if prev_t != Z_EROFS_LCLUSTER_TYPE_NONHEAD {
            lo = 0;
        } else if prev_lo & (Z_EROFS_LI_D0_CBLKCNT as u32) != 0 {
            lo = 1;
        } else {
            lo = prev_lo;
        }
        m.delta[0] = (lo + 1) as u16;
        return Ok(());
    }
    m.clusterofs = lo as u16;
    m.delta[0] = 0;
    let nblk: u64 = if !big_pcluster {
        let mut nblk = 1u64;
        let mut i = i;
        while i > 0 {
            i -= 1;
            let (lo2, t2) = decode_compactedbits(lobits, pack, encodebits * i as u32);
            if t2 == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
                i -= lo2 as i64;
            }
            if i >= 0 {
                nblk += 1;
            }
        }
        nblk
    } else {
        let mut nblk = 0u64;
        let mut i = i;
        while i > 0 {
            i -= 1;
            let (lo2, t2) = decode_compactedbits(lobits, pack, encodebits * i as u32);
            if t2 == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
                if lo2 & (Z_EROFS_LI_D0_CBLKCNT as u32) != 0 {
                    i -= 1;
                    nblk += (lo2 & !(Z_EROFS_LI_D0_CBLKCNT as u32)) as u64;
                    continue;
                }
                if lo2 <= 1 {
                    return Err(Error::efscorrupted());
                }
                i -= lo2 as i64 - 2;
                continue;
            }
            nblk += 1;
        }
        nblk
    };
    let tail = get_unaligned_le32(pack, pack_size - 4);
    m.pblk = tail as u64 + nblk;
    Ok(())
}

fn z_erofs_load_lcluster_from_disk(
    vi: &Inode,
    m: &mut Maprecorder,
    lcn: u64,
    lookahead: bool,
) -> Result<()> {
    if vi.datalayout == EROFS_INODE_COMPRESSED_COMPACT {
        z_erofs_load_compact_lcluster(vi, m, lcn, lookahead)?;
    } else {
        if vi.datalayout != EROFS_INODE_COMPRESSED_FULL {
            return Err(Error::efscorrupted());
        }
        z_erofs_load_full_lcluster(vi, m, lcn)?;
    }

    if m.r#type >= Z_EROFS_LCLUSTER_TYPE_MAX {
        return Err(Error::eopnotsupp());
    } else if m.r#type != Z_EROFS_LCLUSTER_TYPE_NONHEAD && m.clusterofs >= (1 << vi.z_lclusterbits)
    {
        return Err(Error::efscorrupted());
    }
    Ok(())
}

fn z_erofs_extent_lookback(
    vi: &Inode,
    m: &mut Maprecorder,
    mut lookback_distance: u64,
) -> Result<()> {
    while m.lcn >= lookback_distance {
        let lcn = m.lcn - lookback_distance;
        z_erofs_load_lcluster_from_disk(vi, m, lcn, false)?;

        if m.r#type >= Z_EROFS_LCLUSTER_TYPE_MAX {
            return Err(Error::eopnotsupp());
        } else if m.r#type == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
            lookback_distance = m.delta[0] as u64;
            if lookback_distance == 0 {
                break;
            }
            continue;
        } else {
            m.headtype = m.r#type;
            return Ok(());
        }
    }
    Err(Error::efscorrupted())
}

fn z_erofs_get_extent_compressedlen(
    vi: &Inode,
    m: &mut Maprecorder,
    _initial_lcn: u64,
) -> Result<()> {
    let bigpcl1 = vi.z_advise & Z_EROFS_ADVISE_BIG_PCLUSTER_1 != 0;
    let bigpcl2 = vi.z_advise & Z_EROFS_ADVISE_BIG_PCLUSTER_2 != 0;
    let lcn = m.lcn + 1;

    if m.r#type == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
        return Err(Error::efscorrupted());
    }

    if (m.headtype == Z_EROFS_LCLUSTER_TYPE_HEAD1 && !bigpcl1)
        || ((m.headtype == Z_EROFS_LCLUSTER_TYPE_PLAIN
            || m.headtype == Z_EROFS_LCLUSTER_TYPE_HEAD2)
            && !bigpcl2)
        || (lcn << vi.z_lclusterbits) >= vi.i_size
    {
        m.compressedblks = 1;
    }

    if m.compressedblks != 0 {
        return Ok(());
    }

    z_erofs_load_lcluster_from_disk(vi, m, lcn, false)?;

    if m.r#type == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
        if m.delta[0] != 1 {
            return Err(Error::efscorrupted());
        }
        if m.compressedblks != 0 {
            return Ok(());
        }
    } else if m.r#type < Z_EROFS_LCLUSTER_TYPE_MAX {
        m.compressedblks = 1;
        return Ok(());
    }
    Err(Error::efscorrupted())
}

fn z_erofs_get_extent_decompressedlen(
    vi: &Inode,
    m: &mut Maprecorder,
    map: &mut MapBlocks,
) -> Result<()> {
    let lclusterbits = vi.z_lclusterbits;
    let mut lcn = m.lcn;
    let headlcn = map.m_la >> lclusterbits;

    loop {
        if (lcn << lclusterbits) >= vi.i_size {
            map.m_llen = vi.i_size - map.m_la;
            return Ok(());
        }

        z_erofs_load_lcluster_from_disk(vi, m, lcn, true)?;

        if m.r#type == Z_EROFS_LCLUSTER_TYPE_NONHEAD {
            if m.delta[1] == 0 {
                m.delta[1] = 1;
            }
        } else if m.r#type < Z_EROFS_LCLUSTER_TYPE_MAX {
            if lcn != headlcn {
                break;
            }
            m.delta[1] = 1;
        } else {
            return Err(Error::eopnotsupp());
        }
        lcn += m.delta[1] as u64;
    }
    map.m_llen = (lcn << lclusterbits) + m.clusterofs as u64 - map.m_la;
    Ok(())
}

fn z_erofs_map_blocks_fo(vi: &mut Inode, map: &mut MapBlocks, flags: u32) -> Result<()> {
    let sbi = vi.sbi.clone();
    let fragment = vi.z_advise & Z_EROFS_ADVISE_FRAGMENT_PCLUSTER != 0;
    let ztailpacking = vi.z_idata_size != 0;
    let lclusterbits = vi.z_lclusterbits;
    let mut m = Maprecorder::new();

    let ofs = if flags & EROFS_GET_BLOCKS_FINDTAIL != 0 {
        vi.i_size - 1
    } else {
        map.m_la
    };
    if fragment && flags & EROFS_GET_BLOCKS_FINDTAIL == 0 && vi.z_tailextent_headlcn == 0 {
        map.m_la = 0;
        map.m_llen = vi.i_size;
        map.m_flags = EROFS_MAP_FRAGMENT;
        return Ok(());
    }
    let initial_lcn = ofs >> lclusterbits;
    let endoff = ofs & ((1 << lclusterbits) - 1);

    z_erofs_load_lcluster_from_disk(vi, &mut m, initial_lcn, false)?;

    if flags & EROFS_GET_BLOCKS_FINDTAIL != 0 && ztailpacking {
        vi.z_fragmentoff = m.nextpackoff;
    }
    map.m_flags = EROFS_MAP_MAPPED | EROFS_MAP_ENCODED;
    let mut end = (m.lcn + 1) << lclusterbits;

    match m.r#type {
        Z_EROFS_LCLUSTER_TYPE_PLAIN | Z_EROFS_LCLUSTER_TYPE_HEAD1 | Z_EROFS_LCLUSTER_TYPE_HEAD2 => {
            if endoff >= m.clusterofs as u64 {
                m.headtype = m.r#type;
                map.m_la = (m.lcn << lclusterbits) | m.clusterofs as u64;
                if ztailpacking && end > vi.i_size {
                    end = vi.i_size;
                }
            } else {
                if m.lcn == 0 {
                    return Err(Error::efscorrupted());
                }
                end = (m.lcn << lclusterbits) | m.clusterofs as u64;
                map.m_flags |= EROFS_MAP_FULL_MAPPED;
                m.delta[0] = 1;
                let d0 = m.delta[0] as u64;
                z_erofs_extent_lookback(vi, &mut m, d0)?;
                map.m_la = (m.lcn << lclusterbits) | m.clusterofs as u64;
            }
        }
        Z_EROFS_LCLUSTER_TYPE_NONHEAD => {
            let d0 = m.delta[0] as u64;
            z_erofs_extent_lookback(vi, &mut m, d0)?;
            map.m_la = (m.lcn << lclusterbits) | m.clusterofs as u64;
        }
        _ => return Err(Error::eopnotsupp()),
    }
    if m.partialref {
        map.m_flags |= EROFS_MAP_PARTIAL_REF;
    }
    map.m_llen = end - map.m_la;

    if flags & EROFS_GET_BLOCKS_FINDTAIL != 0 {
        vi.z_tailextent_headlcn = m.lcn;
        if fragment && vi.datalayout == EROFS_INODE_COMPRESSED_FULL {
            vi.z_fragmentoff |= m.pblk << 32;
        }
    }
    if ztailpacking && m.lcn == vi.z_tailextent_headlcn {
        map.m_flags |= EROFS_MAP_META;
        map.m_pa = vi.z_fragmentoff;
        map.m_plen = vi.z_idata_size as u64;
        if sbi.blkoff(map.m_pa) as u64 + map.m_plen > sbi.blksiz() as u64 {
            return Err(Error::efscorrupted());
        }
    } else if fragment && m.lcn == vi.z_tailextent_headlcn {
        map.m_flags = EROFS_MAP_FRAGMENT;
    } else {
        map.m_pa = sbi.pos(m.pblk);
        z_erofs_get_extent_compressedlen(vi, &mut m, initial_lcn)?;
        map.m_plen = sbi.pos(m.compressedblks);
    }

    if m.headtype == Z_EROFS_LCLUSTER_TYPE_PLAIN {
        if map.m_llen > map.m_plen {
            return Err(Error::efscorrupted());
        }
        if vi.z_advise & Z_EROFS_ADVISE_INTERLACED_PCLUSTER != 0 {
            map.m_algorithmformat = Z_EROFS_COMPRESSION_INTERLACED;
        } else {
            map.m_algorithmformat = Z_EROFS_COMPRESSION_SHIFTED;
        }
    } else if m.headtype == Z_EROFS_LCLUSTER_TYPE_HEAD2 {
        map.m_algorithmformat = vi.z_algorithmtype[1];
    } else {
        map.m_algorithmformat = vi.z_algorithmtype[0];
    }

    if flags & EROFS_GET_BLOCKS_FIEMAP != 0 {
        z_erofs_get_extent_decompressedlen(vi, &mut m, map)?;
        map.m_flags |= EROFS_MAP_FULL_MAPPED;
    }

    Ok(())
}

fn z_erofs_map_blocks_ext(vi: &mut Inode, map: &mut MapBlocks, _flags: u32) -> Result<()> {
    let sbi = vi.sbi.clone();
    let interlaced = vi.z_advise & Z_EROFS_ADVISE_INTERLACED_PCLUSTER != 0;
    let recsz = z_erofs_extent_recsize(vi.z_advise);
    let mut pos = round_up(
        z_erofs_map_header_end(vi.iloc() + vi.inode_isize as u64 + vi.xattr_isize as u64),
        recsz,
    );
    let bmask = sbi.blksiz() as u64 - 1;
    let mut lend = vi.i_size;
    let mut lstart: u64;
    let mut pa: u64;
    let last: bool;
    let fmt: u32;

    map.m_flags = 0;
    if recsz <= 12 {
        if recsz <= 8 {
            let blk = vi.read_meta_cached(pos)?;
            let ext = &blk[sbi.blkoff(pos) as usize..];
            pa = u64::from_le_bytes(ext[0..8].try_into().unwrap());
            pos += 8;
            lstart = 0;
        } else {
            lstart = round_down(map.m_la, 1 << vi.z_lclusterbits);
            pos += (lstart >> vi.z_lclusterbits) * recsz;
            pa = EROFS_NULL_ADDR;
        }

        while lstart <= map.m_la {
            let blk = vi.read_meta_cached(pos)?;
            let ext = &blk[sbi.blkoff(pos) as usize..];
            map.m_plen = get_unaligned_le32(ext, 0) as u64;
            if pa != EROFS_NULL_ADDR {
                map.m_pa = pa;
                pa += map.m_plen & Z_EROFS_EXTENT_PLEN_MASK as u64;
            } else {
                map.m_pa = get_unaligned_le32(ext, 4) as u64;
            }
            pos += recsz;
            lstart += 1 << vi.z_lclusterbits;
        }
        last = lstart >= round_up(lend, 1 << vi.z_lclusterbits);
        lend = std::cmp::min(lstart, lend);
        lstart -= 1 << vi.z_lclusterbits;
    } else {
        lstart = lend;
        let (mut l, mut r) = (0u64, vi.z_extents);
        while l < r {
            let mid = l + (r - l) / 2;
            let blk = vi.read_meta_cached(pos + mid * recsz)?;
            let ext = &blk[sbi.blkoff(pos + mid * recsz) as usize..];

            let mut la = get_unaligned_le32(ext, 12) as u64;
            pa = get_unaligned_le32(ext, 8) as u64 | (get_unaligned_le32(ext, 4) as u64) << 32;
            if recsz > 20 {
                la |= (get_unaligned_le32(ext, 16) as u64) << 32;
            }

            if la > map.m_la {
                r = mid;
                if la > lend {
                    return Err(Error::efscorrupted());
                }
                lend = la;
            } else {
                l = mid + 1;
                if map.m_la == la {
                    r = std::cmp::min(l + 1, r);
                }
                lstart = la;
                map.m_plen = get_unaligned_le32(ext, 0) as u64;
                map.m_pa = pa;
            }
        }
        last = l >= vi.z_extents;
    }

    if lstart < lend {
        map.m_la = lstart;
        if last && vi.z_advise & Z_EROFS_ADVISE_FRAGMENT_PCLUSTER != 0 {
            map.m_flags = EROFS_MAP_FRAGMENT;
            vi.z_fragmentoff = map.m_plen;
            if recsz > 8 {
                vi.z_fragmentoff |= map.m_pa << 32;
            }
        } else if map.m_plen & Z_EROFS_EXTENT_PLEN_MASK as u64 != 0 {
            map.m_flags |= EROFS_MAP_MAPPED | EROFS_MAP_FULL_MAPPED | EROFS_MAP_ENCODED;
            fmt = (map.m_plen >> Z_EROFS_EXTENT_PLEN_FMT_BIT) as u32;
            if map.m_plen & Z_EROFS_EXTENT_PLEN_PARTIAL as u64 != 0 {
                map.m_flags |= EROFS_MAP_PARTIAL_REF;
            }
            map.m_plen &= Z_EROFS_EXTENT_PLEN_MASK as u64;
            if fmt != 0 {
                map.m_algorithmformat = (fmt - 1) as u8;
            } else if interlaced && (map.m_pa | map.m_plen) & bmask == 0 {
                map.m_algorithmformat = Z_EROFS_COMPRESSION_INTERLACED;
            } else {
                map.m_algorithmformat = Z_EROFS_COMPRESSION_SHIFTED;
            }
        }
    }
    map.m_llen = lend - map.m_la;
    Ok(())
}

pub fn z_erofs_fill_inode_lazy(vi: &mut Inode) -> Result<()> {
    if vi.z_inited {
        return Ok(());
    }

    let pos = round_up(vi.iloc() + vi.inode_isize as u64 + vi.xattr_isize as u64, 8);
    let blk = vi.read_meta_cached(pos)?;
    let h = &blk[vi.sbi.blkoff(pos) as usize..];

    if h[7] >> Z_EROFS_FRAGMENT_INODE_BIT != 0 {
        vi.z_advise = Z_EROFS_ADVISE_FRAGMENT_PCLUSTER;
        vi.z_fragmentoff = u64::from_le_bytes(h[0..8].try_into().unwrap()) ^ (1u64 << 63);
        vi.z_tailextent_headlcn = 0;
        vi.z_inited = true;
        return Ok(());
    }

    vi.z_advise = get_unaligned_le16(h, 4);
    vi.z_lclusterbits = (vi.sbi.blkszbits + (h[7] & 15)) as u32;
    if vi.datalayout == EROFS_INODE_COMPRESSED_FULL && vi.z_advise & Z_EROFS_ADVISE_EXTENTS != 0 {
        vi.z_extents = get_unaligned_le32(h, 0) as u64 | (get_unaligned_le16(h, 6) as u64) << 32;
        vi.z_inited = true;
        return Ok(());
    }
    vi.z_algorithmtype[0] = h[6] & 15;
    vi.z_algorithmtype[1] = h[6] >> 4;
    if vi.z_advise & Z_EROFS_ADVISE_FRAGMENT_PCLUSTER != 0 {
        vi.z_fragmentoff = get_unaligned_le32(h, 0) as u64;
    } else if vi.z_advise & Z_EROFS_ADVISE_INLINE_PCLUSTER != 0 {
        vi.z_idata_size = get_unaligned_le16(h, 2);
    }

    if vi.datalayout == EROFS_INODE_COMPRESSED_COMPACT
        && (vi.z_advise & Z_EROFS_ADVISE_BIG_PCLUSTER_1 == 0)
            != (vi.z_advise & Z_EROFS_ADVISE_BIG_PCLUSTER_2 == 0)
    {
        return Err(Error::efscorrupted());
    }

    if vi.z_idata_size != 0 || vi.z_advise & Z_EROFS_ADVISE_FRAGMENT_PCLUSTER != 0 {
        let mut map = MapBlocks::default();
        z_erofs_map_blocks_fo(vi, &mut map, EROFS_GET_BLOCKS_FINDTAIL)?;
    }
    vi.z_inited = true;
    Ok(())
}

fn z_erofs_map_sanity_check(vi: &Inode, map: &MapBlocks) -> Result<()> {
    if !(map.m_flags & EROFS_MAP_ENCODED != 0) {
        return Ok(());
    }
    if map.m_algorithmformat >= Z_EROFS_COMPRESSION_RUNTIME_MAX {
        return Err(Error::eopnotsupp());
    }
    if map.m_algorithmformat < Z_EROFS_COMPRESSION_MAX
        && vi.sbi.available_compr_algs & (1 << map.m_algorithmformat) == 0
    {
        return Err(Error::efscorrupted());
    }
    if map.m_plen > Z_EROFS_PCLUSTER_MAX_SIZE || map.m_llen > Z_EROFS_PCLUSTER_MAX_DSIZE {
        return Err(Error::eopnotsupp());
    }
    Ok(())
}

pub fn z_erofs_map_blocks_iter(vi: &mut Inode, map: &mut MapBlocks, flags: u32) -> Result<()> {
    if map.m_la >= vi.i_size {
        map.m_llen = map.m_la + 1 - vi.i_size;
        map.m_la = vi.i_size;
        map.m_flags = 0;
    } else {
        let err = z_erofs_fill_inode_lazy(vi);
        if err.is_ok() {
            let r = if vi.datalayout == EROFS_INODE_COMPRESSED_FULL
                && vi.z_advise & Z_EROFS_ADVISE_EXTENTS != 0
            {
                z_erofs_map_blocks_ext(vi, map, flags)
            } else {
                z_erofs_map_blocks_fo(vi, map, flags)
            };
            if r.is_err() {
                map.m_llen = 0;
                return r;
            }
        } else {
            map.m_llen = 0;
            return err;
        }
        let r = z_erofs_map_sanity_check(vi, map);
        if r.is_err() {
            map.m_llen = 0;
            return r;
        }
    }
    Ok(())
}
