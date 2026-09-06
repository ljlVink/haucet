use crate::data::Sb;
use crate::dir::{Dirent, iterate_dir};
use crate::erofs_fs::*;
use crate::error::{Error, Result};
use crate::inode::{Inode, erofs_mode_to_ftype, s_isdir};
use crate::xattr::{self, XATTR_NAME_CAPABILITY, XATTR_NAME_SELINUX};

pub const PATH_MAX: usize = 4096;
const VFS_CAP_REVISION_MASK: u32 = 0xFF00_0000;
const VFS_CAP_REVISION_1: u32 = 0x0100_0000;
const VFS_CAP_REVISION_2: u32 = 0x0200_0000;
const VFS_CAP_REVISION_3: u32 = 0x0300_0000;
const XATTR_CAPS_SZ_1: usize = 12;
const XATTR_CAPS_SZ_2: usize = 20;
const XATTR_CAPS_SZ_3: usize = 24;

pub struct ErofsNode {
    pub path: String,
    pub inode: Inode,
    pub fs_config: String,
    pub selinux_label: String,
    pub selinux_label_config: String,
    pub exception_info: Option<String>,
}

impl ErofsNode {
    pub fn get_type_str(&self) -> &'static str {
        match erofs_mode_to_ftype(self.inode.i_mode) {
            EROFS_FT_DIR => "DIR",
            EROFS_FT_REG_FILE => "FILE",
            EROFS_FT_SYMLINK => "LINK",
            EROFS_FT_CHRDEV => "CHR",
            EROFS_FT_BLKDEV => "BLK",
            EROFS_FT_FIFO => "FIFO",
            EROFS_FT_SOCK => "SOCK",
            _ => "UNKNOWN",
        }
    }

    pub fn get_data_layout_str(&self) -> &'static str {
        match self.inode.datalayout {
            EROFS_INODE_FLAT_PLAIN => "PLAIN",
            EROFS_INODE_FLAT_INLINE => "INLINE",
            EROFS_INODE_CHUNK_BASED => "CHUNK",
            EROFS_INODE_COMPRESSED_FULL => "COMPRESSED_FULL",
            EROFS_INODE_COMPRESSED_COMPACT => "COMPRESSED_COMPACT",
            _ => "UNKNOWN",
        }
    }

    pub fn init_exception_info(&mut self, err_code: i32) -> bool {
        if err_code != 0 && err_code != crate::config::RET_EXTRACT_FAIL_SKIP {
            self.exception_info = Some(format!(
                "err={:3}[{:3}] type={:7} dataLayout={:19} name={}",
                err_code,
                crate::error::Error(err_code).to_string(),
                self.get_type_str(),
                self.get_data_layout_str(),
                self.get_path()
            ));
            return true;
        }
        false
    }

    pub fn get_path(&self) -> &str {
        &self.path
    }

    pub fn get_nid(&self) -> u64 {
        self.inode.nid
    }

    pub fn get_nlink(&self) -> u32 {
        self.inode.i_nlink
    }
}

pub fn handle_special_symbols(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '.' | '+' | '[' | ']' | '*') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub static OTHER_PATHS_IN_ROOT_DIR: [&str; 1] = ["/lost+found"];

fn init_security_context(node: &mut ErofsNode) {
    let mut buf = [0u8; 128];
    match xattr::getxattr(&mut node.inode, XATTR_NAME_SELINUX, &mut buf, false) {
        Ok(len) if len > 0 => {
            node.selinux_label = String::from_utf8_lossy(&buf[..len]).into_owned();
            node.selinux_label_config =
                handle_special_symbols(&format!("{} {}", node.path, node.selinux_label));
        }
        _ => {}
    }

    let mut buf = [0u8; 128];
    match xattr::getxattr(&mut node.inode, XATTR_NAME_CAPABILITY, &mut buf, false) {
        Ok(len) if len > 0 => {
            let capabilities = parse_vfs_cap_data(&buf[..len]);
            if let Some(caps) = capabilities
                && caps != 0
            {
                node.fs_config
                    .push_str(&format!(" capabilities=0x{:X}", caps));
            }
        }
        _ => {}
    }
}

fn parse_vfs_cap_data(data: &[u8]) -> Option<u64> {
    if data.len() < 4 {
        return None;
    }
    let magic_etc = u32::from_le_bytes(data[0..4].try_into().unwrap());
    match magic_etc & VFS_CAP_REVISION_MASK {
        VFS_CAP_REVISION_1 => {
            if data.len() != XATTR_CAPS_SZ_1 {
                return None;
            }
            Some(u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64)
        }
        VFS_CAP_REVISION_2 | VFS_CAP_REVISION_3 => {
            if data.len() == XATTR_CAPS_SZ_2 || data.len() == XATTR_CAPS_SZ_3 {
                let p0 = u32::from_le_bytes(data[4..8].try_into().unwrap()) as u64;
                let p1 = u32::from_le_bytes(data[12..16].try_into().unwrap()) as u64;
                Some(p0 | p1 << 32)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn create_node(nodes: &mut Vec<ErofsNode>, path: &str, inode: &Inode) -> Result<()> {
    let fs_config = format!(
        "{} {} {} {:04o}",
        path,
        inode.i_uid,
        inode.i_gid,
        inode.i_mode & 0o777
    );
    let mut node = ErofsNode {
        path: path.to_string(),
        inode: inode.clone(),
        fs_config,
        selinux_label: String::new(),
        selinux_label_config: String::new(),
        exception_info: None,
    };
    init_security_context(&mut node);
    nodes.push(node);
    Ok(())
}

fn do_iter_node(
    nodes: &mut Vec<ErofsNode>,
    sbi: &Sb,
    path: &mut String,
    ent: &Dirent,
) -> Result<()> {
    if ent.is_dot_dotdot {
        return Ok(());
    }

    if path.len() + ent.name.len() + 1 >= PATH_MAX {
        return Err(Error::eopnotsupp());
    }

    let prev_len = path.len();
    path.push('/');
    path.push_str(&String::from_utf8_lossy(&ent.name));

    let mut dir = Inode::new(sbi.clone(), ent.nid);
    dir.read_from_disk()?;
    if !dir.is_packed_inode() {
        create_node(nodes, path, &dir)?;
    }

    if s_isdir(dir.i_mode) {
        iterate_dir(sbi, &mut dir, |d| do_iter_node(nodes, sbi, path, d))?;
    }

    path.truncate(prev_len);
    Ok(())
}

pub fn init_erofs_node_by_root(nodes: &mut Vec<ErofsNode>, sbi: &Sb) -> Result<()> {
    let mut vi = Inode::new(sbi.clone(), sbi.root_nid);
    vi.read_from_disk()?;

    let mut path = String::new();
    create_node(nodes, "/", &vi)?;

    if s_isdir(vi.i_mode) {
        iterate_dir(sbi, &mut vi, |d| do_iter_node(nodes, sbi, &mut path, d))?;
    }
    Ok(())
}

pub fn init_erofs_node_by_path(
    nodes: &mut Vec<ErofsNode>,
    sbi: &Sb,
    target: &str,
    recursive: bool,
) -> bool {
    let vi = match crate::dir::erofs_ilookup(sbi, target) {
        Ok(vi) => vi,
        Err(_) => {
            crate::log::loge(&format!("path not found: '{}'", target));
            return !nodes.is_empty();
        }
    };

    let target_path = match crate::dir::erofs_get_pathname(sbi, vi.nid) {
        Ok(p) => p,
        Err(_) => return !nodes.is_empty(),
    };

    let mut vi = vi;
    if create_node(nodes, &target_path, &vi).is_err() {
        return !nodes.is_empty();
    }

    if recursive && s_isdir(vi.i_mode) {
        let mut path = if target_path.len() != 1 {
            target_path.clone()
        } else {
            String::new()
        };
        let _ = iterate_dir(sbi, &mut vi, |d| do_iter_node(nodes, sbi, &mut path, d));
    }
    !nodes.is_empty()
}

pub fn init_erofs_node_by_targets(
    nodes: &mut Vec<ErofsNode>,
    sbi: &Sb,
    targets: &[String],
    recursive: bool,
) -> bool {
    for t in targets {
        init_erofs_node_by_path(nodes, sbi, t, recursive);
    }
    !nodes.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_revision_1_capability() {
        let mut data = vec![0u8; XATTR_CAPS_SZ_1];
        data[0..4].copy_from_slice(&VFS_CAP_REVISION_1.to_le_bytes());
        data[4..8].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
        data[8..12].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        assert_eq!(parse_vfs_cap_data(&data), Some(0x89ab_cdef));
    }

    #[test]
    fn parses_revision_3_capability_halves() {
        let mut data = vec![0u8; XATTR_CAPS_SZ_3];
        data[0..4].copy_from_slice(&VFS_CAP_REVISION_3.to_le_bytes());
        data[4..8].copy_from_slice(&0x0123_4567u32.to_le_bytes());
        data[8..12].copy_from_slice(&0xaaaa_aaaau32.to_le_bytes());
        data[12..16].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
        data[16..20].copy_from_slice(&0xbbbb_bbbbu32.to_le_bytes());
        assert_eq!(parse_vfs_cap_data(&data), Some(0x89ab_cdef_0123_4567));
    }

    #[test]
    fn rejects_wrong_capability_size() {
        let mut data = vec![0u8; XATTR_CAPS_SZ_2 - 1];
        data[0..4].copy_from_slice(&VFS_CAP_REVISION_2.to_le_bytes());
        assert_eq!(parse_vfs_cap_data(&data), None);
    }
}
