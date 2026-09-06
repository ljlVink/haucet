use std::sync::Arc;

use crate::data::{Sb, inode_pread};
use crate::error::{Error, Result};
use crate::inode::Inode;
use crate::sb::SbInfo;

pub fn erofs_packedfile_read(sbi: &Sb, buf: &mut [u8], pos: u64) -> Result<()> {
    let len = buf.len() as u64;
    let packed_nid = sbi.packed_nid;
    if packed_nid == 0 {
        return Err(Error::efscorrupted());
    }
    let mut pi = Inode::new(sbi.clone(), packed_nid);
    pi.read_from_disk()?;
    inode_pread(&mut pi, buf, pos)?;
    let _ = len;
    Ok(())
}

pub fn erofs_xattr_get_ishare_prefix(sbi: &Arc<SbInfo>) -> Option<String> {
    if !sbi.has_ishare_xattrs() {
        return None;
    }

    let id = sbi.ishare_xattr_prefix_id?;
    let (base_index, infix) = if id & crate::erofs_fs::EROFS_XATTR_LONG_PREFIX != 0 {
        let idx = (id & crate::erofs_fs::EROFS_XATTR_LONG_PREFIX_MASK) as usize;
        let pf = sbi.xattr_prefixes.get(idx)?;
        (pf.base_index, pf.infix.clone())
    } else {
        (
            id & crate::erofs_fs::EROFS_XATTR_LONG_PREFIX_MASK,
            Vec::new(),
        )
    };

    let base = crate::xattr::xattr_prefix_for_index(base_index)?;
    let mut name = base.to_string();
    if !infix.is_empty() {
        name.push_str(&String::from_utf8_lossy(&infix));
    }
    Some(name)
}
