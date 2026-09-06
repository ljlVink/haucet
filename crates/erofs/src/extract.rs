use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};

use sha2::{Digest, Sha256};

use crate::config::{Config, RET_EXTRACT_FAIL_SKIP};
use crate::data::{
    MapBlocks, erofs_map_blocks, erofs_read_one_data, inode_pread, z_erofs_read_one_data,
};
use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::inode::{Inode, erofs_mode_to_ftype, s_islnk, s_isreg};
use crate::log;
use crate::platform;
use crate::xattr;

fn errno_of(error: &std::io::Error) -> i32 {
    -platform::io_error_code(error)
}

pub fn mkdirs(path: &str, mode: u32) -> Result<()> {
    platform::create_dirs(path, mode).map_err(Error::from)
}

fn file_exists(path: &str) -> bool {
    std::fs::metadata(path)
        .map(|m| !m.is_dir())
        .unwrap_or(false)
}

pub fn erofs_verify_inode_data<W: Seek + Write + ?Sized>(
    config: &Config,
    inode: &mut Inode,
    mut outfd: Option<&mut W>,
    mut digest: Option<&mut Sha256>,
) -> Result<()> {
    let mut map = MapBlocks::default();
    let needdecode = config.check_decomp && !inode.is_packed_inode();
    let compressed = erofs_inode_is_data_compressed(inode.datalayout);
    let mut pos: u64 = 0;
    let mut raw: Vec<u8> = Vec::new();
    let mut raw_size = 0usize;
    let mut decoded: Vec<u8> = Vec::new();

    while pos < inode.i_size {
        map.m_la = pos;
        erofs_map_blocks(inode, &mut map, EROFS_GET_BLOCKS_FIEMAP)?;

        if !compressed && map.m_llen != map.m_plen {
            return Err(Error::efscorrupted());
        }

        /* the last lcluster can be divided into 3 parts */
        if map.m_la + map.m_llen > inode.i_size {
            map.m_llen = inode.i_size - map.m_la;
        }

        pos += map.m_llen;

        /* should skip decomp? */
        if map.m_la >= inode.i_size || !needdecode {
            continue;
        }

        if !(map.m_flags & EROFS_MAP_MAPPED != 0) {
            if let Some(d) = digest.as_deref_mut() {
                const ZEROS: [u8; 4096] = [0; 4096];
                let mut remain = map.m_llen;
                while remain > 0 {
                    let chunk = std::cmp::min(remain, ZEROS.len() as u64) as usize;
                    d.update(&ZEROS[..chunk]);
                    remain -= chunk as u64;
                }
            } else if let Some(f) = outfd.as_deref_mut() {
                f.seek(SeekFrom::Current(map.m_llen as i64))?;
            }
            continue;
        }

        let alloc_rawsize = if map.m_plen > Z_EROFS_PCLUSTER_MAX_SIZE {
            if compressed && map.m_flags & EROFS_MAP_FRAGMENT_BIT == 0 {
                return Err(Error::efscorrupted());
            }
            Z_EROFS_PCLUSTER_MAX_SIZE as usize
        } else {
            map.m_plen as usize
        };

        if alloc_rawsize > raw_size {
            raw.resize(alloc_rawsize, 0);
            raw_size = alloc_rawsize;
        }

        if compressed {
            let llen = map.m_llen;
            let decoded_len = llen as usize;
            if decoded_len > decoded.len() {
                decoded.resize(decoded_len, 0);
            }
            let buffer = &mut decoded[..decoded_len];
            z_erofs_read_one_data(inode, &mut map, &mut raw, buffer, 0, llen, false)?;

            if let Some(d) = digest.as_deref_mut() {
                d.update(&*buffer);
            }
            if let Some(f) = outfd.as_deref_mut() {
                f.write_all(buffer)?;
            }
        } else {
            let mut p = 0u64;
            loop {
                let count = std::cmp::min(alloc_rawsize as u64, map.m_llen) as usize;
                erofs_read_one_data(inode, &map, &mut raw[..count], p)?;

                if let Some(d) = digest.as_deref_mut() {
                    d.update(&raw[..count]);
                }
                if let Some(f) = outfd.as_deref_mut() {
                    f.write_all(&raw[..count])?;
                }
                map.m_llen -= count as u64;
                p += count as u64;
                if map.m_llen == 0 {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn verify_file_digest(config: &Config, inode: &mut Inode, digest: &[u8; 32]) -> Result<()> {
    let mut stored = [0u8; 32 + 7];

    let ret = xattr::getxattr(inode, &config.digest_xattr_name, &mut stored, true);
    match ret {
        Err(Error(e)) if e == -libc::ENODATA => return Ok(()),
        Err(e) => return Err(e),
        Ok(len) => {
            if len != stored.len() || !stored.starts_with(b"sha256:") {
                return Err(Error::efscorrupted());
            }
        }
    }

    if digest != &stored[7..] {
        return Err(Error::efscorrupted());
    }
    Ok(())
}

pub fn calc_inode_data<W: Seek + Write + ?Sized>(
    config: &Config,
    inode: &mut Inode,
    outfd: Option<&mut W>,
) -> Result<()> {
    if !config.digest_xattr_name.is_empty() && s_isreg(inode.i_mode) && inode.i_size > 0 {
        let mut md = Sha256::new();
        erofs_verify_inode_data(config, inode, outfd, Some(&mut md))?;
        let out: [u8; 32] = md.finalize().into();
        verify_file_digest(config, inode, &out)
    } else {
        erofs_verify_inode_data(config, inode, outfd, None)
    }
}

pub fn erofs_extract_dir(config: &Config, dir_path: &str) -> Result<()> {
    let mut tryagain = true;
    loop {
        match mkdirs(dir_path, 0o700) {
            Ok(()) => return Ok(()),
            Err(Error(e)) => {
                let errno = -e;
                if config.overwrite && tryagain {
                    if errno == libc::EEXIST {
                        match std::fs::symlink_metadata(dir_path) {
                            Ok(st) if st.is_dir() => {
                                platform::set_mode(dir_path, 0o700).map_err(Error::from)?;
                            }
                            _ => {
                                platform::remove_file(dir_path).map_err(Error::from)?;
                            }
                        }
                    }
                    tryagain = false;
                    continue;
                }

                if errno == libc::EEXIST {
                    match std::fs::symlink_metadata(dir_path) {
                        Ok(st) if st.is_dir() => {}
                        _ => return Err(Error::errno(-libc::ENOTDIR)),
                    }
                }
                return Err(Error(-errno));
            }
        }
    }
}

pub fn erofs_extract_file(config: &Config, inode: &mut Inode, file_path: &str) -> Result<()> {
    let mut tryagain = true;
    loop {
        match platform::open_output_file(file_path, config.overwrite) {
            Ok(file) => {
                const MAX_WRITE_BUFFER: u64 = 1024 * 1024;
                let capacity = inode.i_size.min(MAX_WRITE_BUFFER) as usize;
                let mut writer = BufWriter::with_capacity(capacity, file);
                let mut ret = calc_inode_data(config, inode, Some(&mut writer));
                if ret.is_ok()
                    && let Err(err) = writer.flush()
                {
                    ret = Err(Error::from(err));
                }
                return ret;
            }
            Err(err) => {
                let error_code = platform::io_error_code(&err);
                if config.overwrite && tryagain {
                    if error_code == libc::EISDIR {
                        if platform::remove_dir(file_path).is_err() {
                            return Err(Error::errno(-libc::EISDIR));
                        }
                    } else if error_code == libc::EACCES
                        && platform::set_mode(file_path, 0o700).is_err()
                    {
                        return Err(Error(errno_of(&err)));
                    }
                    tryagain = false;
                    continue;
                }
                if error_code == libc::EEXIST && !config.overwrite {
                    return Err(Error::errno(RET_EXTRACT_FAIL_SKIP));
                }
                return Err(Error(errno_of(&err)));
            }
        }
    }
}

pub fn erofs_extract_symlink(config: &Config, inode: &mut Inode, file_path: &str) -> Result<()> {
    let bufsz = inode
        .i_size
        .checked_add(1)
        .ok_or(Error::errno(-libc::ENOMEM))?;
    let mut buf = vec![0u8; bufsz as usize];

    inode_pread(inode, &mut buf[..inode.i_size as usize], 0)?;
    buf[inode.i_size as usize] = 0;
    let target = String::from_utf8_lossy(&buf[..inode.i_size as usize]).into_owned();

    let target_is_dir = platform::symlink_target_is_dir(&config.out_dir, file_path, &target);
    let mut tryagain = true;
    loop {
        match platform::create_symlink(&target, file_path, target_is_dir, &config.out_dir) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let error_code = platform::io_error_code(&err);
                if error_code == libc::EEXIST && config.overwrite && tryagain {
                    if platform::remove_file(file_path).is_err() {
                        return Err(Error(errno_of(&err)));
                    }
                    tryagain = false;
                    continue;
                }
                if error_code == libc::EEXIST && !config.overwrite {
                    return Err(Error::errno(RET_EXTRACT_FAIL_SKIP));
                }
                return Err(Error(errno_of(&err)));
            }
        }
    }
}

pub fn erofs_extract_hardlink(
    config: &Config,
    inode: &mut Inode,
    src_path: &str,
    target_path: &str,
) -> Result<()> {
    let mut ret = Ok(());
    if !file_exists(src_path) {
        ret = erofs_extract_file(config, inode, src_path);
    }
    if src_path != target_path {
        let mut tryagain = true;
        loop {
            match platform::create_hard_link(src_path, target_path) {
                Ok(()) => break,
                Err(err) => {
                    let error_code = platform::io_error_code(&err);
                    if error_code == libc::EEXIST && config.overwrite && tryagain {
                        if platform::remove_file(target_path).is_err() {
                            return Err(Error(errno_of(&err)));
                        }
                        tryagain = false;
                        continue;
                    }
                    if error_code == libc::EEXIST && !config.overwrite {
                        return Err(Error::errno(RET_EXTRACT_FAIL_SKIP));
                    }
                    return Err(Error(errno_of(&err)));
                }
            }
        }
    }
    ret
}

pub fn erofs_extract_special(config: &Config, inode: &Inode, file_path: &str) -> Result<()> {
    let mut tryagain = true;
    loop {
        match platform::create_special(file_path, inode.i_mode, inode.i_rdev) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let error_code = platform::io_error_code(&err);
                if error_code == libc::EEXIST && config.overwrite && tryagain {
                    if platform::remove_file(file_path).is_err() {
                        return Err(Error(errno_of(&err)));
                    }
                    tryagain = false;
                    continue;
                }
                if error_code == libc::EOPNOTSUPP {
                    return Err(Error::errno(-libc::EOPNOTSUPP));
                }
                if error_code == libc::EEXIST || config.superuser {
                    return Err(Error(errno_of(&err)));
                }
                return Err(Error::errno(-libc::ECANCELED));
            }
        }
    }
}

pub fn set_attributes(config: &Config, inode: &Inode, path: &str) {
    if platform::set_times(path, inode.i_mtime, inode.i_mtime_nsec, true).is_err() {
        log::logw(&format!("failed to set times: {}", path));
    }

    if config.preserve_owner && platform::set_owner(path, inode.i_uid, inode.i_gid).is_err() {
        log::logw(&format!("failed to change ownership: {}", path));
    }

    if !s_islnk(inode.i_mode) {
        let mode: u32 = if inode.i_mode & 0o777 == 0o500 {
            (inode.i_mode & !0o777) | 0o777
        } else {
            inode.i_mode
        };
        let target = if config.preserve_perms {
            mode
        } else {
            mode & !config.umask
        };
        if platform::set_mode(path, target).is_err() {
            log::logw(&format!("failed to set permissions: {}", path));
        }
    }
}

pub fn write_to_file(
    config: &Config,
    node: &mut crate::node::ErofsNode,
    hardlinks: &std::sync::Mutex<std::collections::HashMap<u64, String>>,
) -> i32 {
    let out_dir = config.out_dir.clone();
    let file_path = match platform::join_image_path(&out_dir, &node.path) {
        Ok(path) => path,
        Err(Error(code)) => return code,
    };
    let inode = &mut node.inode;

    // C++: the hardlink lock is held for the whole erofs_extract_hardlink call
    {
        let guard = hardlinks.lock().unwrap();
        if let Some(src) = guard.get(&inode.nid) {
            let source_path = match platform::join_image_path(&out_dir, src) {
                Ok(path) => path,
                Err(Error(code)) => return code,
            };
            let r = erofs_extract_hardlink(config, inode, &source_path, &file_path);
            return match r {
                Ok(()) => 0,
                Err(Error(e)) => e,
            };
        }
    }

    let err: i32 = match erofs_mode_to_ftype(inode.i_mode) {
        EROFS_FT_DIR => match erofs_extract_dir(config, &file_path) {
            Ok(()) => 0,
            Err(Error(e)) => e,
        },
        EROFS_FT_REG_FILE => match erofs_extract_file(config, inode, &file_path) {
            Ok(()) => 0,
            Err(Error(e)) => e,
        },
        EROFS_FT_SYMLINK => match erofs_extract_symlink(config, inode, &file_path) {
            Ok(()) => 0,
            Err(Error(e)) => e,
        },
        EROFS_FT_CHRDEV | EROFS_FT_BLKDEV | EROFS_FT_FIFO | EROFS_FT_SOCK => {
            match erofs_extract_special(config, inode, &file_path) {
                Ok(()) => 0,
                Err(Error(e)) => e,
            }
        }
        _ => -libc::EOPNOTSUPP,
    };

    if err == 0 {
        set_attributes(config, inode, &file_path);
    }
    err
}

pub fn open_truncate(path: &str) -> std::io::Result<File> {
    platform::open_truncate(path)
}
