use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::inode::Inode;

pub const XATTR_USER_PREFIX: &str = "user.";
pub const XATTR_TRUSTED_PREFIX: &str = "trusted.";
pub const XATTR_SECURITY_PREFIX: &str = "security.";
pub const XATTR_NAME_POSIX_ACL_ACCESS: &str = "system.posix_acl_access";
pub const XATTR_NAME_POSIX_ACL_DEFAULT: &str = "system.posix_acl_default";
pub const XATTR_NAME_SELINUX: &str = "security.selinux";
pub const XATTR_NAME_CAPABILITY: &str = "security.capability";

static XATTR_TYPES: [Option<&str>; 7] = [
    Some(""),
    Some(XATTR_USER_PREFIX),
    Some(XATTR_NAME_POSIX_ACL_ACCESS),
    Some(XATTR_NAME_POSIX_ACL_DEFAULT),
    Some(XATTR_TRUSTED_PREFIX),
    None,
    Some(XATTR_SECURITY_PREFIX),
];

pub fn xattr_prefix_for_index(index: u8) -> Option<&'static str> {
    XATTR_TYPES.get(index as usize).copied().flatten()
}

pub fn erofs_xattr_prefix_matches(key: &str) -> Option<(u8, usize)> {
    for (i, p) in XATTR_TYPES.iter().enumerate().skip(1) {
        if let Some(prefix) = p
            && key.starts_with(prefix)
        {
            return Some((i as u8, prefix.len()));
        }
    }
    None
}

fn read_meta_bytes(vi: &Inode, pos: u64, len: usize, out: &mut Vec<u8>) -> Result<()> {
    let sbi = &vi.sbi;
    out.clear();
    out.resize(len, 0);
    let mut done = 0usize;
    while done < len {
        let cur = pos + done as u64;
        let blk = sbi.read_meta(cur)?;
        let off = sbi.blkoff(cur) as usize;
        let cnt = std::cmp::min(sbi.blksiz() as usize - off, len - done);
        out[done..done + cnt].copy_from_slice(&blk[off..off + cnt]);
        done += cnt;
    }
    Ok(())
}

fn erofs_init_inode_xattrs(vi: &mut Inode) -> Result<()> {
    if vi.xattr_inited {
        return Ok(());
    }

    if vi.xattr_isize == EROFS_XATTR_IBODY_HEADER_SIZE {
        return Err(Error::eopnotsupp());
    } else if vi.xattr_isize < EROFS_XATTR_IBODY_HEADER_SIZE {
        if vi.xattr_isize != 0 {
            return Err(Error::efscorrupted());
        }
        return Err(Error::enodata());
    }

    let mut header = Vec::new();
    read_meta_bytes(
        vi,
        vi.iloc() + vi.inode_isize as u64,
        EROFS_XATTR_IBODY_HEADER_SIZE as usize,
        &mut header,
    )?;

    let h_shared_count = header[4] as u32;
    let mut shared = Vec::with_capacity(h_shared_count as usize);
    for i in 0..h_shared_count {
        let mut idbuf = Vec::new();
        read_meta_bytes(
            vi,
            vi.iloc() + vi.inode_isize as u64 + EROFS_XATTR_IBODY_HEADER_SIZE as u64 + 4 * i as u64,
            4,
            &mut idbuf,
        )?;
        shared.push(u32::from_le_bytes(idbuf[0..4].try_into().unwrap()));
    }
    vi.xattr_shared_count = h_shared_count;
    vi.xattr_shared_xattrs = shared;
    vi.xattr_inited = true;
    Ok(())
}

struct XattrIterCtx<'a> {
    name: &'a str,
    index: u8,
    buffer: Option<&'a mut [u8]>,
    buffer_ofs: usize,
    infix_len: usize,
}

fn erofs_getxattr_foreach(vi: &Inode, it: &mut XattrIterCtx, pos: &mut u64) -> Result<bool> {
    let mut entry = Vec::new();
    read_meta_bytes(vi, *pos, 4, &mut entry)?;
    *pos += 4;

    let e_name_len = entry[0] as usize;
    let e_name_index = entry[1];
    let value_sz = u16::from_le_bytes(entry[2..4].try_into().unwrap()) as usize;

    /* should also match the infix for long name prefixes */
    let infix_len = if e_name_index & EROFS_XATTR_LONG_PREFIX != 0 {
        let idx = (e_name_index & EROFS_XATTR_LONG_PREFIX_MASK) as usize;
        let pf = vi.sbi.xattr_prefixes.get(idx).ok_or(Error::enodata())?;

        if it.index != pf.base_index || it.name.len() != e_name_len + pf.infix.len() {
            return Ok(false);
        }
        if !it.name.as_bytes().starts_with(&pf.infix) {
            return Ok(false);
        }
        pf.infix.len()
    } else {
        if it.index != e_name_index || it.name.len() != e_name_len {
            return Ok(false);
        }
        0
    };
    it.infix_len = infix_len;

    /* 2. handle xattr name */
    let mut namebuf = Vec::new();
    read_meta_bytes(vi, *pos, e_name_len, &mut namebuf)?;
    *pos += e_name_len as u64;
    if it.name.as_bytes()[infix_len..] != namebuf[..] {
        return Ok(false);
    }

    /* 3. handle xattr value */
    match &mut it.buffer {
        None => {
            it.buffer_ofs = value_sz;
        }
        Some(buffer) => {
            if buffer.len() < value_sz {
                return Err(Error::errno(-libc::ERANGE));
            }
            // read the value directly into the output buffer, block by block
            let mut off = 0usize;
            let buf = &mut **buffer;
            while off < value_sz {
                let cur = *pos + off as u64;
                let blk = vi.sbi.read_meta(cur)?;
                let b = vi.sbi.blkoff(cur) as usize;
                let cnt = std::cmp::min(vi.sbi.blksiz() as usize - b, value_sz - off);
                buf[off..off + cnt].copy_from_slice(&blk[b..b + cnt]);
                off += cnt;
            }
            *pos += value_sz as u64;
            it.buffer_ofs = value_sz;
        }
    }
    Ok(true)
}

fn erofs_xattr_iter_inline(vi: &Inode, it: &mut XattrIterCtx) -> Result<bool> {
    let xattr_header_sz = EROFS_XATTR_IBODY_HEADER_SIZE + 4 * vi.xattr_shared_count;
    if xattr_header_sz >= vi.xattr_isize {
        if xattr_header_sz > vi.xattr_isize {
            return Err(Error::efscorrupted());
        }
        return Err(Error::enodata());
    }

    let mut remaining = vi.xattr_isize - xattr_header_sz;
    let mut pos = vi.iloc() + vi.inode_isize as u64 + xattr_header_sz as u64;
    loop {
        let mut entry = Vec::new();
        read_meta_bytes(vi, pos, 4, &mut entry)?;
        let entry_sz = erofs_xattr_entry_size(
            entry[0],
            u16::from_le_bytes(entry[2..4].try_into().unwrap()),
        ) as u64;
        /* xattr on-disk corruption: xattr entry beyond xattr_isize */
        if remaining < entry_sz as u32 {
            return Err(Error::efscorrupted());
        }
        remaining -= entry_sz as u32;
        let next_pos = pos + entry_sz;

        let matched = erofs_getxattr_foreach(vi, it, &mut pos)?;
        if matched {
            return Ok(true);
        }
        pos = next_pos;
        if remaining == 0 {
            break;
        }
    }
    Ok(false)
}

fn erofs_xattr_iter_shared(vi: &Inode, it: &mut XattrIterCtx) -> Result<bool> {
    let sbi = &vi.sbi;
    for i in 0..vi.xattr_shared_count {
        let mut pos =
            sbi.pos(sbi.xattr_blkaddr as u64) + vi.xattr_shared_xattrs[i as usize] as u64 * 4;
        let matched = erofs_getxattr_foreach(vi, it, &mut pos)?;
        if matched {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn getxattr(vi: &mut Inode, name: &str, buffer: &mut [u8], hidden: bool) -> Result<usize> {
    if name.is_empty() {
        return Err(Error::errno(-libc::EINVAL));
    }

    erofs_init_inode_xattrs(vi)?;

    let (prefix, prefixlen) = match erofs_xattr_prefix_matches(name) {
        Some((idx, len)) => (idx, len),
        None => {
            if !hidden {
                return Err(Error::enodata());
            }
            (0, 0)
        }
    };

    let suffix = &name[prefixlen..];
    if suffix.len() > EROFS_NAME_LEN as usize {
        return Err(Error::errno(-libc::ERANGE));
    }

    let mut it = XattrIterCtx {
        name: suffix,
        index: prefix,
        buffer: Some(buffer),
        buffer_ofs: 0,
        infix_len: 0,
    };

    let r = erofs_xattr_iter_inline(vi, &mut it);
    match r {
        Err(Error(e)) if e == -libc::ENODATA => {}
        Err(e) => return Err(e),
        Ok(true) => return Ok(it.buffer_ofs),
        Ok(false) => {}
    }

    let matched = erofs_xattr_iter_shared(vi, &mut it)?;
    if matched {
        return Ok(it.buffer_ofs);
    }
    Err(Error::enodata())
}

pub fn listxattr(_vi: &mut Inode, _buffer: &mut [u8]) -> Result<usize> {
    Err(Error::eopnotsupp())
}
