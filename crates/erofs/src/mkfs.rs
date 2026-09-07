// SPDX-License-Identifier: GPL-3.0-only
// EROFS on-disk encoding follows erofs-utils lib/inode.c, lib/xattr.c and
// lib/super.c (GPL-2.0+ OR MIT): Copyright (C) 2018-2019 HUAWEI, Inc.;
// Copyright (C) 2019 Li Guifu and Gao Xiang; Copyright (C) 2025 Alibaba Cloud.
// Created upstream by Li Guifu, with heavy changes by Gao Xiang.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::build_options::BuildOptions;
use crate::compression::{Compression, compress_file, relocate_indexes};
use crate::erofs_fs::*;
use crate::inode::*;
use crate::metadata::{EntryMetadata, MetadataResolver};

#[derive(Debug, Clone, Default)]
pub struct BuildReport {
    pub inode_count: u64,
    pub file_count: u64,
    pub directory_count: u64,
    pub uncompressed_bytes: u64,
    pub image_bytes: u64,
    pub compressed_files: u64,
}

struct Entry {
    host: PathBuf,
    path: String,
    metadata: EntryMetadata,
    parent: usize,
    children: Vec<(Vec<u8>, usize)>,
    alias: Option<usize>,
    size: u64,
    links: u32,
    inode_size: usize,
    xattrs: Vec<u8>,
    layout: u8,
    data_union: u32,
    inline: Vec<u8>,
    compression: Vec<u8>,
    nid: u64,
}

impl Entry {
    fn body_size(&self) -> u64 {
        let header = (self.inode_size + self.xattrs.len()) as u64;
        if erofs_inode_is_data_compressed(self.layout) {
            round_up(header, 8) + self.compression.len() as u64
        } else {
            header + self.inline.len() as u64
        }
    }

    fn can_inline(&self, block_size: u32, enabled: bool) -> bool {
        let tail = self.size % u64::from(block_size);
        enabled
            && tail != 0
            && self.inode_size as u64 + self.xattrs.len() as u64 + tail <= u64::from(block_size)
    }
}

struct IncompleteImage {
    path: PathBuf,
    keep: bool,
}

impl Drop for IncompleteImage {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Build an EROFS filesystem without invoking an external mkfs process.
///
/// The destination must not exist and its parent must be outside the source.
/// A failed build removes the partial image; successful images are block aligned.
pub fn build(source: &Path, output: &Path, options: &BuildOptions) -> Result<BuildReport> {
    options.validate()?;
    let source = fs::canonicalize(source)
        .with_context(|| format!("resolve source directory {}", source.display()))?;
    ensure!(
        source.is_dir(),
        "source is not a directory: {}",
        source.display()
    );
    let output_name = output.file_name().context("output has no file name")?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("resolve output parent {}", parent.display()))?;
    ensure!(
        !parent.starts_with(&source),
        "output must be outside the source directory"
    );
    let output = parent.join(output_name);
    ensure!(
        fs::symlink_metadata(&output).is_err_and(|e| e.kind() == io::ErrorKind::NotFound),
        "refusing to overwrite output {}",
        output.display()
    );
    let resolver = MetadataResolver::load(options)?;
    let mut entries = Vec::new();
    collect_entries(&source, "/".to_owned(), 0, &resolver, &mut entries)?;
    let epoch = options
        .timestamp
        .unwrap_or_else(|| entries.iter().map(|e| e.metadata.mtime).min().unwrap_or(0));
    resolve_hardlinks(&mut entries)?;
    let shared_xattrs = prepare_entries(&mut entries, options, epoch)?;

    let mut image = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("create EROFS image {}", output.display()))?;
    let mut incomplete = IncompleteImage {
        path: output.clone(),
        keep: false,
    };
    let report = write_filesystem(&mut image, &mut entries, options, epoch, &shared_xattrs)
        .with_context(|| format!("build EROFS image {}", output.display()))?;
    image.sync_all().context("flush completed EROFS image")?;
    drop(image);
    incomplete.keep = true;
    Ok(report)
}

fn collect_entries(
    host: &Path,
    path: String,
    parent: usize,
    resolver: &MetadataResolver,
    entries: &mut Vec<Entry>,
) -> Result<usize> {
    let host_metadata = fs::symlink_metadata(host)
        .with_context(|| format!("read metadata for {}", host.display()))?;
    let metadata = resolver.resolve(&path, host, &host_metadata)?;
    ensure!(
        metadata.mode <= u16::MAX as u32,
        "invalid inode mode for {path}"
    );
    ensure!(
        metadata.mtime_nsec < 1_000_000_000,
        "invalid nanosecond timestamp for {path}"
    );
    ensure!(
        erofs_mode_to_ftype(metadata.mode) != EROFS_FT_UNKNOWN,
        "unsupported file type for {path}"
    );
    let is_directory = s_isdir(metadata.mode);
    ensure!(
        !is_directory || host_metadata.is_dir(),
        "directory metadata does not match source {path}"
    );
    ensure!(
        !s_isreg(metadata.mode) || host_metadata.is_file(),
        "regular file metadata does not match source {path}"
    );
    let size = if s_isreg(metadata.mode) {
        host_metadata.len()
    } else if s_islnk(metadata.mode) {
        metadata
            .symlink
            .as_ref()
            .with_context(|| format!("missing symlink target for {path}"))?
            .len() as u64
    } else {
        0
    };
    let index = entries.len();
    entries.push(Entry {
        host: host.to_owned(),
        path: path.clone(),
        metadata,
        parent,
        children: Vec::new(),
        alias: None,
        size,
        links: if is_directory { 2 } else { 1 },
        inode_size: 64,
        xattrs: Vec::new(),
        layout: EROFS_INODE_FLAT_PLAIN,
        data_union: u32::MAX,
        inline: Vec::new(),
        compression: Vec::new(),
        nid: 0,
    });
    if is_directory {
        let mut children = fs::read_dir(host)
            .with_context(|| format!("read source directory {}", host.display()))?
            .collect::<io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 source name in {}", host.display()))?;
            ensure!(
                !name.is_empty()
                    && name.len() <= EROFS_NAME_LEN as usize
                    && !name.as_bytes().contains(&0),
                "invalid EROFS file name {name:?}"
            );
            let child_path = if path == "/" {
                format!("/{name}")
            } else {
                format!("{path}/{name}")
            };
            let child_index = collect_entries(&child.path(), child_path, index, resolver, entries)?;
            if s_isdir(entries[child_index].metadata.mode) {
                entries[index].links = entries[index]
                    .links
                    .checked_add(1)
                    .context("too many directory links")?;
            }
            entries[index]
                .children
                .push((name.into_bytes(), child_index));
        }
        entries[index].children.sort_by(|a, b| a.0.cmp(&b.0));
    }
    Ok(index)
}

#[derive(Hash, PartialEq, Eq)]
enum LinkIdentity {
    Original(u64),
    #[cfg(any(unix, windows))]
    Host(u64, u64),
}

fn hardlink_identity(entry: &Entry) -> Result<Option<LinkIdentity>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(&entry.host)?;
        if metadata.nlink() > 1 {
            return Ok(Some(LinkIdentity::Host(metadata.dev(), metadata.ino())));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            GetFileInformationByHandle,
        };
        let file = OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&entry.host)
            .with_context(|| format!("open source identity {}", entry.host.display()))?;
        let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: the handle remains open and the output points to writable storage.
        let status =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
        if status == 0 {
            return Err(io::Error::last_os_error()).context("read source file identity");
        }
        // SAFETY: GetFileInformationByHandle initializes the structure on success.
        let information = unsafe { information.assume_init() };
        if information.nNumberOfLinks > 1 {
            let file_id = (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow);
            return Ok(Some(LinkIdentity::Host(
                u64::from(information.dwVolumeSerialNumber),
                file_id,
            )));
        }
    }
    Ok(entry.metadata.original_nid.map(LinkIdentity::Original))
}

fn equivalent_metadata(a: &Entry, b: &Entry) -> bool {
    a.size == b.size
        && a.metadata.mode == b.metadata.mode
        && a.metadata.uid == b.metadata.uid
        && a.metadata.gid == b.metadata.gid
        && a.metadata.mtime == b.metadata.mtime
        && a.metadata.mtime_nsec == b.metadata.mtime_nsec
        && a.metadata.rdev == b.metadata.rdev
        && a.metadata.xattrs == b.metadata.xattrs
        && a.metadata.symlink == b.metadata.symlink
}

fn file_digest(path: &Path) -> Result<[u8; 32]> {
    let mut file = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

fn resolve_hardlinks(entries: &mut [Entry]) -> Result<()> {
    let mut identities: HashMap<LinkIdentity, Vec<usize>> = HashMap::new();
    let mut digests: HashMap<usize, [u8; 32]> = HashMap::new();
    for index in 0..entries.len() {
        if s_isdir(entries[index].metadata.mode) {
            continue;
        }
        let Some(identity) = hardlink_identity(&entries[index])? else {
            continue;
        };
        let candidates = identities.entry(identity).or_default();
        let mut alias = None;
        for &candidate in candidates.iter() {
            if !equivalent_metadata(&entries[index], &entries[candidate]) {
                continue;
            }
            if s_isreg(entries[index].metadata.mode) {
                if let std::collections::hash_map::Entry::Vacant(slot) = digests.entry(index) {
                    slot.insert(file_digest(&entries[index].host)?);
                }
                if let std::collections::hash_map::Entry::Vacant(slot) = digests.entry(candidate) {
                    slot.insert(file_digest(&entries[candidate].host)?);
                }
                if digests[&index] != digests[&candidate] {
                    continue;
                }
            }
            alias = Some(candidate);
            break;
        }
        if let Some(candidate) = alias {
            entries[index].alias = Some(candidate);
            entries[candidate].links = entries[candidate]
                .links
                .checked_add(1)
                .context("too many hardlinks")?;
        } else {
            candidates.push(index);
        }
    }
    Ok(())
}

fn prepare_entries(entries: &mut [Entry], options: &BuildOptions, epoch: u64) -> Result<Vec<u8>> {
    let mut counts = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.alias.is_none()) {
        for (name, value) in &entry.metadata.xattrs {
            *counts
                .entry((name.clone(), value.clone()))
                .or_insert(0usize) += 1;
        }
    }
    let mut shared = BTreeMap::new();
    let mut shared_bytes = Vec::new();
    for ((name, value), count) in counts {
        if count <= options.xattr_tolerance as usize {
            continue;
        }
        let offset =
            u32::try_from(shared_bytes.len() / 4).context("shared xattr table is too large")?;
        encode_xattr(&mut shared_bytes, &name, &value)?;
        shared.insert((name, value), offset);
    }
    for index in 0..entries.len() {
        if entries[index].alias.is_some() {
            continue;
        }
        if s_isdir(entries[index].metadata.mode) {
            entries[index].size =
                directory_size(&directory_entries(entries, index), options.block_size)?;
        }
        let entry = &mut entries[index];
        entry.xattrs = encode_xattrs(&entry.metadata.xattrs, &shared)
            .with_context(|| format!("encode extended attributes for {}", entry.path))?;
        entry.inode_size = if entry.size <= u32::MAX as u64
            && entry.metadata.uid <= u16::MAX as u32
            && entry.metadata.gid <= u16::MAX as u32
            && entry.links <= u16::MAX as u32
            && entry
                .metadata
                .mtime
                .checked_sub(epoch)
                .is_some_and(|time| time <= u32::MAX as u64)
            && entry.metadata.mtime_nsec == 0
        {
            32
        } else {
            64
        };
        if entry.can_inline(options.block_size, options.inline_data) {
            entry.layout = EROFS_INODE_FLAT_INLINE;
            entry
                .inline
                .resize((entry.size % u64::from(options.block_size)) as usize, 0);
        }
    }
    Ok(shared_bytes)
}

fn encode_xattr(result: &mut Vec<u8>, name: &str, value: &[u8]) -> Result<()> {
    let (index, prefix) = crate::xattr::erofs_xattr_prefix_matches(name)
        .with_context(|| format!("unsupported xattr namespace: {name}"))?;
    let suffix = &name.as_bytes()[prefix..];
    ensure!(
        suffix.len() <= u8::MAX as usize && value.len() <= u16::MAX as usize,
        "xattr name or value is too large: {name}"
    );
    result.push(suffix.len() as u8);
    result.push(index);
    result.extend_from_slice(&(value.len() as u16).to_le_bytes());
    result.extend_from_slice(suffix);
    result.extend_from_slice(value);
    result.resize(round_up(result.len() as u64, 4) as usize, 0);
    Ok(())
}

fn encode_xattrs(
    attributes: &BTreeMap<String, Vec<u8>>,
    shared: &BTreeMap<(String, Vec<u8>), u32>,
) -> Result<Vec<u8>> {
    if attributes.is_empty() {
        return Ok(Vec::new());
    }
    let mut result = vec![0u8; 12];
    let mut inline = Vec::new();
    for (name, value) in attributes {
        if result[4] < u8::MAX
            && let Some(offset) = shared.get(&(name.clone(), value.clone()))
        {
            result[4] += 1;
            result.extend_from_slice(&offset.to_le_bytes());
        } else {
            encode_xattr(&mut inline, name, value)?;
        }
    }
    result.extend_from_slice(&inline);
    ensure!(
        (result.len() - 12) / 4 < u16::MAX as usize,
        "inode xattrs exceed EROFS inline xattr limit"
    );
    Ok(result)
}

struct DirectoryEntry {
    name: Vec<u8>,
    nid: u64,
    file_type: u8,
}

fn directory_entries(entries: &[Entry], index: usize) -> Vec<DirectoryEntry> {
    let entry = &entries[index];
    let mut children = vec![
        DirectoryEntry {
            name: b".".to_vec(),
            nid: entry.nid,
            file_type: EROFS_FT_DIR,
        },
        DirectoryEntry {
            name: b"..".to_vec(),
            nid: entries[entry.parent].nid,
            file_type: EROFS_FT_DIR,
        },
    ];
    children.extend(entry.children.iter().map(|(name, child)| DirectoryEntry {
        name: name.clone(),
        nid: entries[*child].nid,
        file_type: erofs_mode_to_ftype(entries[*child].metadata.mode),
    }));
    children.sort_by(|a, b| a.name.cmp(&b.name));
    children
}

fn directory_size(entries: &[DirectoryEntry], block_size: u32) -> Result<u64> {
    let block_size = u64::from(block_size);
    let mut size = 0u64;
    for entry in entries {
        let length = 12 + entry.name.len() as u64;
        ensure!(
            length <= block_size,
            "directory entry exceeds filesystem block size"
        );
        if size % block_size + length > block_size {
            size = round_up(size, block_size);
        }
        size = size
            .checked_add(length)
            .context("directory size overflow")?;
    }
    Ok(size)
}

fn encode_directory(entries: &[DirectoryEntry], block_size: u32) -> Result<Vec<u8>> {
    let size =
        usize::try_from(directory_size(entries, block_size)?).context("directory is too large")?;
    let mut data = vec![0u8; size];
    let mut first = 0usize;
    let mut offset = 0usize;
    while first < entries.len() {
        let mut end = first;
        let mut used = 0usize;
        while end < entries.len() && used + 12 + entries[end].name.len() <= block_size as usize {
            used += 12 + entries[end].name.len();
            end += 1;
        }
        let mut name_offset = (end - first) * 12;
        for (number, entry) in entries[first..end].iter().enumerate() {
            let record = offset + number * 12;
            put64(&mut data, record, entry.nid);
            put16(&mut data, record + 8, name_offset as u16);
            data[record + 10] = entry.file_type;
            data[offset + name_offset..offset + name_offset + entry.name.len()]
                .copy_from_slice(&entry.name);
            name_offset += entry.name.len();
        }
        first = end;
        offset += block_size as usize;
    }
    Ok(data)
}

fn write_filesystem(
    image: &mut File,
    entries: &mut [Entry],
    options: &BuildOptions,
    epoch: u64,
    shared_xattrs: &[u8],
) -> Result<BuildReport> {
    let block_size = u64::from(options.block_size);
    let initial_size = round_up(EROFS_SUPER_OFFSET + 128 + 18, block_size).max(4096);
    write_zeros(image, initial_size)?;
    let mut staged = tempfile::tempfile().context("create EROFS data staging file")?;
    write_zeros(&mut staged, initial_size)?;
    let mut report = BuildReport::default();
    for entry in entries.iter_mut().filter(|e| e.alias.is_none()) {
        report.inode_count += 1;
        if s_isdir(entry.metadata.mode) {
            report.directory_count += 1;
            continue;
        }
        if s_isreg(entry.metadata.mode) {
            report.file_count += 1;
            report.uncompressed_bytes = report
                .uncompressed_bytes
                .checked_add(entry.size)
                .context("filesystem size overflow")?;
            write_regular(&mut staged, entry, options)
                .with_context(|| format!("write file {}", entry.path))?;
            if erofs_inode_is_data_compressed(entry.layout) {
                report.compressed_files += 1;
            }
        } else if s_islnk(entry.metadata.mode) {
            let target = entry
                .metadata
                .symlink
                .as_ref()
                .context("missing symlink target")?
                .as_bytes()
                .to_vec();
            ensure!(
                !target.contains(&0),
                "symlink target contains NUL: {}",
                entry.path
            );
            write_flat_bytes(&mut staged, entry, &target, options.block_size)?;
        } else {
            entry.data_union = entry.metadata.rdev;
        }
    }

    // Compression determines inode sizes. Stage data once, then place metadata
    // before the payload and relocate only physical addresses, not block counts.
    let stage_bytes = staged.stream_position()? - initial_size;
    let metadata_start = initial_size;
    let metadata_block = 0;
    let offset = assign_inode_slots(entries, block_size)?;
    let xattr_block = if shared_xattrs.is_empty() {
        0
    } else {
        block_address(
            metadata_start + round_up(offset, block_size),
            options.block_size,
        )?
    };
    let data_start = round_up(
        metadata_start
            + round_up(offset, block_size)
            + if shared_xattrs.is_empty() {
                0
            } else {
                round_up(shared_xattrs.len() as u64, block_size)
            },
        block_size,
    );
    let data_shift = u32::try_from((data_start - initial_size) / block_size)
        .context("EROFS data address overflow")?;
    for entry in entries.iter_mut().filter(|e| e.alias.is_none()) {
        entry.nid += metadata_start >> EROFS_ISLOTBITS;
        if erofs_inode_is_data_compressed(entry.layout) {
            relocate_indexes(
                &mut entry.compression,
                entry.inode_size + entry.xattrs.len(),
                usize::try_from(entry.size.div_ceil(block_size))
                    .context("too many EROFS indexes")?,
                entry.layout == EROFS_INODE_COMPRESSED_COMPACT,
                data_shift,
            )?;
        } else if (s_isreg(entry.metadata.mode) || s_islnk(entry.metadata.mode))
            && entry.data_union != u32::MAX
        {
            entry.data_union = entry
                .data_union
                .checked_add(data_shift)
                .context("EROFS data address overflow")?;
        }
    }
    write_zeros(image, data_start - metadata_start)?;
    if !shared_xattrs.is_empty() {
        image.seek(SeekFrom::Start(
            metadata_start + round_up(offset, block_size),
        ))?;
        image.write_all(shared_xattrs)?;
        pad_block(image, options.block_size)?;
    }
    image.seek(SeekFrom::Start(data_start))?;
    staged.seek(SeekFrom::Start(initial_size))?;
    io::copy(&mut staged.take(stage_bytes), image).context("copy staged EROFS data")?;
    let data_end = image.stream_position()?;
    ensure!(
        data_end == data_start + stage_bytes,
        "staged EROFS data length changed"
    );
    for index in 0..entries.len() {
        if let Some(alias) = entries[index].alias {
            entries[index].nid = entries[alias].nid;
        }
    }
    for index in 0..entries.len() {
        if !s_isdir(entries[index].metadata.mode) {
            continue;
        }
        let data = encode_directory(&directory_entries(entries, index), options.block_size)?;
        write_flat_bytes(image, &mut entries[index], &data, options.block_size)?;
    }
    let image_bytes = image.stream_position()?;
    let blocks = block_address(image_bytes, options.block_size)?;
    for (number, entry) in entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.alias.is_none())
    {
        let inode_offset = entry.nid << EROFS_ISLOTBITS;
        image.seek(SeekFrom::Start(inode_offset))?;
        let inode = encode_inode(
            entry,
            u32::try_from(number + 1).context("too many EROFS inodes")?,
            epoch,
        )?;
        image.write_all(&inode)?;
        image.write_all(&entry.xattrs)?;
        if erofs_inode_is_data_compressed(entry.layout) {
            let padding = (round_up(
                inode_offset + inode.len() as u64 + entry.xattrs.len() as u64,
                8,
            ) - image.stream_position()?) as usize;
            image.write_all(&[0u8; 8][..padding])?;
            image.write_all(&entry.compression)?;
        } else {
            image.write_all(&entry.inline)?;
        }
    }
    write_superblock(
        image,
        entries,
        options,
        epoch,
        metadata_block,
        xattr_block,
        blocks,
    )?;
    ensure!(
        image.metadata()?.len() == image_bytes,
        "EROFS output length does not match its encoded block count"
    );
    report.image_bytes = image_bytes;
    Ok(report)
}

fn assign_inode_slots(entries: &mut [Entry], block_size: u64) -> Result<u64> {
    let mut end = 0u64;
    let mut gaps = BTreeMap::<u64, Vec<u64>>::new();
    for entry in entries.iter_mut().filter(|e| e.alias.is_none()) {
        let size = round_up(entry.body_size(), EROFS_SLOTSIZE as u64);
        let gap = gaps.range(size..).next().map(|(&length, _)| length);
        let offset = if let Some(length) = gap {
            let positions = gaps.get_mut(&length).unwrap();
            let offset = positions.pop().unwrap();
            if positions.is_empty() {
                gaps.remove(&length);
            }
            if length > size {
                gaps.entry(length - size).or_default().push(offset + size);
            }
            offset
        } else {
            // Inline tails cannot cross a block. Reuse the skipped slots for later
            // inodes instead of permanently padding every partially occupied block.
            if entry.layout == EROFS_INODE_FLAT_INLINE && end % block_size + size > block_size {
                let next = round_up(end, block_size);
                gaps.entry(next - end).or_default().push(end);
                end = next;
            }
            let offset = end;
            end = end
                .checked_add(size)
                .context("inode metadata size overflow")?;
            offset
        };
        entry.nid = offset >> EROFS_ISLOTBITS;
    }
    Ok(end)
}

fn write_regular(image: &mut File, entry: &mut Entry, options: &BuildOptions) -> Result<()> {
    let mut input = BufReader::with_capacity(256 * 1024, File::open(&entry.host)?);
    let start = image.stream_position()?;
    if entry.size > options.block_size as u64 && options.compression != Compression::None {
        let mut compressed = compress_file(
            &mut input,
            image,
            options.block_size,
            options.cluster_size,
            options.compression,
        )?;
        ensure!(
            compressed.original_size == entry.size,
            "source file changed size while building"
        );
        if compressed.used_compression && options.compact_indexes {
            compressed
                .compact_indexes(entry.inode_size + entry.xattrs.len(), options.block_size)?;
        }
        let flat_size = round_up(
            entry.size - entry.inline.len() as u64,
            options.block_size as u64,
        ) + entry.inline.len() as u64;
        if compressed.used_compression
            && compressed.physical_size + (compressed.metadata.len() as u64) < flat_size
        {
            entry.layout = if options.compact_indexes {
                EROFS_INODE_COMPRESSED_COMPACT
            } else {
                EROFS_INODE_COMPRESSED_FULL
            };
            entry.data_union = compressed.compressed_blocks;
            entry.inline.clear();
            entry.compression = compressed.metadata;
            return Ok(());
        }
        // Rewind and overwrite the candidate without truncating the image. On Windows,
        // scanners can map the growing output and make SetEndOfFile fail with error 1224.
        image.seek(SeekFrom::Start(start))?;
        input.seek(SeekFrom::Start(0))?;
    }
    entry.data_union = if entry.size > entry.inline.len() as u64 {
        block_address(start, options.block_size)?
    } else {
        u32::MAX
    };
    let external_bytes = entry.size - entry.inline.len() as u64;
    let copied = io::copy(&mut (&mut input).take(external_bytes), image)?;
    ensure!(
        copied == external_bytes,
        "source file became shorter while building"
    );
    input.read_exact(&mut entry.inline)?;
    let mut extra = [0u8; 1];
    ensure!(
        input.read(&mut extra)? == 0,
        "source file grew while building"
    );
    pad_block(image, options.block_size)?;
    Ok(())
}

fn write_flat_bytes(
    image: &mut File,
    entry: &mut Entry,
    data: &[u8],
    block_size: u32,
) -> Result<()> {
    ensure!(
        entry.size == data.len() as u64,
        "inconsistent inode data size for {}",
        entry.path
    );
    let external_len = data.len() - entry.inline.len();
    entry.data_union = if external_len != 0 {
        block_address(image.stream_position()?, block_size)?
    } else {
        u32::MAX
    };
    image.write_all(&data[..external_len])?;
    entry.inline.copy_from_slice(&data[external_len..]);
    pad_block(image, block_size)?;
    Ok(())
}

fn pad_block(image: &mut File, block_size: u32) -> Result<()> {
    let position = image.stream_position()?;
    let padding = (round_up(position, block_size as u64) - position) as usize;
    image.write_all(&[0u8; 4096][..padding])?;
    Ok(())
}

fn write_zeros(image: &mut File, mut length: u64) -> Result<()> {
    static ZEROES: [u8; 64 * 1024] = [0; 64 * 1024];
    while length != 0 {
        let count = length.min(ZEROES.len() as u64) as usize;
        image.write_all(&ZEROES[..count])?;
        length -= count as u64;
    }
    Ok(())
}

fn block_address(position: u64, block_size: u32) -> Result<u32> {
    ensure!(
        position.is_multiple_of(block_size as u64),
        "unaligned EROFS block address"
    );
    u32::try_from(position / block_size as u64)
        .context("image exceeds EROFS 32-bit block addressing")
}

fn encode_inode(entry: &Entry, number: u32, epoch: u64) -> Result<Vec<u8>> {
    let mut inode = vec![0u8; entry.inode_size];
    let extended = entry.inode_size == 64;
    put16(
        &mut inode,
        0,
        u16::from(extended) | ((entry.layout as u16) << 1),
    );
    let xattr_count = if entry.xattrs.is_empty() {
        0
    } else {
        1 + (entry.xattrs.len() - 12) / 4
    };
    put16(
        &mut inode,
        2,
        u16::try_from(xattr_count).context("xattr count exceeds EROFS format")?,
    );
    put16(&mut inode, 4, entry.metadata.mode as u16);
    put32(&mut inode, 16, entry.data_union);
    put32(&mut inode, 20, number);
    if extended {
        put64(&mut inode, 8, entry.size);
        put32(&mut inode, 24, entry.metadata.uid);
        put32(&mut inode, 28, entry.metadata.gid);
        put64(&mut inode, 32, entry.metadata.mtime);
        put32(&mut inode, 40, entry.metadata.mtime_nsec);
        put32(&mut inode, 44, entry.links);
    } else {
        put16(&mut inode, 6, entry.links as u16);
        put32(&mut inode, 8, entry.size as u32);
        put32(&mut inode, 12, (entry.metadata.mtime - epoch) as u32);
        put16(&mut inode, 24, entry.metadata.uid as u16);
        put16(&mut inode, 26, entry.metadata.gid as u16);
    }
    Ok(inode)
}

fn write_superblock(
    image: &mut File,
    entries: &[Entry],
    options: &BuildOptions,
    epoch: u64,
    metadata_block: u32,
    xattr_block: u32,
    blocks: u32,
) -> Result<()> {
    let block_size = options.block_size as usize;
    let end = round_up(EROFS_SUPER_OFFSET + 128 + 18, block_size as u64).max(4096) as usize;
    let mut initial = vec![0u8; end];
    let sb = &mut initial[EROFS_SUPER_OFFSET as usize..];
    put32(sb, 0, EROFS_SUPER_MAGIC_V1);
    put32(
        sb,
        8,
        EROFS_FEATURE_COMPAT_MTIME
            | if options.checksum {
                EROFS_FEATURE_COMPAT_SB_CHKSUM
            } else {
                0
            },
    );
    sb[12] = options.block_size.trailing_zeros() as u8;
    put16(
        sb,
        14,
        u16::try_from(entries[0].nid).context("root inode exceeds 16-bit EROFS nid")?,
    );
    put64(
        sb,
        16,
        entries.iter().filter(|entry| entry.alias.is_none()).count() as u64,
    );
    put64(sb, 24, epoch);
    put32(sb, 36, blocks);
    put32(sb, 40, metadata_block);
    put32(sb, 44, xattr_block);
    let uuid = options.uuid.unwrap_or_else(|| {
        let mut hash = Sha256::new();
        hash.update(epoch.to_le_bytes());
        for entry in entries {
            hash.update(entry.path.as_bytes());
            hash.update(entry.size.to_le_bytes());
        }
        let digest = hash.finalize();
        let mut uuid: [u8; 16] = digest[..16].try_into().unwrap();
        uuid[6] = (uuid[6] & 0x0f) | 0x40;
        uuid[8] = (uuid[8] & 0x3f) | 0x80;
        uuid
    });
    sb[48..64].copy_from_slice(&uuid);
    sb[64..64 + options.volume_label.len()].copy_from_slice(options.volume_label.as_bytes());
    if entries
        .iter()
        .any(|entry| erofs_inode_is_data_compressed(entry.layout))
    {
        let big_clusters = options.cluster_size > options.block_size;
        put32(
            sb,
            80,
            EROFS_FEATURE_INCOMPAT_LZ4_0PADDING
                | if big_clusters {
                    EROFS_FEATURE_INCOMPAT_BIG_PCLUSTER
                } else {
                    0
                },
        );
        if big_clusters {
            put16(sb, 84, 1 << Z_EROFS_COMPRESSION_LZ4);
            put16(sb, 128, 14);
            put16(sb, 130, u16::MAX);
            put16(sb, 132, (options.cluster_size / options.block_size) as u16);
        } else {
            put16(sb, 84, u16::MAX);
        }
    }
    let build_time = options.timestamp.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    });
    put32(
        sb,
        108,
        u32::try_from(build_time.saturating_sub(epoch)).unwrap_or(u32::MAX),
    );
    if options.checksum {
        let length = if block_size > EROFS_SUPER_OFFSET as usize {
            block_size - EROFS_SUPER_OFFSET as usize
        } else {
            block_size
        };
        let checksum = !crc32c::crc32c(&sb[..length]);
        put32(sb, 4, checksum);
    }
    image.seek(SeekFrom::Start(0))?;
    image.write_all(&initial)?;
    Ok(())
}

fn put16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[cfg(windows)]
    struct ReadOnlyMapping {
        handle: windows_sys::Win32::Foundation::HANDLE,
        view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    }

    #[cfg(windows)]
    impl ReadOnlyMapping {
        fn new(file: &File) -> Self {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Memory::{
                CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY,
            };

            // SAFETY: the file handle remains open for the lifetime of this mapping.
            let handle = unsafe {
                CreateFileMappingW(
                    file.as_raw_handle(),
                    std::ptr::null(),
                    PAGE_READONLY,
                    0,
                    0,
                    std::ptr::null(),
                )
            };
            assert!(!handle.is_null(), "{}", io::Error::last_os_error());
            // SAFETY: handle is a valid read-only file mapping object.
            let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
            if view.Value.is_null() {
                // SAFETY: handle was created successfully and is not used afterward.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                panic!("{}", io::Error::last_os_error());
            }
            Self { handle, view }
        }
    }

    #[cfg(windows)]
    impl Drop for ReadOnlyMapping {
        fn drop(&mut self) {
            // SAFETY: both resources are valid and released exactly once here.
            unsafe {
                windows_sys::Win32::System::Memory::UnmapViewOfFile(self.view);
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }

    fn read_image(path: &Path) -> Arc<crate::sb::SbInfo> {
        Arc::new(
            crate::sb::erofs_read_superblock(
                crate::io::Device::open(path.to_str().unwrap(), 0).unwrap(),
            )
            .unwrap(),
        )
    }

    fn read_file(image: &Arc<crate::sb::SbInfo>, path: &str) -> (Inode, Vec<u8>) {
        let mut inode = crate::dir::erofs_ilookup(image, path).unwrap();
        let mut content = vec![0; inode.i_size as usize];
        crate::data::inode_pread(&mut inode, &mut content, 0).unwrap();
        (inode, content)
    }

    #[test]
    fn flat_images_roundtrip_multiblock_directories_and_inline_tails() {
        for block_size in [512, 1024, 2048, 4096] {
            let temporary = TempDir::new().unwrap();
            let source = temporary.path().join("source");
            fs::create_dir(&source).unwrap();
            fs::create_dir(source.join("empty-dir")).unwrap();
            let data: Vec<u8> = (0..3 * block_size + 73).map(|i| (i % 251) as u8).collect();
            fs::write(source.join("large"), &data).unwrap();
            fs::write(source.join("empty"), []).unwrap();
            for number in 0..150 {
                fs::write(source.join(format!("entry-{number:03}")), [number as u8]).unwrap();
            }
            let output = temporary.path().join("image.erofs");
            let options = BuildOptions {
                block_size,
                cluster_size: block_size,
                compression: Compression::None,
                timestamp: Some(123456),
                ..BuildOptions::default()
            };
            let report = build(&source, &output, &options).unwrap();
            assert_eq!(report.directory_count, 2);
            assert_eq!(report.file_count, 152);
            assert_eq!(report.image_bytes % block_size as u64, 0);
            let image = read_image(&output);
            assert_eq!(read_file(&image, "/large").1, data);
            assert!(read_file(&image, "/empty").1.is_empty());
            assert_eq!(read_file(&image, "/entry-149").1, [149]);
            let root = read_file(&image, "/").0;
            assert_eq!(root.i_nlink, 3);
            let mut raw = fs::read(&output).unwrap();
            let stored = get_unaligned_le32(&raw, 1028);
            put32(&mut raw, 1028, 0);
            let checksum_length = if block_size > 1024 {
                block_size - 1024
            } else {
                block_size
            };
            assert_eq!(
                stored,
                !crc32c::crc32c(&raw[1024..1024 + checksum_length as usize])
            );
        }
    }

    #[test]
    fn compressed_images_roundtrip_and_fall_back_for_noise() {
        for (cluster_size, compact_indexes) in
            [(4096, false), (4096, true), (16384, false), (16384, true)]
        {
            let temporary = TempDir::new().unwrap();
            let source = temporary.path().join("source");
            fs::create_dir(&source).unwrap();
            let data: Vec<u8> = (0..360_019).map(|i| ((i / 133) % 239) as u8).collect();
            fs::write(source.join("compressible"), &data).unwrap();
            let mut state = 0x1234abcd_u32;
            let noise: Vec<u8> = (0..65593)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect();
            fs::write(source.join("noise"), &noise).unwrap();
            let output = temporary.path().join("image.erofs");
            let options = BuildOptions {
                cluster_size,
                compact_indexes,
                timestamp: Some(0),
                ..BuildOptions::default()
            };
            let report = build(&source, &output, &options).unwrap();
            assert_eq!(report.compressed_files, 1);
            let image = read_image(&output);
            let (inode, read) = read_file(&image, "/compressible");
            assert_eq!(
                inode.datalayout,
                if compact_indexes {
                    EROFS_INODE_COMPRESSED_COMPACT
                } else {
                    EROFS_INODE_COMPRESSED_FULL
                }
            );
            assert_eq!(read, data);
            assert_eq!(read_file(&image, "/noise").1, noise);
        }
    }

    #[test]
    fn inline_inode_gaps_are_reused_without_overlapping_data() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        for number in 0..10 {
            fs::write(source.join(format!("a-{number}")), vec![number; 2600]).unwrap();
            fs::write(source.join(format!("z-{number}")), vec![number; 1000]).unwrap();
        }
        let output = temporary.path().join("image.erofs");
        let report = build(
            &source,
            &output,
            &BuildOptions {
                compression: Compression::None,
                timestamp: Some(0),
                ..BuildOptions::default()
            },
        )
        .unwrap();
        assert!(report.image_bytes <= 12 * 4096);
        crate::verify_image(&output).unwrap();
        let image = read_image(&output);
        assert_eq!(image.meta_blkaddr, 0);
        assert_ne!(image.root_nid, 0);
        for number in 0..10 {
            assert_eq!(
                read_file(&image, &format!("/a-{number}")).1,
                vec![number; 2600]
            );
            assert_eq!(
                read_file(&image, &format!("/z-{number}")).1,
                vec![number; 1000]
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn incompressible_fallback_works_with_mapped_output() {
        let temporary = TempDir::new().unwrap();
        let source_dir = temporary.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        let source = source_dir.join("libabsl_container.z.so");
        let mut state = 0x1234abcd_u32;
        let data: Vec<u8> = (0..7800)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        fs::write(&source, &data).unwrap();

        let output = temporary.path().join("mapped-output.erofs");
        let mut image = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&output)
            .unwrap();
        write_zeros(&mut image, 4096).unwrap();
        let mapping = ReadOnlyMapping::new(&image);
        image.seek(SeekFrom::Start(4096)).unwrap();

        let mut entry = Entry {
            host: source,
            path: "/system/lib64/libabsl_container.z.so".to_owned(),
            metadata: EntryMetadata {
                mode: S_IFREG | 0o644,
                uid: 0,
                gid: 0,
                mtime: 0,
                mtime_nsec: 0,
                rdev: 0,
                original_nid: None,
                symlink: None,
                xattrs: BTreeMap::new(),
            },
            parent: 0,
            children: Vec::new(),
            alias: None,
            size: data.len() as u64,
            links: 1,
            inode_size: 32,
            xattrs: Vec::new(),
            layout: EROFS_INODE_FLAT_INLINE,
            data_union: u32::MAX,
            inline: vec![0; data.len() % 4096],
            compression: Vec::new(),
            nid: 0,
        };
        write_regular(&mut image, &mut entry, &BuildOptions::default()).unwrap();

        assert_eq!(entry.layout, EROFS_INODE_FLAT_INLINE);
        assert_eq!(entry.inline, data[4096..]);
        let mut external = vec![0; 4096];
        image.seek(SeekFrom::Start(4096)).unwrap();
        image.read_exact(&mut external).unwrap();
        assert_eq!(external, data[..4096]);

        let options = BuildOptions {
            timestamp: Some(0),
            ..BuildOptions::default()
        };
        let resolver = MetadataResolver::load(&options).unwrap();
        let mut entries = Vec::new();
        collect_entries(&source_dir, "/".to_owned(), 0, &resolver, &mut entries).unwrap();
        let shared = prepare_entries(&mut entries, &options, 0).unwrap();
        image.seek(SeekFrom::Start(0)).unwrap();
        let report = write_filesystem(&mut image, &mut entries, &options, 0, &shared).unwrap();
        assert_eq!(image.metadata().unwrap().len(), report.image_bytes);
        drop(mapping);
        drop(image);
        crate::verify_image(&output).unwrap();
        assert_eq!(
            read_file(&read_image(&output), "/libabsl_container.z.so").1,
            data
        );
    }

    #[test]
    fn rejects_overwrite_and_output_in_source() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        let existing = temporary.path().join("existing");
        fs::write(&existing, b"keep").unwrap();
        assert!(build(&source, &existing, &BuildOptions::default()).is_err());
        assert_eq!(fs::read(existing).unwrap(), b"keep");
        assert!(build(&source, &source.join("output"), &BuildOptions::default()).is_err());
        assert!(!source.join("output").exists());
    }

    #[test]
    fn xattr_encoding_uses_namespaces_and_rejects_overflow() {
        let attributes = BTreeMap::from([
            (
                "security.selinux".to_owned(),
                b"u:object_r:system_file:s0\0".to_vec(),
            ),
            ("user.note".to_owned(), b"value".to_vec()),
        ]);
        let bytes = encode_xattrs(&attributes, &BTreeMap::new()).unwrap();
        assert_eq!(bytes[12], 7);
        assert_eq!(bytes[13], EROFS_XATTR_INDEX_SECURITY);
        assert_eq!(bytes.len() % 4, 0);
        assert!(
            encode_xattrs(
                &BTreeMap::from([("user.huge".to_owned(), vec![0; 65536])]),
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn preserves_recorded_ownership_xattrs_and_hardlinks_with_edited_copies() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        for (name, content) in [("first", "same"), ("second", "same"), ("edited", "edit")] {
            fs::write(source.join(name), content).unwrap();
        }
        let attributes = BTreeMap::from([
            (
                "security.selinux".to_owned(),
                b"u:object_r:system_file:s0\0".to_vec(),
            ),
            ("user.large".to_owned(), vec![0x53; 6001]),
        ]);
        let metadata = EntryMetadata {
            mode: S_IFREG | 0o6751,
            uid: 123456,
            gid: 654321,
            mtime: 424242,
            mtime_nsec: 12345,
            rdev: 0,
            original_nid: Some(99),
            symlink: None,
            xattrs: attributes.clone(),
        };
        let manifest = crate::metadata::MetadataManifest {
            version: 1,
            block_size: 4096,
            uuid: [0; 16],
            volume_label: String::new(),
            build_time: 7,
            extraction_root: None,
            entries: BTreeMap::from([
                ("/first".to_owned(), metadata.clone()),
                ("/second".to_owned(), metadata.clone()),
                ("/edited".to_owned(), metadata),
            ]),
        };
        let metadata_file = temporary.path().join("metadata.json");
        fs::write(&metadata_file, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let output = temporary.path().join("image.erofs");
        let options = BuildOptions {
            metadata_file: Some(metadata_file),
            timestamp: Some(7),
            xattr_tolerance: 1,
            ..BuildOptions::default()
        };
        let report = build(&source, &output, &options).unwrap();
        assert_eq!(report.inode_count, 3);
        let image = read_image(&output);
        let (mut first, content) = read_file(&image, "/first");
        assert_eq!(content, b"same");
        assert_eq!(first.nid, read_file(&image, "/second").0.nid);
        assert_ne!(first.nid, read_file(&image, "/edited").0.nid);
        assert_eq!(first.i_nlink, 2);
        assert_eq!(
            (first.i_mode, first.i_uid, first.i_gid),
            (S_IFREG | 0o6751, 123456, 654321)
        );
        assert_eq!((first.i_mtime, first.i_mtime_nsec), (424242, 12345));
        assert!(image.xattr_blkaddr != 0);
        for (name, expected) in attributes {
            let mut actual = vec![0; expected.len()];
            let length = crate::xattr::getxattr(&mut first, &name, &mut actual, false).unwrap();
            assert_eq!(length, expected.len());
            assert_eq!(actual, expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_host_symlink_targets_without_following_them() {
        use std::os::unix::fs::symlink;
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        symlink("/system/bin/tool", source.join("absolute")).unwrap();
        symlink("../absent", source.join("relative")).unwrap();
        let output = temporary.path().join("image.erofs");
        build(&source, &output, &BuildOptions::default()).unwrap();
        let image = read_image(&output);
        assert_eq!(read_file(&image, "/absolute").1, b"/system/bin/tool");
        assert_eq!(read_file(&image, "/relative").1, b"../absent");
    }

    #[test]
    fn preserves_native_host_hardlinks_without_metadata_sidecar() {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("first"), b"linked content").unwrap();
        fs::hard_link(source.join("first"), source.join("second")).unwrap();
        let output = temporary.path().join("image.erofs");
        let report = build(&source, &output, &BuildOptions::default()).unwrap();
        assert_eq!(report.file_count, 1);
        let image = read_image(&output);
        let first = read_file(&image, "/first").0;
        assert_eq!(first.nid, read_file(&image, "/second").0.nid);
        assert_eq!(first.i_nlink, 2);
    }

    #[cfg(windows)]
    #[test]
    fn windows_symlinks_roundtrip_root_rebased_and_relative_targets() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("destination"), b"target").unwrap();
        symlink_file("destination", source.join("relative")).unwrap();
        symlink_file(source.join("destination"), source.join("absolute")).unwrap();
        symlink_dir(source.join("missing-dir"), source.join("directory-link")).unwrap();
        symlink_dir(&source, source.join("root-link")).unwrap();
        let output = temporary.path().join("image.erofs");
        build(&source, &output, &BuildOptions::default()).unwrap();
        let image = read_image(&output);
        assert_eq!(read_file(&image, "/relative").1, b"destination");
        assert_eq!(read_file(&image, "/absolute").1, b"/destination");
        assert_eq!(read_file(&image, "/directory-link").1, b"/missing-dir");
        assert_eq!(read_file(&image, "/root-link").1, b"/");
    }

    #[cfg(windows)]
    #[test]
    fn relative_extraction_output_preserves_absolute_symlinks_when_repacked() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let cwd = std::env::current_dir().unwrap();
        let temporary = tempfile::Builder::new()
            .prefix("erofs-relative-")
            .tempdir_in(&cwd)
            .unwrap();
        let source = temporary.path().join("source");
        fs::create_dir_all(source.join("system/bin")).unwrap();
        fs::write(source.join("system/bin/tool"), b"target content").unwrap();
        symlink_dir(source.join("system/bin"), source.join("bin")).unwrap();
        symlink_file("system/bin/tool", source.join("relative")).unwrap();
        let image = temporary.path().join("initial.img");
        build(&source, &image, &BuildOptions::default()).unwrap();
        let extracted = temporary.path().join("extracted");
        let relative_output = extracted.strip_prefix(&cwd).unwrap();
        crate::extract(
            &image,
            relative_output,
            crate::ExtractOptions {
                threads: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!extracted.join("config/erofs-metadata.json").exists());
        let extracted_source = extracted.join("initial");
        assert!(
            fs::read_link(extracted_source.join("bin"))
                .unwrap()
                .is_absolute()
        );
        assert_eq!(
            fs::read(extracted_source.join("bin/tool")).unwrap(),
            b"target content"
        );
        let rebuilt = temporary.path().join("rebuilt.img");
        build(&extracted_source, &rebuilt, &BuildOptions::default()).unwrap();
        let image = read_image(&rebuilt);
        assert_eq!(read_file(&image, "/bin").1, b"/system/bin");
        assert_eq!(read_file(&image, "/relative").1, b"system/bin/tool");
        assert_eq!(read_file(&image, "/system/bin/tool").1, b"target content");
    }
}
