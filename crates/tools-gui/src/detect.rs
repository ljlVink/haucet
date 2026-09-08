use std::fs::File;
use std::io::Read;
use std::path::Path;

use common::nvme::{NVE_BLOCK_SIZE, NVE_HEADER_MAGIC, NVE_HEADER_SIZE, NVE_PARTITION_COUNT};

const HVB_FOOTER_SIZE: usize = 104;
const HVB_FOOTER_MAGIC: &[u8; 8] = b"HVB\0\0\0\0\0";
const HARMONY_MAGIC: &[u8; 8] = b"HARMONY!";
const RVT_MAGIC: &[u8; 4] = b"rot\0";
const GPT_MAGIC: &[u8; 8] = b"EFI PART";
const EROFS_MAGIC: &[u8; 4] = &[0xe2, 0xe1, 0xf5, 0xe0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Unknown,
    ZipPackage,
    UpdateApp,
    Erofs,
    Ext4,
    HarmonyFrame,
    Rvt,
    Gpt,
    SecImage,
    HvbWrapped,
    Nve,
    OemInfo,
    Cpio,
    ErofsWorkspace,
    RamdiskWorkspace,
}

impl FileKind {
    pub fn label(self) -> String {
        match self {
            Self::Unknown => tr!("format-unknown"),
            Self::ZipPackage => tr!("format-zip-update"),
            Self::UpdateApp => "UPDATE.APP".to_owned(),
            Self::Erofs => tr!("format-erofs-image"),
            Self::Ext4 => tr!("format-ext4-image"),
            Self::HarmonyFrame => tr!("format-harmony-image"),
            Self::Rvt => tr!("format-rvt-image"),
            Self::Gpt => tr!("format-gpt-image"),
            Self::SecImage => tr!("format-sec-image"),
            Self::HvbWrapped => tr!("format-hvb-image"),
            Self::Nve => "Hisi-NV-Partition".to_owned(),
            Self::OemInfo => tr!("format-oeminfo-image"),
            Self::Cpio => tr!("format-cpio-archive"),
            Self::ErofsWorkspace => tr!("format-erofs-workspace"),
            Self::RamdiskWorkspace => tr!("format-ramdisk-workspace"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Detection {
    pub kind: FileKind,
    pub path: String,
    pub human: String,
}

pub fn detect(path: &Path) -> Detection {
    let path_string = path.display().to_string();
    let human = if path.is_dir() {
        detect_dir(path)
    } else {
        detect_file(path)
    };
    Detection {
        kind: human.0,
        path: path_string,
        human: human.1,
    }
}

fn detect_dir(path: &Path) -> (FileKind, String) {
    let mut has_cpio = false;
    let mut is_erofs_workspace = false;
    let Ok(entries) = std::fs::read_dir(path) else {
        return (FileKind::Unknown, tr!("detect-directory-read-error"));
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "haucet-erofs.json" {
            is_erofs_workspace = true;
        }
        if name == "ramdisk.cpio" {
            has_cpio = true;
        }
    }
    if is_erofs_workspace {
        (FileKind::ErofsWorkspace, tr!("detect-erofs-workspace"))
    } else if has_cpio {
        (FileKind::RamdiskWorkspace, tr!("detect-ramdisk-workspace"))
    } else {
        (FileKind::Unknown, tr!("detect-no-workspace"))
    }
}

//todo use local funcs, rather than judge itself.
fn detect_file(path: &Path) -> (FileKind, String) {
    let Ok(mut file) = File::open(path) else {
        return (FileKind::Unknown, tr!("detect-file-open-error"));
    };
    let Ok(metadata) = file.metadata() else {
        return (FileKind::Unknown, tr!("detect-metadata-error"));
    };
    let length = metadata.len();
    if length < 8 {
        return (FileKind::Unknown, tr!("detect-file-too-small"));
    }

    let mut head = [0_u8; 180];
    let head_len = read_at(&mut file, &mut head, 0).unwrap_or(0);

    // ZIP package
    if head_len >= 4 && &head[0..4] == b"PK\x03\x04" {
        return (FileKind::ZipPackage, tr!("detect-zip"));
    }

    // update.bin: TLV type 0x01 (L2) or 0x11 (L1) + sane component table size
    // TODO: IS HERE SHOULD CHANGE FROM LE TO BE BYTES?
    if head_len >= 180 {
        let tlv_type = u16::from_le_bytes([head[0], head[1]]);
        let compinfo_len = u16::from_le_bytes([head[178], head[179]]) as usize;
        if (tlv_type == 0x01 || tlv_type == 0x11)
            && compinfo_len > 0
            && compinfo_len <= 8 * 1024 * 1024
            && length as usize >= 180 + compinfo_len + 16
        {
            return (
                FileKind::ZipPackage,
                tr!("detect-update-bin", "count" => compinfo_len / 71),
            );
        }
    }

    if common::splituapp::probe_file(path).unwrap_or(false) {
        return (FileKind::UpdateApp, tr!("detect-update-app"));
    }

    // cpio archive
    if head_len >= 6 && (&head[0..6] == b"070701" || &head[0..6] == b"070702") {
        return (FileKind::Cpio, tr!("detect-cpio"));
    }

    // HARMONY! frame
    if head_len >= 8 && &head[0..8] == HARMONY_MAGIC {
        return (FileKind::HarmonyFrame, tr!("detect-harmony"));
    }

    // RVT
    if head_len >= 4 && &head[0..4] == RVT_MAGIC {
        return (FileKind::Rvt, tr!("detect-rvt"));
    }

    // GPT header is at LBA 1 (offset 512) for a standard 512-byte logical block.
    let mut gpt_magic = [0_u8; 8];
    if length >= 520
        && read_at(&mut file, &mut gpt_magic, 512).unwrap_or(0) == GPT_MAGIC.len()
        && &gpt_magic == GPT_MAGIC
    {
        return (FileKind::Gpt, tr!("detect-gpt"));
    }

    if common::formats::secimg::probe_image(path).unwrap_or(false) {
        return (FileKind::SecImage, tr!("detect-sec-image"));
    }

    // EROFS magic at offset 1024
    let mut erofs_magic = [0_u8; 4];
    if length >= 1028
        && read_at(&mut file, &mut erofs_magic, 1024).unwrap_or(0) == 4
        && &erofs_magic == EROFS_MAGIC
    {
        return (FileKind::Erofs, tr!("detect-erofs"));
    }

    if common::formats::ext4::is_ext4(path).unwrap_or(false) {
        return (FileKind::Ext4, tr!("detect-ext4"));
    }

    // HVB footer at the tail
    let mut footer = [0_u8; HVB_FOOTER_SIZE];
    if length >= HVB_FOOTER_SIZE as u64
        && read_at(&mut file, &mut footer, length - HVB_FOOTER_SIZE as u64).unwrap_or(0)
            == HVB_FOOTER_SIZE
        && &footer[0..8] == HVB_FOOTER_MAGIC
    {
        return (FileKind::HvbWrapped, tr!("detect-hvb"));
    }

    if length >= NVE_BLOCK_SIZE as u64 && length % NVE_BLOCK_SIZE as u64 == 0 {
        let mut nve_magic = [0_u8; NVE_HEADER_MAGIC.len()];
        let header_offset = (NVE_BLOCK_SIZE - NVE_HEADER_SIZE) as u64;
        for block_index in 0..NVE_PARTITION_COUNT {
            let block_offset = (block_index * NVE_BLOCK_SIZE) as u64;
            if block_offset >= length {
                break;
            }
            if read_at(&mut file, &mut nve_magic, block_offset + header_offset).unwrap_or(0)
                == nve_magic.len()
                && nve_magic == NVE_HEADER_MAGIC
            {
                return (FileKind::Nve, tr!("detect-nve"));
            }
        }
    }

    // OEMINFO has no fixed image-level header, so only probe for embedded block headers
    // after formats with stronger signatures have been ruled out.
    if common::oeminfo::probe_file(path).unwrap_or(false) {
        return (FileKind::OemInfo, tr!("detect-oeminfo"));
    }

    (FileKind::Unknown, tr!("detect-unknown"))
}

fn read_at(file: &mut File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    let mut read = 0;
    while read < buf.len() {
        let n = file.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(read)
}
