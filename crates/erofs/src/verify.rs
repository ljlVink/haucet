use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::erofs_fs::*;
use crate::inode::{S_IFLNK, S_IFMT, S_IFREG};
use crate::metadata::MetadataManifest;

#[derive(Debug, Serialize)]
pub struct ImageInventory {
    pub metadata: MetadataManifest,
    pub sha256: BTreeMap<String, String>,
}

/// Validate superblock bounds/checksum, directory traversal and every data extent.
pub fn verify_image(image: &Path) -> Result<()> {
    image_inventory(image).map(|_| ())
}

/// Read every file in bounded buffers, preserving metadata and content digests.
pub fn image_inventory(image: &Path) -> Result<ImageInventory> {
    let path = image.to_str().context("image path is not UTF-8")?;
    let device = crate::io::Device::open(path, 0)?;
    let image_size = device.file.metadata()?.len();
    ensure!(
        image_size >= EROFS_SUPER_OFFSET + 128,
        "truncated EROFS superblock"
    );
    let sb = Arc::new(crate::sb::erofs_read_superblock(device)?);
    let filesystem_size = sb
        .primarydevice_blocks
        .checked_mul(sb.blksiz() as u64)
        .context("filesystem size overflow")?;
    ensure!(
        filesystem_size <= image_size && filesystem_size >= EROFS_SUPER_OFFSET + sb.sb_size as u64,
        "EROFS filesystem size {filesystem_size} exceeds image length {image_size} or is too small"
    );
    if sb.has_compat(EROFS_FEATURE_COMPAT_SB_CHKSUM) {
        let checksum_end =
            (EROFS_SUPER_OFFSET + 1).div_ceil(sb.blksiz() as u64) * sb.blksiz() as u64;
        let mut bytes = vec![0; (checksum_end - EROFS_SUPER_OFFSET) as usize];
        sb.dev.read_at(&mut bytes, EROFS_SUPER_OFFSET)?;
        bytes[4..8].fill(0);
        ensure!(
            !crc32c::crc32c(&bytes) == sb.checksum,
            "EROFS superblock checksum mismatch"
        );
    }
    let mut nodes = Vec::new();
    crate::node::init_erofs_node_by_root(&mut nodes, &sb)
        .context("reading EROFS directory tree")?;
    for node in &mut nodes {
        validate_extents(&mut node.inode, filesystem_size)
            .with_context(|| format!("validating extents for {}", node.path))?;
    }
    if sb.packed_nid != 0 {
        let mut packed = crate::inode::Inode::new(sb.clone(), sb.packed_nid);
        packed.read_from_disk()?;
        validate_extents(&mut packed, filesystem_size).context("validating packed inode")?;
    }
    let metadata = crate::metadata::from_nodes(&nodes, &sb)?;
    let mut sha256 = BTreeMap::new();
    let mut by_inode = BTreeMap::new();
    let mut buffer = vec![0; 8 * 1024 * 1024];
    for mut node in nodes {
        if !matches!(node.inode.i_mode & S_IFMT, S_IFREG | S_IFLNK) {
            continue;
        }
        if let Some(digest) = by_inode.get(&node.inode.nid) {
            sha256.insert(node.path, String::clone(digest));
            continue;
        }
        let mut hasher = Sha256::new();
        let mut offset = 0;
        while offset < node.inode.i_size {
            let size = (node.inode.i_size - offset).min(buffer.len() as u64) as usize;
            crate::data::inode_pread(&mut node.inode, &mut buffer[..size], offset)
                .with_context(|| format!("reading {} at offset {offset}", node.path))?;
            hasher.update(&buffer[..size]);
            offset += size as u64;
        }
        let digest = format!("{:x}", hasher.finalize());
        by_inode.insert(node.inode.nid, digest.clone());
        sha256.insert(node.path, digest);
    }
    Ok(ImageInventory { metadata, sha256 })
}

fn validate_extents(inode: &mut crate::inode::Inode, filesystem_size: u64) -> Result<()> {
    let metadata_end = inode
        .iloc()
        .checked_add(inode.inode_isize as u64)
        .and_then(|end| end.checked_add(inode.xattr_isize as u64))
        .context("inode metadata offset overflow")?;
    ensure!(
        metadata_end <= filesystem_size,
        "inode metadata extends beyond filesystem"
    );
    if !matches!(
        inode.i_mode & S_IFMT,
        S_IFREG | S_IFLNK | crate::inode::S_IFDIR
    ) {
        return Ok(());
    }
    let mut offset = 0;
    while offset < inode.i_size {
        let mut map = crate::data::MapBlocks {
            m_la: offset,
            ..Default::default()
        };
        crate::data::erofs_map_blocks(inode, &mut map, EROFS_GET_BLOCKS_FIEMAP)?;
        let end = map
            .m_la
            .checked_add(map.m_llen)
            .context("logical extent overflow")?;
        ensure!(
            map.m_la <= offset && end > offset,
            "invalid or empty data extent"
        );
        if map.m_flags & EROFS_MAP_FRAGMENT_BIT != 0 {
            ensure!(
                inode.sbi.packed_nid != 0 && inode.sbi.packed_nid != inode.nid,
                "missing or self-referencing packed inode"
            );
            let mut packed = crate::inode::Inode::new(inode.sbi.clone(), inode.sbi.packed_nid);
            packed.read_from_disk()?;
            let length = map.m_llen.min(inode.i_size - map.m_la);
            ensure!(
                inode
                    .z_fragmentoff
                    .checked_add(length)
                    .is_some_and(|end| end <= packed.i_size),
                "fragment extends beyond packed inode"
            );
        }
        if map.m_flags & EROFS_MAP_MAPPED != 0 && map.m_flags & EROFS_MAP_FRAGMENT_BIT == 0 {
            ensure!(map.m_deviceid == 0, "external-device extent is unsupported");
            ensure!(
                map.m_pa
                    .checked_add(map.m_plen)
                    .is_some_and(|end| end <= filesystem_size),
                "data extent extends beyond filesystem"
            );
        }
        offset = end;
    }
    Ok(())
}
