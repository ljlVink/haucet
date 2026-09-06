pub const EROFS_SUPER_MAGIC_V1: u32 = 0xE0F5_E1E2;
pub const EROFS_SUPER_OFFSET: u64 = 1024;
pub const EROFS_MAX_BLOCK_SIZE: u32 = 4096;
pub const EROFS_ISLOTBITS: u32 = 5;
pub const EROFS_SLOTSIZE: u32 = 1 << EROFS_ISLOTBITS;
pub const EROFS_SB_EXTSLOT_SIZE: u32 = 16;
pub const EROFS_DEVT_SLOT_SIZE: u32 = 128;
pub const EROFS_FEATURE_COMPAT_SB_CHKSUM: u32 = 0x0000_0001;
pub const EROFS_FEATURE_COMPAT_MTIME: u32 = 0x0000_0002;
pub const EROFS_FEATURE_COMPAT_XATTR_FILTER: u32 = 0x0000_0004;
pub const EROFS_FEATURE_COMPAT_SHARED_EA_IN_METABOX: u32 = 0x0000_0008;
pub const EROFS_FEATURE_COMPAT_PLAIN_XATTR_PFX: u32 = 0x0000_0010;
pub const EROFS_FEATURE_COMPAT_ISHARE_XATTRS: u32 = 0x0000_0020;
pub const EROFS_FEATURE_INCOMPAT_LZ4_0PADDING: u32 = 0x0000_0001;
pub const EROFS_FEATURE_INCOMPAT_COMPR_CFGS: u32 = 0x0000_0002;
pub const EROFS_FEATURE_INCOMPAT_BIG_PCLUSTER: u32 = 0x0000_0002;
pub const EROFS_FEATURE_INCOMPAT_CHUNKED_FILE: u32 = 0x0000_0004;
pub const EROFS_FEATURE_INCOMPAT_DEVICE_TABLE: u32 = 0x0000_0008;
pub const EROFS_FEATURE_INCOMPAT_COMPR_HEAD2: u32 = 0x0000_0008;
pub const EROFS_FEATURE_INCOMPAT_ZTAILPACKING: u32 = 0x0000_0010;
pub const EROFS_FEATURE_INCOMPAT_FRAGMENTS: u32 = 0x0000_0020;
pub const EROFS_FEATURE_INCOMPAT_DEDUPE: u32 = 0x0000_0020;
pub const EROFS_FEATURE_INCOMPAT_XATTR_PREFIXES: u32 = 0x0000_0040;
pub const EROFS_FEATURE_INCOMPAT_48BIT: u32 = 0x0000_0080;
pub const EROFS_FEATURE_INCOMPAT_METABOX: u32 = 0x0000_0100;
pub const EROFS_ALL_FEATURE_INCOMPAT: u32 = (EROFS_FEATURE_INCOMPAT_METABOX << 1) - 1;
pub const EROFS_INODE_FLAT_PLAIN: u8 = 0;
pub const EROFS_INODE_COMPRESSED_FULL: u8 = 1;
pub const EROFS_INODE_FLAT_INLINE: u8 = 2;
pub const EROFS_INODE_COMPRESSED_COMPACT: u8 = 3;
pub const EROFS_INODE_CHUNK_BASED: u8 = 4;
pub const EROFS_INODE_DATALAYOUT_MAX: u8 = 5;
pub const EROFS_I_VERSION_MASK: u16 = 0x01;
pub const EROFS_I_DATALAYOUT_MASK: u16 = 0x07;
pub const EROFS_I_VERSION_BIT: u16 = 0;
pub const EROFS_I_DATALAYOUT_BIT: u16 = 1;
pub const EROFS_I_NLINK_1_BIT: u16 = 4;
pub const EROFS_I_DOT_OMITTED_BIT: u16 = 4;
pub const EROFS_I_ALL: u16 = (1 << (EROFS_I_NLINK_1_BIT + 1)) - 1;
pub const EROFS_CHUNK_FORMAT_BLKBITS_MASK: u16 = 0x001F;
pub const EROFS_CHUNK_FORMAT_INDEXES: u16 = 0x0020;
pub const EROFS_CHUNK_FORMAT_48BIT: u16 = 0x0040;
pub const EROFS_CHUNK_FORMAT_ALL: u16 = (EROFS_CHUNK_FORMAT_48BIT << 1) - 1;
pub const EROFS_INODE_LAYOUT_COMPACT: u16 = 0;
pub const EROFS_INODE_LAYOUT_EXTENDED: u16 = 1;
pub const EROFS_XATTR_INDEX_USER: u8 = 1;
pub const EROFS_XATTR_INDEX_POSIX_ACL_ACCESS: u8 = 2;
pub const EROFS_XATTR_INDEX_POSIX_ACL_DEFAULT: u8 = 3;
pub const EROFS_XATTR_INDEX_TRUSTED: u8 = 4;
pub const EROFS_XATTR_INDEX_LUSTRE: u8 = 5;
pub const EROFS_XATTR_INDEX_SECURITY: u8 = 6;
pub const EROFS_XATTR_LONG_PREFIX: u8 = 0x80;
pub const EROFS_XATTR_LONG_PREFIX_MASK: u8 = 0x7f;
pub const EROFS_XATTR_FILTER_BITS: u32 = 32;
pub const EROFS_XATTR_FILTER_DEFAULT: u32 = u32::MAX;
pub const EROFS_XATTR_FILTER_SEED: u32 = 0x25BB_E08F;
pub const EROFS_NULL_ADDR: u64 = u64::MAX;
pub const EROFS_BLOCK_MAP_ENTRY_SIZE: u32 = 4;
pub const EROFS_DIRENT_NID_METABOX_BIT: u64 = 63;
pub const EROFS_DIRENT_NID_MASK: u64 = (1u64 << EROFS_DIRENT_NID_METABOX_BIT) - 1;
pub const EROFS_FT_UNKNOWN: u8 = 0;
pub const EROFS_FT_REG_FILE: u8 = 1;
pub const EROFS_FT_DIR: u8 = 2;
pub const EROFS_FT_CHRDEV: u8 = 3;
pub const EROFS_FT_BLKDEV: u8 = 4;
pub const EROFS_FT_FIFO: u8 = 5;
pub const EROFS_FT_SOCK: u8 = 6;
pub const EROFS_FT_SYMLINK: u8 = 7;
pub const EROFS_FT_MAX: u8 = 8;
pub const EROFS_NAME_LEN: u32 = 255;
pub const Z_EROFS_PCLUSTER_MAX_SIZE: u64 = 1024 * 1024;
pub const Z_EROFS_PCLUSTER_MAX_DSIZE: u64 = 12 * 1024 * 1024;
pub const Z_EROFS_COMPRESSION_LZ4: u8 = 0;
pub const Z_EROFS_COMPRESSION_LZMA: u8 = 1;
pub const Z_EROFS_COMPRESSION_DEFLATE: u8 = 2;
pub const Z_EROFS_COMPRESSION_ZSTD: u8 = 3;
pub const Z_EROFS_COMPRESSION_MAX: u8 = 4;
pub const Z_EROFS_ALL_COMPR_ALGS: u16 = (1 << Z_EROFS_COMPRESSION_MAX) - 1;
pub const Z_EROFS_COMPRESSION_SHIFTED: u8 = Z_EROFS_COMPRESSION_MAX;
pub const Z_EROFS_COMPRESSION_INTERLACED: u8 = Z_EROFS_COMPRESSION_MAX + 1;
pub const Z_EROFS_COMPRESSION_RUNTIME_MAX: u8 = Z_EROFS_COMPRESSION_INTERLACED + 1;
pub const Z_EROFS_ADVISE_COMPACTED_2B: u16 = 0x0001;
pub const Z_EROFS_ADVISE_EXTENTS: u16 = 0x0001;
pub const Z_EROFS_ADVISE_BIG_PCLUSTER_1: u16 = 0x0002;
pub const Z_EROFS_ADVISE_BIG_PCLUSTER_2: u16 = 0x0004;
pub const Z_EROFS_ADVISE_INLINE_PCLUSTER: u16 = 0x0008;
pub const Z_EROFS_ADVISE_INTERLACED_PCLUSTER: u16 = 0x0010;
pub const Z_EROFS_ADVISE_FRAGMENT_PCLUSTER: u16 = 0x0020;
pub const Z_EROFS_ADVISE_EXTRECSZ_BIT: u16 = 1;
pub const Z_EROFS_ADVISE_EXTRECSZ_MASK: u16 = 0x3;
pub const Z_EROFS_FRAGMENT_INODE_BIT: u8 = 7;
pub const Z_EROFS_LCLUSTER_TYPE_PLAIN: u8 = 0;
pub const Z_EROFS_LCLUSTER_TYPE_HEAD1: u8 = 1;
pub const Z_EROFS_LCLUSTER_TYPE_NONHEAD: u8 = 2;
pub const Z_EROFS_LCLUSTER_TYPE_HEAD2: u8 = 3;
pub const Z_EROFS_LCLUSTER_TYPE_MAX: u8 = 4;
pub const Z_EROFS_LI_LCLUSTER_TYPE_MASK: u16 = (Z_EROFS_LCLUSTER_TYPE_MAX - 1) as u16;
pub const Z_EROFS_LI_PARTIAL_REF: u16 = 1 << 15;
pub const Z_EROFS_LI_D0_CBLKCNT: u16 = 1 << 11;
pub const Z_EROFS_EXTENT_PLEN_PARTIAL: u32 = 1 << 27;
pub const Z_EROFS_EXTENT_PLEN_FMT_BIT: u32 = 28;
pub const Z_EROFS_EXTENT_PLEN_MASK: u32 = ((Z_EROFS_PCLUSTER_MAX_SIZE as u32) << 1) - 1;
pub const EROFS_MAP_META: u32 = 1;
pub const EROFS_MAP_MAPPED: u32 = 1 << 1;
pub const EROFS_MAP_ENCODED: u32 = 1 << 2;
pub const EROFS_MAP_FULL_MAPPED: u32 = 1 << 3;
pub const EROFS_MAP_FRAGMENT_BIT: u32 = 1 << 4;
pub const EROFS_MAP_FRAGMENT: u32 = EROFS_MAP_MAPPED | EROFS_MAP_FRAGMENT_BIT;
pub const EROFS_MAP_PARTIAL_REF: u32 = 1 << 5;
pub const EROFS_GET_BLOCKS_FIEMAP: u32 = 0x0002;
pub const EROFS_GET_BLOCKS_FINDTAIL: u32 = 0x0008;
pub const EROFS_INODE_COMPACT_SIZE: u32 = 32;
pub const EROFS_INODE_EXTENDED_SIZE: u32 = 64;
pub const EROFS_XATTR_IBODY_HEADER_SIZE: u32 = 12;

pub fn erofs_inode_is_data_compressed(datamode: u8) -> bool {
    datamode == EROFS_INODE_COMPRESSED_COMPACT || datamode == EROFS_INODE_COMPRESSED_FULL
}

pub fn erofs_inode_version(ifmt: u16) -> u16 {
    (ifmt >> EROFS_I_VERSION_BIT) & EROFS_I_VERSION_MASK
}

pub fn erofs_inode_datalayout(ifmt: u16) -> u8 {
    ((ifmt >> EROFS_I_DATALAYOUT_BIT) & EROFS_I_DATALAYOUT_MASK) as u8
}

pub fn erofs_xattr_ibody_size(icount: u16) -> u32 {
    if icount == 0 {
        0
    } else {
        EROFS_XATTR_IBODY_HEADER_SIZE + 4 * (icount as u32 - 1)
    }
}

pub fn erofs_xattr_entry_size(e_name_len: u8, e_value_size: u16) -> u32 {
    round_up(4u64 + e_name_len as u64 + e_value_size as u64, 4) as u32
}

pub fn z_erofs_map_header_start(end: u64) -> u64 {
    round_up(end, 8)
}

pub fn z_erofs_map_header_end(end: u64) -> u64 {
    z_erofs_map_header_start(end) + 8
}

pub fn z_erofs_full_index_start(end: u64) -> u64 {
    z_erofs_map_header_end(end) + 8
}

pub fn z_erofs_extent_recsize(advise: u16) -> u64 {
    4 << ((advise >> Z_EROFS_ADVISE_EXTRECSZ_BIT) & Z_EROFS_ADVISE_EXTRECSZ_MASK)
}

pub fn round_up(x: u64, n: u64) -> u64 {
    x.div_ceil(n) * n
}

pub fn round_down(x: u64, n: u64) -> u64 {
    (x / n) * n
}

pub fn ilog2(x: u32) -> u32 {
    if x == 0 {
        return 0;
    }
    31 - x.leading_zeros()
}

pub fn blk_round_up(blkszbits: u8, size: u64) -> u64 {
    round_up(size, 1u64 << blkszbits) >> blkszbits
}

pub fn get_unaligned_le32(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap())
}

pub fn get_unaligned_le16(buf: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap())
}
