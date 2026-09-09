use crate::formats::gpt::{self, GptInfo};
use crate::formats::harmony::{HARMONY_MAGIC, HarmonyHeader, HvbFrame};
use crate::formats::hvb::{HvbCert, HvbFooter, HvbWrapper};
use crate::formats::{rvt, secimg};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const RVT_MAGIC: &[u8; 4] = b"rot\0";
const HARMONY_HEADER_PROBE_SIZE: usize = 0x800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertSummary {
    pub version_major: u32,
    pub version_minor: u32,
    pub partition_name: String,
    pub image_original_len: u64,
    pub image_len: u64,
    pub verity_type: u32,
    pub hash_algo: u32,
    pub salt_size: u64,
    pub digest_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarmonySummary {
    pub hdr_size: u32,
    pub image_size: u32,
    pub flags: u32,
    pub buildvariant: String,
    pub footer: HvbFooter,
    pub cert: CertSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PartitionSummary {
    Harmony(HarmonySummary),
    Rvt(rvt::RvtInfo),
    Gpt(GptInfo),
    SecImage(secimg::SecImageInfo),
    HvbWrapped {
        footer: HvbFooter,
        cert: Option<CertSummary>,
        cert_error: Option<String>,
    },
}

pub fn info(image: &Path) -> io::Result<()> {
    match summarize(image)? {
        PartitionSummary::Harmony(summary) => {
            print_harmony_summary(&summary);
            Ok(())
        }
        PartitionSummary::Rvt(_) => {
            let path = image
                .to_str()
                .ok_or_else(|| invalid("path is not valid UTF-8"))?;
            rvt::parse_file(path)
        }
        PartitionSummary::Gpt(summary) => {
            print_gpt_summary(&summary);
            Ok(())
        }
        PartitionSummary::SecImage(summary) => {
            print_secimg_summary(&summary);
            Ok(())
        }
        PartitionSummary::HvbWrapped {
            footer,
            cert,
            cert_error,
        } => {
            print_wrapper_summary(&footer, cert.as_ref(), cert_error.as_deref());
            Ok(())
        }
    }
}

pub fn summarize(image: &Path) -> io::Result<PartitionSummary> {
    // if image starts with harmony magic (like system erofs image) may cause too much memory load, using a optimized algorithm to reduce mem.
    if starts_with_magic(image, HARMONY_MAGIC)? {
        let mut file = File::open(image)?;
        let length = file.metadata()?.len();
        return summarize_harmony_reader(&mut file, length).map(PartitionSummary::Harmony);
    }
    if starts_with_magic(image, RVT_MAGIC)? {
        return Ok(PartitionSummary::Rvt(rvt::parse_image(image)?));
    }
    if has_magic_at(image, gpt::GPT_HEADER_OFFSET, gpt::GPT_SIGNATURE)? {
        return Ok(PartitionSummary::Gpt(gpt::parse_image(image)?));
    }
    if gpt::looks_like_storage_head(image)? {
        return Ok(PartitionSummary::Gpt(gpt::parse_image(image)?));
    }
    if secimg::probe_image(image)? {
        return Ok(PartitionSummary::SecImage(secimg::parse_image(image)?));
    }
    if let Some(wrapper) = HvbWrapper::read_from(image)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
    {
        let (cert, cert_error) = match HvbCert::parse(&wrapper.certificate) {
            Ok(cert) => (Some(summarize_cert(&cert)), None),
            Err(e) => (None, Some(e.to_string())),
        };
        return Ok(PartitionSummary::HvbWrapped {
            footer: wrapper.footer,
            cert,
            cert_error,
        });
    }
    Err(invalid("not a HARMONY!/HVB/RVT/GPT/Huawei secure image"))
}

fn summarize_harmony_reader(
    reader: &mut (impl Read + Seek),
    length: u64,
) -> io::Result<HarmonySummary> {
    if length < HARMONY_MAGIC.len() as u64 {
        return Err(invalid("file too small"));
    }

    reader.seek(SeekFrom::Start(0))?;
    let header_len = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .min(HARMONY_HEADER_PROBE_SIZE);
    let mut header = vec![0_u8; header_len];
    reader.read_exact(&mut header)?;
    let harmony = HarmonyHeader::parse(&header)?;

    let wrapper = HvbWrapper::read_from_reader(reader, length)
        .map_err(|error| invalid(&error.to_string()))?
        .ok_or_else(|| invalid("not an HVB wrapped image"))?;
    let cert = HvbCert::parse(&wrapper.certificate)?;

    let header_size = u64::from(harmony.hdr_size);
    let payload_end = header_size
        .checked_add(u64::from(harmony.image_size))
        .ok_or_else(|| invalid("HARMONY payload range overflow"))?;
    if header_size < 0x20
        || header_size > wrapper.footer.image_size
        || payload_end > wrapper.footer.image_size
        || wrapper.footer.image_size > wrapper.footer.cert_offset
    {
        return Err(invalid("HVB image range is invalid"));
    }

    Ok(HarmonySummary {
        hdr_size: harmony.hdr_size,
        image_size: harmony.image_size,
        flags: harmony.flags,
        buildvariant: harmony.buildvariant,
        footer: wrapper.footer,
        cert: summarize_cert(&cert),
    })
}

fn starts_with_magic(path: &Path, magic: &[u8]) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut bytes = vec![0_u8; magic.len()];
    let read = file.read(&mut bytes)?;
    Ok(read == magic.len() && bytes == magic)
}

fn has_magic_at(path: &Path, offset: u64, magic: &[u8]) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut bytes = vec![0_u8; magic.len()];
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    let read = file.read(&mut bytes)?;
    Ok(read == magic.len() && bytes == magic)
}

fn summarize_cert(cert: &HvbCert) -> CertSummary {
    CertSummary {
        version_major: cert.version_major,
        version_minor: cert.version_minor,
        partition_name: cert.partition_name.clone(),
        image_original_len: cert.image_original_len,
        image_len: cert.image_len,
        verity_type: cert.verity_type,
        hash_algo: cert.hash_algo,
        salt_size: cert.salt_size,
        digest_size: cert.digest_size,
    }
}

pub fn print_frame_summary(frame: &HvbFrame) {
    print_harmony_summary(&HarmonySummary {
        hdr_size: frame.harmony.hdr_size,
        image_size: frame.harmony.image_size,
        flags: frame.harmony.flags,
        buildvariant: frame.harmony.buildvariant.clone(),
        footer: frame.footer.clone(),
        cert: summarize_cert(&frame.cert),
    });
}

fn print_harmony_summary(summary: &HarmonySummary) {
    eprintln!("--- HARMONY! header ---");
    eprintln!("  hdr_size     = 0x{:X}", summary.hdr_size);
    eprintln!("  image_size   = 0x{:X}", summary.image_size);
    eprintln!("  flags        = 0x{:X}", summary.flags);
    eprintln!("  buildvariant = {:?}", summary.buildvariant);
    eprintln!("--- HVB footer ---");
    eprintln!("  cert_offset    = 0x{:X}", summary.footer.cert_offset);
    eprintln!("  cert_size      = {}", summary.footer.cert_size);
    eprintln!("  image_size     = 0x{:X}", summary.footer.image_size);
    eprintln!("  partition_size = 0x{:X}", summary.footer.partition_size);
    eprintln!("--- HVB cert ---");
    print_cert_summary(&summary.cert);
}

fn print_wrapper_summary(footer: &HvbFooter, cert: Option<&CertSummary>, cert_error: Option<&str>) {
    eprintln!("--- HVB footer ---");
    eprintln!("  cert_offset    = 0x{:X}", footer.cert_offset);
    eprintln!("  cert_size      = {}", footer.cert_size);
    eprintln!("  image_size     = 0x{:X}", footer.image_size);
    eprintln!("  partition_size = 0x{:X}", footer.partition_size);
    eprintln!("--- HVB cert ---");
    match cert {
        Some(cert) => print_cert_summary(cert),
        None => eprintln!("  (parse failed: {})", cert_error.unwrap_or("unknown")),
    }
}

fn print_gpt_summary(summary: &GptInfo) {
    eprintln!(
        "--- GPT tables: {}, partitions: {} ---",
        summary.tables.len(),
        summary.partition_count()
    );
    for table in &summary.tables {
        let header = &table.header;
        eprintln!(
            "--- GPT table at image offset 0x{:X} ---",
            table.image_offset
        );
        eprintln!(
            "  revision              = {}.{}",
            header.revision >> 16,
            header.revision & 0xFFFF
        );
        eprintln!("  header_size           = {}", header.header_size);
        eprintln!("  disk_guid             = {}", header.disk_guid);
        eprintln!("  current_lba           = 0x{:X}", header.current_lba);
        eprintln!("  backup_lba            = 0x{:X}", header.backup_lba);
        eprintln!(
            "  usable_lba_range      = 0x{:X}-0x{:X}",
            header.first_usable_lba, header.last_usable_lba
        );
        eprintln!(
            "  partition_entry_lba   = 0x{:X} (image offset 0x{:X})",
            header.partition_entry_lba, table.entry_array_offset
        );
        eprintln!(
            "  partition_entry_format= {} entries x {} bytes",
            header.partition_entry_count, header.partition_entry_size
        );
        eprintln!("--- GPT partitions ({}) ---", table.partitions.len());
        for partition in &table.partitions {
            eprintln!(
                "  [{:>3}] {:?}: LBA 0x{:X}-0x{:X} (0x{:X} sectors), type={}, guid={}, attrs=0x{:X}",
                partition.index,
                partition.name,
                partition.first_lba,
                partition.last_lba,
                partition.sector_count(),
                partition.type_guid,
                partition.unique_guid,
                partition.attributes,
            );
        }
    }
}

fn print_secimg_summary(summary: &secimg::SecImageInfo) {
    eprintln!("--- Huawei secure image ---");
    eprintln!("  image_name             = {:?}", summary.image_name);
    eprintln!("  partition_name         = {:?}", summary.partition_name);
    eprintln!("  file_size              = 0x{:X}", summary.file_size);
    eprintln!(
        "  certificate_chain_size = 0x{:X}",
        summary.certificate_chain_size
    );
    eprintln!("  header_size            = 0x{:X}", summary.header_size);
    eprintln!("  payload_offset         = 0x{:X}", summary.payload_offset);
    eprintln!("  payload_size           = 0x{:X}", summary.payload_size);
    if let Some(size) = summary.secondary_size {
        eprintln!("  secondary_size (OID .69)= 0x{size:X}");
    }
    eprintln!("  trailing_size          = 0x{:X}", summary.trailing_size);
    eprintln!(
        "  payload_sha256         = {} ({})",
        summary.declared_payload_sha256,
        if summary.payload_hash_valid {
            "verified"
        } else {
            "MISMATCH"
        }
    );
    eprintln!("--- X.509 certificate chain ---");
    for certificate in &summary.certificates {
        eprintln!(
            "  #{} offset=0x{:X} size=0x{:X} subject={:?}",
            certificate.chain_index + 1,
            certificate.offset,
            certificate.size,
            certificate.subject
        );
        eprintln!(
            "     validity={} .. {} signature={}",
            certificate.not_before, certificate.not_after, certificate.signature_algorithm_oid
        );
    }
    for warning in &summary.warnings {
        eprintln!("  WARN: {warning}");
    }
}

fn print_cert_summary(cert: &CertSummary) {
    eprintln!(
        "  version           = {}.{}",
        cert.version_major, cert.version_minor
    );
    eprintln!("  partition_name    = {:?}", cert.partition_name);
    eprintln!("  image_original_len= 0x{:X}", cert.image_original_len);
    eprintln!("  image_len         = 0x{:X}", cert.image_len);
    eprintln!(
        "  verity_type       = {} ({})",
        cert.verity_type,
        match cert.verity_type {
            1 => "hash",
            2 => "hashtree",
            _ => "?",
        }
    );
    eprintln!(
        "  hash_algo         = {} ({})",
        cert.hash_algo,
        match cert.hash_algo {
            0 => "SHA256",
            1 => "SHA128",
            2 => "SHA512",
            3 => "SM3",
            _ => "?",
        }
    );
    eprintln!("  salt_size         = {}", cert.salt_size);
    eprintln!("  digest_size       = {}", cert.digest_size);
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
