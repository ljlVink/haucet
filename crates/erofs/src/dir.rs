use std::sync::Arc;

use crate::data::inode_pread;
use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::inode::{Inode, s_isdir};
use crate::sb::SbInfo;

pub struct Dirent {
    pub nid: u64,
    pub name: Vec<u8>,
    pub file_type: u8,
    pub is_dot_dotdot: bool,
}

fn is_dot_dotdot(name: &[u8]) -> bool {
    if !name.is_empty() && name[0] != b'.' {
        return false;
    }
    name.len() == 1 || (name.len() == 2 && name[1] == b'.')
}

fn traverse_dirents<F: FnMut(&Dirent) -> Result<()>>(
    _sbi: &SbInfo,
    _dir_nid: u64,
    dentry_blk: &[u8],
    mut next_nameoff: usize,
    maxsize: usize,
    cb: &mut F,
) -> Result<()> {
    let mut de = 0usize;
    let end = next_nameoff;
    while de < end {
        if de + 12 > end {
            return Err(Error::efscorrupted());
        }
        let nameoff = get_unaligned_le16(dentry_blk, de + 8) as usize;

        let de_namelen = if de + 12 >= end {
            let mut n = 0usize;
            while nameoff + n < maxsize && dentry_blk[nameoff + n] != 0 {
                n += 1;
            }
            n
        } else {
            get_unaligned_le16(dentry_blk, de + 12 + 8) as usize - nameoff
        };

        if nameoff != next_nameoff {
            return Err(Error::efscorrupted());
        }
        if nameoff + de_namelen > maxsize || de_namelen == 0 || de_namelen > EROFS_NAME_LEN as usize
        {
            return Err(Error::efscorrupted());
        }

        let de_nid = u64::from_le_bytes(dentry_blk[de..de + 8].try_into().unwrap());
        let file_type = dentry_blk[de + 10];
        let name = dentry_blk[nameoff..nameoff + de_namelen].to_vec();
        let dot = is_dot_dotdot(&name);
        if !dot {
            crate::platform::validate_image_component(&name)?;
        }

        let ent = Dirent {
            nid: de_nid,
            name,
            file_type,
            is_dot_dotdot: dot,
        };
        cb(&ent)?;

        next_nameoff += de_namelen;
        de += 12;
    }
    Ok(())
}

pub fn iterate_dir<F: FnMut(&Dirent) -> Result<()>>(
    sbi: &SbInfo,
    dir: &mut Inode,
    mut cb: F,
) -> Result<()> {
    if !s_isdir(dir.i_mode) {
        return Err(Error::errno(-libc::ENOTDIR));
    }

    let blksz = sbi.blksiz() as u64;
    let mut buf = vec![0u8; blksz as usize];
    let mut pos: u64 = 0;
    while pos < dir.i_size {
        let maxsize = std::cmp::min(dir.i_size - pos, blksz) as usize;
        inode_pread(dir, &mut buf[..maxsize], pos)?;

        let nameoff = get_unaligned_le16(&buf, 8) as usize;
        if nameoff < 12 || nameoff >= blksz as usize {
            return Err(Error::efscorrupted());
        }
        traverse_dirents(sbi, dir.nid, &buf[..maxsize], nameoff, maxsize, &mut cb)?;
        pos += maxsize as u64;
    }
    Ok(())
}

fn find_target_dirent(
    dentry_blk: &[u8],
    name: &[u8],
    nameoff: usize,
    maxsize: usize,
) -> Result<Option<u64>> {
    let mut de = 0usize;
    let end = nameoff;
    while de < end {
        if de + 12 > end {
            return Err(Error::efscorrupted());
        }
        let de_nameoff = get_unaligned_le16(dentry_blk, de + 8) as usize;

        let de_namelen = if de + 12 >= end {
            let mut n = 0usize;
            while de_nameoff + n < maxsize && dentry_blk[de_nameoff + n] != 0 {
                n += 1;
            }
            n
        } else {
            get_unaligned_le16(dentry_blk, de + 12 + 8) as usize - de_nameoff
        };

        if de_nameoff + de_namelen > maxsize || de_namelen > EROFS_NAME_LEN as usize {
            return Err(Error::efscorrupted());
        }

        if name.len() == de_namelen && name == &dentry_blk[de_nameoff..de_nameoff + de_namelen] {
            return Ok(Some(u64::from_le_bytes(
                dentry_blk[de..de + 8].try_into().unwrap(),
            )));
        }
        de += 12;
    }
    Ok(None)
}

pub fn erofs_namei(sbi: &Arc<SbInfo>, nid: &mut u64, name: &[u8]) -> Result<()> {
    let mut vi = Inode::new(sbi.clone(), *nid);
    vi.read_from_disk()?;

    let blksz = sbi.blksiz() as u64;
    let mut buf = vec![0u8; blksz as usize];
    let mut offset: u64 = 0;
    while offset < vi.i_size {
        let maxsize = std::cmp::min(vi.i_size - offset, blksz) as usize;
        inode_pread(&mut vi, &mut buf[..maxsize], offset)?;

        let nameoff = get_unaligned_le16(&buf, 8) as usize;
        if nameoff < 12 || nameoff >= blksz as usize {
            return Err(Error::efscorrupted());
        }

        if let Some(found) = find_target_dirent(&buf[..maxsize], name, nameoff, maxsize)? {
            *nid = found;
            return Ok(());
        }
        offset += maxsize as u64;
    }
    Err(Error::errno(-libc::ENOENT))
}

pub fn erofs_ilookup(sbi: &Arc<SbInfo>, path: &str) -> Result<Inode> {
    let mut nid = sbi.root_nid;

    let mut name = path.trim_start_matches('/');
    while !name.is_empty() {
        let (comp, rest) = match name.find('/') {
            Some(i) => (&name[..i], &name[i + 1..]),
            None => (name, ""),
        };
        erofs_namei(sbi, &mut nid, comp.as_bytes())?;
        name = rest.trim_start_matches('/');
    }

    let mut vi = Inode::new(sbi.clone(), nid);
    vi.read_from_disk()?;
    Ok(vi)
}

const EROFS_PATHNAME_FOUND: i32 = 1;

fn pathname_iter(
    sbi: &Arc<SbInfo>,
    target_nid: u64,
    path: &mut Vec<u8>,
    ent: &Dirent,
) -> Result<()> {
    if ent.is_dot_dotdot {
        return Ok(());
    }

    if ent.nid == target_nid {
        path.push(b'/');
        path.extend_from_slice(&ent.name);
        return Err(Error::errno(EROFS_PATHNAME_FOUND));
    }

    if ent.file_type == EROFS_FT_DIR || ent.file_type == EROFS_FT_UNKNOWN {
        let mut dir = Inode::new(sbi.clone(), ent.nid);
        dir.read_from_disk()?;
        if s_isdir(dir.i_mode) {
            let saved = path.len();
            path.push(b'/');
            path.extend_from_slice(&ent.name);
            let ret = iterate_dir(sbi, &mut dir, |d| pathname_iter(sbi, target_nid, path, d));
            match ret {
                Err(Error(e)) if e == EROFS_PATHNAME_FOUND => {
                    return Err(Error::errno(EROFS_PATHNAME_FOUND));
                }
                Err(e) => {
                    path.truncate(saved);
                    return Err(e);
                }
                Ok(()) => {
                    path.truncate(saved);
                }
            }
        }
    }
    Ok(())
}

pub fn erofs_get_pathname(sbi: &Arc<SbInfo>, nid: u64) -> Result<String> {
    if nid == sbi.root_nid {
        return Ok("/".to_string());
    }

    let mut root = Inode::new(sbi.clone(), sbi.root_nid);
    root.read_from_disk()?;

    let mut path: Vec<u8> = Vec::new();

    let ret = iterate_dir(sbi, &mut root, |d| pathname_iter(sbi, nid, &mut path, d));
    match ret {
        Err(Error(e)) if e == EROFS_PATHNAME_FOUND => {
            Ok(String::from_utf8_lossy(&path).into_owned())
        }
        Ok(()) => Err(Error::errno(-libc::ENOENT)),
        Err(e) => Err(e),
    }
}
