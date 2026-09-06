use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::io::Device;

pub struct XattrPrefixItem {
    pub base_index: u8,
    pub infix: Vec<u8>,
}

pub struct SbInfo {
    pub dev: Device,
    pub feature_compat: u32,
    pub feature_incompat: u32,
    pub blkszbits: u8,
    pub sb_size: u32,
    pub primarydevice_blocks: u64,
    pub meta_blkaddr: u32,
    pub xattr_blkaddr: u32,
    pub xattr_prefix_start: u32,
    pub xattr_prefix_count: u8,
    pub root_nid: u64,
    pub packed_nid: u64,
    pub inos: u64,
    pub checksum: u32,
    pub epoch: i64,
    pub fixed_nsec: u32,
    pub build_time: u32,
    pub uuid: [u8; 16],
    pub available_compr_algs: u16,
    pub lz4_max_distance: u16,
    pub lz4_max_pclusterblks: u16,
    pub zstd_windowlog: Option<u8>,
    pub deflate_windowbits: Option<u8>,
    pub lzma_dict_size: Option<u32>,
    pub xattr_prefixes: Vec<XattrPrefixItem>,
    pub ishare_xattr_prefix_id: Option<u8>,
    pub has_48bit: bool,
    pub has_metabox: bool,
    pub has_fragments: bool,
}

impl SbInfo {
    pub fn blksiz(&self) -> u32 {
        1u32 << self.blkszbits
    }

    pub fn blkoff(&self, off: u64) -> u32 {
        (off & (self.blksiz() as u64 - 1)) as u32
    }

    pub fn blknr(&self, off: u64) -> u64 {
        off >> self.blkszbits
    }

    pub fn pos(&self, blkaddr: u64) -> u64 {
        blkaddr << self.blkszbits
    }

    pub fn has_compat(&self, flag: u32) -> bool {
        self.feature_compat & flag != 0
    }

    pub fn has_incompat(&self, flag: u32) -> bool {
        self.feature_incompat & flag != 0
    }

    pub fn has_lz4_0padding(&self) -> bool {
        self.has_incompat(EROFS_FEATURE_INCOMPAT_LZ4_0PADDING)
    }

    pub fn has_compr_cfgs(&self) -> bool {
        self.has_incompat(EROFS_FEATURE_INCOMPAT_COMPR_CFGS)
    }

    pub fn has_ishare_xattrs(&self) -> bool {
        self.has_compat(EROFS_FEATURE_COMPAT_ISHARE_XATTRS)
    }

    pub fn has_shared_ea_in_metabox(&self) -> bool {
        self.has_compat(EROFS_FEATURE_COMPAT_SHARED_EA_IN_METABOX)
    }

    pub fn has_plain_xattr_pfx(&self) -> bool {
        self.has_compat(EROFS_FEATURE_COMPAT_PLAIN_XATTR_PFX)
    }

    pub fn read_meta(&self, offset: u64) -> Result<Vec<u8>> {
        crate::io::read_meta_block(&self.dev, self.blksiz(), offset)
    }
}

pub fn check_layout_compatibility(feature: u32) -> Result<()> {
    if feature & !EROFS_ALL_FEATURE_INCOMPAT != 0 {
        return Err(Error::errno(-libc::EINVAL));
    }
    Ok(())
}

fn z_erofs_load_lz4_config(sbi: &mut SbInfo, data: Option<&[u8]>) -> Result<()> {
    match data {
        Some(cfg) => {
            if cfg.len() < 16 {
                return Err(Error::errno(-libc::EINVAL));
            }
            let distance = get_unaligned_le16(cfg, 0);
            sbi.lz4_max_pclusterblks = get_unaligned_le16(cfg, 2);
            if sbi.lz4_max_pclusterblks == 0 {
                sbi.lz4_max_pclusterblks = 1; /* reserved case */
            }
            sbi.lz4_max_distance = distance;
        }
        None => {
            // distance was read from sb.u1 already
            if sbi.lz4_max_distance == 0 && !sbi.has_lz4_0padding() {
                return Ok(());
            }
            sbi.lz4_max_pclusterblks = 1;
            sbi.available_compr_algs = 1 << Z_EROFS_COMPRESSION_LZ4;
        }
    }
    Ok(())
}

pub fn z_erofs_parse_cfgs(sbi: &mut SbInfo, sb: &[u8]) -> Result<()> {
    if !sbi.has_compr_cfgs() {
        // dsb->u1.lz4_max_distance (offset 84)
        sbi.lz4_max_distance = get_unaligned_le16(sb, 84);
        return z_erofs_load_lz4_config(sbi, None);
    }

    sbi.available_compr_algs = get_unaligned_le16(sb, 84);
    if sbi.available_compr_algs & !Z_EROFS_ALL_COMPR_ALGS != 0 {
        return Err(Error::eopnotsupp());
    }

    let mut offset: u64 = EROFS_SUPER_OFFSET + sbi.sb_size as u64;
    let mut algs = sbi.available_compr_algs;
    let mut alg: u8 = 0;
    while algs != 0 {
        if algs & 1 != 0 {
            let data = crate::data::read_metadata_bdi(sbi, &mut offset)?;
            match alg {
                Z_EROFS_COMPRESSION_LZ4 => z_erofs_load_lz4_config(sbi, Some(&data))?,
                Z_EROFS_COMPRESSION_DEFLATE => {
                    if data.len() < 8 {
                        return Err(Error::errno(-libc::EINVAL));
                    }
                    sbi.deflate_windowbits = Some(data[0]);
                }
                Z_EROFS_COMPRESSION_ZSTD => {
                    if data.len() < 8 {
                        return Err(Error::errno(-libc::EINVAL));
                    }
                    sbi.zstd_windowlog = Some(data[1]);
                }
                Z_EROFS_COMPRESSION_LZMA => {
                    if data.len() < 16 {
                        return Err(Error::errno(-libc::EINVAL));
                    }
                    sbi.lzma_dict_size = Some(get_unaligned_le32(&data, 0));
                }
                _ => {}
            }
        }
        algs >>= 1;
        alg += 1;
    }
    Ok(())
}

pub fn erofs_xattr_prefixes_init(sbi: &mut SbInfo) -> Result<()> {
    if sbi.xattr_prefix_count == 0 {
        return Ok(());
    }

    let plain = sbi.has_plain_xattr_pfx();
    if !plain {
        if sbi.has_metabox {
            return Err(Error::eopnotsupp());
        } else if sbi.packed_nid != 0 {
            // long xattr prefixes stored in the packed inode: rare enough
            // to reject for now.
            return Err(Error::eopnotsupp());
        }
    }

    let mut pos: u64 = (sbi.xattr_prefix_start as u64) << 2;
    let mut pfs = Vec::with_capacity(sbi.xattr_prefix_count as usize);
    for _ in 0..sbi.xattr_prefix_count {
        let buf = crate::data::read_metadata_bdi(sbi, &mut pos)?;
        if buf.is_empty() || buf.len() > EROFS_NAME_LEN as usize + 1 {
            return Err(Error::efscorrupted());
        }
        let base_index = buf[0];
        let infix = buf[1..].to_vec();
        pfs.push(XattrPrefixItem { base_index, infix });
    }
    sbi.xattr_prefixes = pfs;
    Ok(())
}

pub fn erofs_read_superblock(dev: Device) -> Result<SbInfo> {
    let mut data = vec![0u8; EROFS_MAX_BLOCK_SIZE as usize];
    dev.read_at(&mut data, 0)?;

    let sb = &data[EROFS_SUPER_OFFSET as usize..];
    if sb.len() < 144 {
        return Err(Error::eio());
    }

    if get_unaligned_le32(sb, 0) != EROFS_SUPER_MAGIC_V1 {
        return Err(Error::errno(-libc::EINVAL));
    }

    let feature_compat = get_unaligned_le32(sb, 8);
    let feature_incompat = get_unaligned_le32(sb, 80);
    check_layout_compatibility(feature_incompat)?;

    let blkszbits = sb[12];
    if blkszbits < 9 || blkszbits > ilog2(EROFS_MAX_BLOCK_SIZE) as u8 {
        return Err(Error::errno(-libc::EINVAL));
    }

    let sb_extslots = sb[13];
    let sb_size = 128u32 + sb_extslots as u32 * EROFS_SB_EXTSLOT_SIZE;
    if sb_size > (data.len() as u64 - EROFS_SUPER_OFFSET) as u32 {
        return Err(Error::errno(-libc::EINVAL));
    }

    let has_48bit = feature_incompat & EROFS_FEATURE_INCOMPAT_48BIT != 0;
    let has_metabox = feature_incompat & EROFS_FEATURE_INCOMPAT_METABOX != 0;
    let has_fragments = feature_incompat & EROFS_FEATURE_INCOMPAT_FRAGMENTS != 0;

    if has_metabox {
        // not supported by this port yet
        return Err(Error::eopnotsupp());
    }

    let extra_devices = get_unaligned_le16(sb, 86);
    if extra_devices != 0 {
        // multi-device images not supported by this port yet
        return Err(Error::eopnotsupp());
    }

    let mut primarydevice_blocks = get_unaligned_le32(sb, 36) as u64;
    let root_nid = if has_48bit {
        let r8 = u64::from_le_bytes(sb[112..120].try_into().unwrap());
        if r8 != 0 {
            primarydevice_blocks |= (get_unaligned_le16(sb, 14) as u64) << 32;
            r8
        } else {
            get_unaligned_le16(sb, 14) as u64
        }
    } else {
        get_unaligned_le16(sb, 14) as u64
    };

    let packed_nid = u64::from_le_bytes(sb[96..104].try_into().unwrap());
    if packed_nid & (1u64 << EROFS_DIRENT_NID_METABOX_BIT) != 0 {
        return Err(Error::efscorrupted());
    }

    let mut sbi = SbInfo {
        dev,
        feature_compat,
        feature_incompat,
        blkszbits,
        sb_size,
        primarydevice_blocks,
        meta_blkaddr: get_unaligned_le32(sb, 40),
        xattr_blkaddr: get_unaligned_le32(sb, 44),
        xattr_prefix_start: get_unaligned_le32(sb, 92),
        xattr_prefix_count: sb[91],
        root_nid,
        packed_nid,
        inos: u64::from_le_bytes(sb[16..24].try_into().unwrap()),
        checksum: get_unaligned_le32(sb, 4),
        epoch: u64::from_le_bytes(sb[24..32].try_into().unwrap()) as i64,
        fixed_nsec: get_unaligned_le32(sb, 32),
        build_time: get_unaligned_le32(sb, 108),
        uuid: sb[48..64].try_into().unwrap(),
        available_compr_algs: 0,
        lz4_max_distance: 0,
        lz4_max_pclusterblks: 1,
        zstd_windowlog: None,
        deflate_windowbits: None,
        lzma_dict_size: None,
        xattr_prefixes: Vec::new(),
        ishare_xattr_prefix_id: None,
        has_48bit,
        has_metabox,
        has_fragments,
    };

    if sbi.has_ishare_xattrs() {
        let id = sb[105];
        if id >= sbi.xattr_prefix_count {
            return Err(Error::efscorrupted());
        }
        sbi.ishare_xattr_prefix_id = Some(id | EROFS_XATTR_LONG_PREFIX);
    }

    z_erofs_parse_cfgs(&mut sbi, sb)?;
    erofs_xattr_prefixes_init(&mut sbi)?;

    Ok(sbi)
}
