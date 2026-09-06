use std::cell::RefCell;
use std::sync::Arc;

use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::sb::SbInfo;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;

pub fn s_isdir(mode: u32) -> bool {
    mode & S_IFMT == S_IFDIR
}
pub fn s_isreg(mode: u32) -> bool {
    mode & S_IFMT == S_IFREG
}
pub fn s_islnk(mode: u32) -> bool {
    mode & S_IFMT == S_IFLNK
}

pub fn erofs_mode_to_ftype(mode: u32) -> u8 {
    match mode & S_IFMT {
        S_IFDIR => EROFS_FT_DIR,
        S_IFREG => EROFS_FT_REG_FILE,
        S_IFLNK => EROFS_FT_SYMLINK,
        S_IFCHR => EROFS_FT_CHRDEV,
        S_IFBLK => EROFS_FT_BLKDEV,
        S_IFIFO => EROFS_FT_FIFO,
        S_IFSOCK => EROFS_FT_SOCK,
        _ => EROFS_FT_UNKNOWN,
    }
}

pub fn erofs_new_decode_dev(dev: u32) -> u32 {
    let major = ((dev & 0xfff00) >> 8) as u64;
    let minor = ((dev & 0xff) | ((dev >> 12) & 0xfff00)) as u64;
    ((minor & 0xff) | ((major & 0xfff) << 8) | ((minor & !0xff) << 12) | ((major & !0xfff) << 32))
        as u32
}

#[derive(Clone)]
pub struct Inode {
    pub sbi: Arc<SbInfo>,
    pub nid: u64,
    pub i_mode: u32,
    pub i_size: u64,
    pub i_uid: u32,
    pub i_gid: u32,
    pub i_mtime: u64,
    pub i_mtime_nsec: u32,
    pub i_nlink: u32,
    pub datalayout: u8,
    pub inode_isize: u32,
    pub xattr_isize: u32,
    pub xattr_shared_count: u32,
    pub xattr_shared_xattrs: Vec<u32>,
    pub xattr_inited: bool,
    pub u_blkaddr: u64,
    pub i_rdev: u32,
    pub chunkformat: u16,
    pub chunkbits: u8,
    pub dot_omitted: bool,
    pub z_inited: bool,
    pub z_advise: u16,
    pub z_lclusterbits: u32,
    pub z_algorithmtype: [u8; 2],
    pub z_extents: u64,
    pub z_idata_size: u16,
    pub z_fragmentoff: u64,
    pub z_tailextent_headlcn: u64,
    metadata_cache: RefCell<Option<(u64, Arc<[u8]>)>>,
}

impl Inode {
    pub fn new(sbi: Arc<SbInfo>, nid: u64) -> Inode {
        Inode {
            sbi,
            nid,
            i_mode: 0,
            i_size: 0,
            i_uid: 0,
            i_gid: 0,
            i_mtime: 0,
            i_mtime_nsec: 0,
            i_nlink: 0,
            datalayout: 0,
            inode_isize: 0,
            xattr_isize: 0,
            xattr_shared_count: 0,
            xattr_shared_xattrs: Vec::new(),
            xattr_inited: false,
            u_blkaddr: 0,
            i_rdev: 0,
            chunkformat: 0,
            chunkbits: 0,
            dot_omitted: false,
            z_inited: false,
            z_advise: 0,
            z_lclusterbits: 0,
            z_algorithmtype: [0; 2],
            z_extents: 0,
            z_idata_size: 0,
            z_fragmentoff: 0,
            z_tailextent_headlcn: 0,
            metadata_cache: RefCell::new(None),
        }
    }

    pub fn read_meta_cached(&self, offset: u64) -> Result<Arc<[u8]>> {
        let block_offset = round_down(offset, self.sbi.blksiz() as u64);
        {
            let cache = self.metadata_cache.borrow();
            if let Some((cached_offset, block)) = cache.as_ref()
                && *cached_offset == block_offset
            {
                return Ok(Arc::clone(block));
            }
        }

        let block: Arc<[u8]> = self.sbi.read_meta(offset)?.into();
        *self.metadata_cache.borrow_mut() = Some((block_offset, Arc::clone(&block)));
        Ok(block)
    }

    pub fn clear_metadata_cache(&self) {
        self.metadata_cache.borrow_mut().take();
    }

    pub fn in_metabox(&self) -> bool {
        self.nid >> EROFS_DIRENT_NID_METABOX_BIT != 0
    }

    pub fn iloc(&self) -> u64 {
        let base = if self.in_metabox() {
            0
        } else {
            (self.sbi.meta_blkaddr as u64) << self.sbi.blkszbits
        };
        base + ((self.nid & EROFS_DIRENT_NID_MASK) << EROFS_ISLOTBITS)
    }

    pub fn is_packed_inode(&self) -> bool {
        self.nid == self.sbi.packed_nid && self.sbi.packed_nid != 0
    }

    pub fn read_from_disk(&mut self) -> Result<()> {
        let sbi = self.sbi.clone();
        let iloc = self.iloc();
        let inode_start = sbi.blkoff(iloc) as usize;

        let block = sbi.read_meta(iloc)?;

        let ifmt = get_unaligned_le16(&block, inode_start);
        if ifmt & !EROFS_I_ALL != 0 {
            return Err(Error::eopnotsupp());
        }

        let datalayout = erofs_inode_datalayout(ifmt);
        if datalayout >= EROFS_INODE_DATALAYOUT_MAX {
            return Err(Error::eopnotsupp());
        }

        let mut copied_i_u: [u8; 4] = [0; 4];
        let startblk_hi: u64;
        let mut addrmask: u64 = (1u64 << 48) - 1;

        let dic: Vec<u8> = match erofs_inode_version(ifmt) {
            EROFS_INODE_LAYOUT_EXTENDED => {
                let inode_isize = EROFS_INODE_EXTENDED_SIZE as usize;
                // handle cross-block inodes
                if inode_start + inode_isize <= sbi.blksiz() as usize {
                    block[inode_start..].to_vec()
                } else {
                    let gotten = sbi.blksiz() as usize - inode_start;
                    let mut copied = vec![0u8; inode_isize];
                    copied[..gotten].copy_from_slice(&block[inode_start..]);
                    let next = sbi.read_meta(iloc + sbi.blksiz() as u64)?;
                    let rest = inode_isize - gotten;
                    copied[gotten..].copy_from_slice(&next[..rest]);
                    copied
                }
            }
            EROFS_INODE_LAYOUT_COMPACT => {
                if inode_start + EROFS_INODE_COMPACT_SIZE as usize > sbi.blksiz() as usize {
                    return Err(Error::eopnotsupp());
                }
                block[inode_start..].to_vec()
            }
            _ => return Err(Error::eopnotsupp()),
        };

        match erofs_inode_version(ifmt) {
            EROFS_INODE_LAYOUT_EXTENDED => {
                let icount = get_unaligned_le16(&dic, 2);
                self.inode_isize = EROFS_INODE_EXTENDED_SIZE;
                self.xattr_isize = erofs_xattr_ibody_size(icount);
                self.i_mode = get_unaligned_le16(&dic, 4) as u32;
                copied_i_u.copy_from_slice(&dic[16..20]);
                startblk_hi = get_unaligned_le16(&dic, 6) as u64;
                self.i_uid = get_unaligned_le32(&dic, 24);
                self.i_gid = get_unaligned_le32(&dic, 28);
                self.i_nlink = get_unaligned_le32(&dic, 44);
                self.i_mtime = u64::from_le_bytes(dic[32..40].try_into().unwrap());
                self.i_mtime_nsec = get_unaligned_le32(&dic, 40);
                self.i_size = u64::from_le_bytes(dic[8..16].try_into().unwrap());
            }
            EROFS_INODE_LAYOUT_COMPACT => {
                let icount = get_unaligned_le16(&dic, 2);
                self.inode_isize = EROFS_INODE_COMPACT_SIZE;
                self.xattr_isize = erofs_xattr_ibody_size(icount);
                self.i_mode = get_unaligned_le16(&dic, 4) as u32;
                copied_i_u.copy_from_slice(&dic[16..20]);
                self.i_uid = get_unaligned_le16(&dic, 24) as u32;
                self.i_gid = get_unaligned_le16(&dic, 26) as u32;
                if !s_isdir(self.i_mode) && ((ifmt >> EROFS_I_NLINK_1_BIT) & 1) != 0 {
                    self.i_nlink = 1;
                    startblk_hi = get_unaligned_le16(&dic, 6) as u64;
                } else {
                    self.i_nlink = get_unaligned_le16(&dic, 6) as u32;
                    startblk_hi = 0;
                    addrmask = (1u64 << 32) - 1;
                }
                self.i_mtime = (sbi.epoch + get_unaligned_le32(&dic, 12) as i64) as u64;
                self.i_mtime_nsec = sbi.fixed_nsec;
                self.i_size = get_unaligned_le32(&dic, 8) as u64;
            }
            _ => return Err(Error::eopnotsupp()),
        }

        match self.i_mode & S_IFMT {
            S_IFDIR => {
                self.dot_omitted = (ifmt >> EROFS_I_DOT_OMITTED_BIT) & 1 != 0;
                self.u_blkaddr = get_unaligned_le32(&copied_i_u, 0) as u64 | (startblk_hi << 32);
                if self.datalayout == EROFS_INODE_FLAT_PLAIN
                    && (self.u_blkaddr ^ EROFS_NULL_ADDR) & addrmask == 0
                {
                    self.u_blkaddr = EROFS_NULL_ADDR;
                }
            }
            S_IFREG | S_IFLNK => {
                self.u_blkaddr = get_unaligned_le32(&copied_i_u, 0) as u64 | (startblk_hi << 32);
                if self.datalayout == EROFS_INODE_FLAT_PLAIN
                    && (self.u_blkaddr ^ EROFS_NULL_ADDR) & addrmask == 0
                {
                    self.u_blkaddr = EROFS_NULL_ADDR;
                }
            }
            S_IFCHR | S_IFBLK => {
                self.i_rdev = erofs_new_decode_dev(get_unaligned_le32(&copied_i_u, 0));
            }
            S_IFIFO | S_IFSOCK => {
                self.i_rdev = 0;
            }
            _ => return Err(Error::efscorrupted()),
        }

        self.datalayout = datalayout;
        self.chunkformat = 0;
        self.chunkbits = 0;
        if self.datalayout == EROFS_INODE_CHUNK_BASED {
            self.chunkformat = get_unaligned_le16(&copied_i_u, 0);
            if self.chunkformat & !EROFS_CHUNK_FORMAT_ALL != 0 {
                return Err(Error::eopnotsupp());
            }
            self.chunkbits =
                sbi.blkszbits + (self.chunkformat & EROFS_CHUNK_FORMAT_BLKBITS_MASK) as u8;
        }
        Ok(())
    }
}
