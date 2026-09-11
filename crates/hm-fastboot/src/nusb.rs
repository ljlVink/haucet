use nusb::Endpoint;
use nusb::descriptors::TransferType;
use nusb::transfer::Bulk;
use nusb::transfer::Direction;
use nusb::transfer::{Buffer, In, Out};
pub use nusb::{Device, DeviceInfo, Interface, transfer::TransferError};
use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::str::FromStr;
use thiserror::Error;
use tracing::{info, warn};
use tracing::{instrument, trace};

use crate::protocol::{FastBootCommand, FastBootResponseParseError};
use crate::protocol::{FastBootResponse, parse_u32};
use crate::sparse::{
    CHUNK_HEADER_BYTES_LEN, ChunkHeader, FileHeader, FileHeaderBytes, ParseError, SplitError,
    split_image, split_raw,
};

pub async fn devices() -> Result<impl Iterator<Item = DeviceInfo>, nusb::Error> {
    Ok(nusb::list_devices()
        .await?
        .filter(|d| NusbFastBoot::find_fastboot_interface(d).is_some()))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeviceSelectionError {
    #[error(
        "no fastboot device found: make sure the device is in fastboot mode and connected via USB"
    )]
    NotFound,
    #[error("multiple fastboot devices found; disconnect all but the intended target and retry")]
    Multiple,
}

pub fn require_single_device<T>(
    mut devices: impl Iterator<Item = T>,
) -> Result<T, DeviceSelectionError> {
    let device = devices.next().ok_or(DeviceSelectionError::NotFound)?;
    if devices.next().is_some() {
        return Err(DeviceSelectionError::Multiple);
    }
    Ok(device)
}

pub fn clean_device_string(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.is_empty() || !bytes.iter().all(|b| (0x20..=0x7e).contains(b)) {
        return None;
    }
    let cleaned = String::from_utf8(bytes).ok()?;
    (cleaned != s).then_some(cleaned)
}

#[derive(Debug, Error)]
pub enum NusbFastBootError {
    #[error("Transfer error: {0}")]
    Transfer(#[from] TransferError),
    #[error("Fastboot client failure: {0}")]
    FastbootFailed(String),
    #[error("Unexpected fastboot response")]
    FastbootUnexpectedReply,
    #[error("Unknown fastboot response: {0}")]
    FastbootParseError(#[from] FastBootResponseParseError),
}

#[derive(Debug, Error)]
pub enum NusbFastBootOpenError {
    #[error("Failed to open device: {0}")]
    Device(nusb::Error),
    #[error("Failed to claim interface: {0}")]
    Interface(nusb::Error),
    #[error("Failed to find interface for fastboot")]
    MissingInterface,
    #[error("Failed to find required endpoints for fastboot")]
    MissingEndpoints,
    #[error("Unknown fastboot response: {0}")]
    FastbootParseError(#[from] FastBootResponseParseError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StorageRangeParseError {
    #[error("storage range must be OFFSET:LENGTH")]
    Format,
    #[error("invalid storage offset: {0}")]
    Offset(String),
    #[error("invalid storage length: {0}")]
    Length(String),
    #[error("storage length must not be zero")]
    ZeroLength,
    #[error("storage range overflows the 64-bit address space")]
    Overflow,
}

impl FromStr for StorageRange {
    type Err = StorageRangeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (offset, length) = value
            .trim()
            .split_once(':')
            .ok_or(StorageRangeParseError::Format)?;
        if offset.is_empty() || length.is_empty() || length.contains(':') {
            return Err(StorageRangeParseError::Format);
        }

        let offset = parse_storage_hex(offset)
            .ok_or_else(|| StorageRangeParseError::Offset(offset.to_owned()))?;
        let length = parse_storage_hex(length)
            .ok_or_else(|| StorageRangeParseError::Length(length.to_owned()))?;
        if length == 0 {
            return Err(StorageRangeParseError::ZeroLength);
        }
        offset
            .checked_add(length)
            .ok_or(StorageRangeParseError::Overflow)?;

        Ok(Self { offset, length })
    }
}

fn parse_storage_hex(value: &str) -> Option<u64> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    (!value.is_empty())
        .then(|| u64::from_str_radix(value, 16).ok())
        .flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractPartEvent {
    Started(StorageRange),
    Progress { written: u64, total: u64 },
}

#[derive(Debug, Error)]
pub enum ExtractPartError {
    #[error(transparent)]
    Fastboot(#[from] NusbFastBootError),
    #[error("invalid storage range returned for partition {partition}: {value:?}: {source}")]
    InvalidStorageRange {
        partition: String,
        value: String,
        #[source]
        source: StorageRangeParseError,
    },
    #[error("output image I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub struct NusbFastBoot {
    ep_out: Endpoint<Bulk, Out>,
    max_out: usize,
    ep_in: Endpoint<Bulk, In>,
    max_in: usize,
}

const HARMONY_BOOT_ID: &[(u16, u16)] = &[(0x12d1, 0x1100), (0x12d1, 0x1101)];
const EXTRACT_PART_CHUNK_SIZE: u64 = 64 * 1024 * 1024;
const DOWNLOAD_CHUNK_SIZE: u32 = 1024 * 1024;

impl NusbFastBoot {
    pub fn find_fastboot_interface(info: &DeviceInfo) -> Option<u8> {
        let standard = info.interfaces().find_map(|i| {
            if i.class() == 0xff && i.subclass() == 0x42 && i.protocol() == 0x3 {
                Some(i.interface_number())
            } else {
                None
            }
        });
        standard.or_else(|| {
            let known = HARMONY_BOOT_ID
                .iter()
                .any(|&(vid, pid)| vid == info.vendor_id() && pid == info.product_id());
            known.then(|| {
                info.interfaces()
                    .next()
                    .map(|i| i.interface_number())
                    .unwrap_or(0)
            })
        })
    }

    #[tracing::instrument(skip_all, err)]
    pub fn from_interface(interface: Interface) -> Result<Self, NusbFastBootOpenError> {
        let (ep_out, max_out, ep_in, max_in) = interface
            .descriptors()
            .find_map(|alt| {
                let (ep_out, max_out) = alt.endpoints().find_map(|end| {
                    if end.transfer_type() == TransferType::Bulk
                        && end.direction() == Direction::Out
                    {
                        Some((end.address(), end.max_packet_size()))
                    } else {
                        None
                    }
                })?;

                let (ep_in, max_in) = alt.endpoints().find_map(|end| {
                    if end.transfer_type() == TransferType::Bulk && end.direction() == Direction::In
                    {
                        Some((end.address(), end.max_packet_size()))
                    } else {
                        None
                    }
                })?;
                Some((ep_out, max_out, ep_in, max_in))
            })
            .ok_or(NusbFastBootOpenError::MissingEndpoints)?;
        trace!(
            "Fastboot endpoints: OUT: {} (max: {}), IN: {} (max: {})",
            ep_out, max_out, ep_in, max_in
        );
        let ep_out = interface
            .endpoint::<Bulk, Out>(ep_out)
            .map_err(NusbFastBootOpenError::Interface)?;
        let ep_in = interface
            .endpoint::<Bulk, In>(ep_in)
            .map_err(NusbFastBootOpenError::Interface)?;
        Ok(Self {
            ep_out,
            max_out,
            ep_in,
            max_in,
        })
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn from_device(device: Device, interface: u8) -> Result<Self, NusbFastBootOpenError> {
        let interface = device
            .claim_interface(interface)
            .await
            .map_err(NusbFastBootOpenError::Interface)?;
        Self::from_interface(interface)
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn from_info(info: &DeviceInfo) -> Result<Self, NusbFastBootOpenError> {
        let interface =
            Self::find_fastboot_interface(info).ok_or(NusbFastBootOpenError::MissingInterface)?;
        let device = info.open().await.map_err(NusbFastBootOpenError::Device)?;
        Self::from_device(device, interface).await
    }

    #[tracing::instrument(skip_all, err)]
    async fn send_data(&mut self, data: Vec<u8>) -> Result<(), NusbFastBootError> {
        self.ep_out.submit(data.into());
        self.ep_out.next_complete().await.into_result()?;
        Ok(())
    }

    async fn send_command<S: Display>(
        &mut self,
        cmd: FastBootCommand<S>,
    ) -> Result<(), NusbFastBootError> {
        let mut out = vec![];
        out.write_fmt(format_args!("{}", cmd)).unwrap();
        trace!(
            "Sending command: {}",
            std::str::from_utf8(&out).unwrap_or("Invalid utf-8")
        );
        self.send_data(out).await
    }

    #[tracing::instrument(skip_all, err)]
    async fn read_response(&mut self) -> Result<FastBootResponse, NusbFastBootError> {
        self.ep_in.submit(Buffer::new(self.max_in));
        let resp = self
            .ep_in
            .next_complete()
            .await
            .into_result()
            .map_err(NusbFastBootError::Transfer)?;
        Ok(FastBootResponse::from_bytes(&resp)?)
    }

    #[tracing::instrument(skip_all, err)]
    async fn handle_responses(&mut self) -> Result<String, NusbFastBootError> {
        loop {
            let resp = self.read_response().await?;
            trace!("Response: {:?}", resp);
            match resp {
                FastBootResponse::Info(_) => (),
                FastBootResponse::Text(_) => (),
                FastBootResponse::Data(_) => {
                    return Err(NusbFastBootError::FastbootUnexpectedReply);
                }
                FastBootResponse::Okay(value) => return Ok(value),
                FastBootResponse::Fail(fail) => {
                    return Err(NusbFastBootError::FastbootFailed(fail));
                }
            }
        }
    }

    #[tracing::instrument(skip_all, err)]
    async fn execute<S: Display>(
        &mut self,
        cmd: FastBootCommand<S>,
    ) -> Result<String, NusbFastBootError> {
        self.send_command(cmd).await?;
        self.handle_responses().await
    }

    fn allocate(&self) -> Buffer {
        let size = (1024usize * 1024).next_multiple_of(self.max_out);
        self.ep_out.allocate(size)
    }

    pub async fn get_var(&mut self, var: &str) -> Result<String, NusbFastBootError> {
        let cmd = FastBootCommand::GetVar(var);
        self.execute(cmd).await
    }

    pub async fn download(&'_ mut self, size: u32) -> Result<DataDownload<'_>, NusbFastBootError> {
        let cmd = FastBootCommand::<&str>::Download(size);
        self.send_command(cmd).await?;
        loop {
            let resp = self.read_response().await?;
            match resp {
                FastBootResponse::Info(i) => info!("info: {i}"),
                FastBootResponse::Text(t) => info!("Text: {}", t),
                FastBootResponse::Data(size) => {
                    return Ok(DataDownload::new(self, size));
                }
                FastBootResponse::Okay(_) => {
                    return Err(NusbFastBootError::FastbootUnexpectedReply);
                }
                FastBootResponse::Fail(fail) => {
                    return Err(NusbFastBootError::FastbootFailed(fail));
                }
            }
        }
    }

    pub async fn flash(&mut self, target: &str) -> Result<(), NusbFastBootError> {
        let cmd = FastBootCommand::Flash(target);
        self.execute(cmd).await.map(|v| {
            trace!("Flash ok: {v}");
        })
    }

    pub async fn continue_boot(&mut self) -> Result<(), NusbFastBootError> {
        let cmd = FastBootCommand::<&str>::Continue;
        self.execute(cmd).await.map(|v| {
            trace!("Continue ok: {v}");
        })
    }

    pub async fn erase(&mut self, target: &str) -> Result<(), NusbFastBootError> {
        let cmd = FastBootCommand::Erase(target);
        self.execute(cmd).await.map(|v| {
            trace!("Erase ok: {v}");
        })
    }

    pub async fn ultraflash(&mut self, target: &str) -> Result<(), NusbFastBootError> {
        let cmd = FastBootCommand::Ultraflash(target);
        self.execute(cmd).await.map(|v| {
            trace!("Ultraflash ok: {v}");
        })
    }

    pub async fn ultraflash_stop(&mut self) -> Result<(), NusbFastBootError> {
        self.execute(FastBootCommand::<&str>::UltraflashStop)
            .await
            .map(|v| {
                trace!("Ultraflash stop ok: {v}");
            })
    }

    pub async fn reboot_bootloader(&mut self) -> Result<(), NusbFastBootError> {
        self.execute(FastBootCommand::<&str>::RebootBootloader)
            .await
            .map(|v| {
                trace!("Reboot bootloader ok: {v}");
            })
    }

    pub async fn reboot_recovery(&mut self) -> Result<(), NusbFastBootError> {
        self.execute(FastBootCommand::<&str>::RebootRecovery)
            .await
            .map(|v| {
                trace!("Reboot recovery ok: {v}");
            })
    }

    pub async fn reboot_fastboot(&mut self) -> Result<(), NusbFastBootError> {
        self.execute(FastBootCommand::<&str>::RebootFastboot)
            .await
            .map(|v| {
                trace!("Reboot fastboot ok: {v}");
            })
    }

    pub async fn upload_memory(
        &mut self,
        params: &str,
        size: u32,
    ) -> Result<Vec<u8>, NusbFastBootError> {
        let cmd = FastBootCommand::UploadMemory(params);
        self.upload_data(cmd, size).await
    }

    pub async fn upload_storage(
        &mut self,
        params: &str,
        size: u32,
    ) -> Result<Vec<u8>, NusbFastBootError> {
        let cmd = FastBootCommand::UploadStorage(params);
        self.upload_data(cmd, size).await
    }

    pub async fn read_storage_head(&mut self) -> Result<Vec<u8>, NusbFastBootError> {
        // Initialize storage access before uploading; otherwise the device may
        // reject the request with "Not Ready". The OEMINFO range is not needed.
        self.get_var("storage:oeminfo").await?;
        self.upload_storage("0:0x100000", 0x100000).await
    }

    pub async fn extract_part(
        &mut self,
        partition: &str,
        output: &Path,
        progress: &mut dyn FnMut(ExtractPartEvent),
    ) -> Result<StorageRange, ExtractPartError> {
        let variable = format!("storage:{partition}");
        let value = self.get_var(&variable).await?;
        let range = value
            .parse()
            .map_err(|source| ExtractPartError::InvalidStorageRange {
                partition: partition.to_owned(),
                value,
                source,
            })?;
        progress(ExtractPartEvent::Started(range));

        let file = File::create(output)?;
        let mut writer = BufWriter::new(file);
        let mut written = 0u64;
        while written < range.length {
            let length = (range.length - written).min(EXTRACT_PART_CHUNK_SIZE) as u32;
            let offset = range.offset + written;
            let params = format!("{offset:016x}:{length:016x}");
            self.upload_storage_to(&params, length, &mut writer).await?;
            written += u64::from(length);
            progress(ExtractPartEvent::Progress {
                written,
                total: range.length,
            });
        }
        writer.flush()?;
        Ok(range)
    }

    async fn upload_storage_to<W: Write>(
        &mut self,
        params: &str,
        size: u32,
        output: &mut W,
    ) -> Result<(), ExtractPartError> {
        self.send_command(FastBootCommand::UploadStorage(params))
            .await?;

        loop {
            match self.read_response().await? {
                FastBootResponse::Info(i) => info!("info: {i}"),
                FastBootResponse::Text(t) => info!("Text: {t}"),
                FastBootResponse::Okay(_) => break,
                FastBootResponse::Data(_) => {
                    return Err(NusbFastBootError::FastbootUnexpectedReply.into());
                }
                FastBootResponse::Fail(fail) => {
                    return Err(NusbFastBootError::FastbootFailed(fail).into());
                }
            }
        }

        let mut received = 0u32;
        while received < size {
            self.ep_in.submit(Buffer::new(self.max_in));
            let chunk = self
                .ep_in
                .next_complete()
                .await
                .into_result()
                .map_err(NusbFastBootError::Transfer)?;
            if chunk.is_empty() {
                return Err(NusbFastBootError::FastbootUnexpectedReply.into());
            }
            let remaining = (size - received) as usize;
            let amount = chunk.len().min(remaining);
            output.write_all(&chunk[..amount])?;
            received += amount as u32;
        }

        self.handle_responses().await?;
        Ok(())
    }

    async fn upload_data<S: Display>(
        &mut self,
        cmd: FastBootCommand<S>,
        size: u32,
    ) -> Result<Vec<u8>, NusbFastBootError> {
        self.send_command(cmd).await?;

        loop {
            match self.read_response().await? {
                FastBootResponse::Info(i) => info!("info: {i}"),
                FastBootResponse::Text(t) => info!("Text: {t}"),
                FastBootResponse::Okay(_) => break,
                FastBootResponse::Data(_) => {
                    return Err(NusbFastBootError::FastbootUnexpectedReply);
                }
                FastBootResponse::Fail(fail) => {
                    return Err(NusbFastBootError::FastbootFailed(fail));
                }
            }
        }

        let mut data = Vec::with_capacity(size as usize);
        while data.len() < size as usize {
            self.ep_in.submit(Buffer::new(self.max_in));
            let chunk = self
                .ep_in
                .next_complete()
                .await
                .into_result()
                .map_err(NusbFastBootError::Transfer)?;
            if chunk.is_empty() {
                return Err(NusbFastBootError::FastbootUnexpectedReply);
            }
            let remaining = size as usize - data.len();
            data.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }

        self.handle_responses().await?;
        Ok(data)
    }

    pub async fn reboot(&mut self) -> Result<(), NusbFastBootError> {
        let cmd = FastBootCommand::<&str>::Reboot;
        self.execute(cmd).await.map(|v| {
            trace!("Reboot ok: {v}");
        })
    }

    pub async fn reboot_to(&mut self, mode: &str) -> Result<(), NusbFastBootError> {
        let cmd = FastBootCommand::<&str>::RebootTo(mode);
        self.execute(cmd).await.map(|v| {
            trace!("Reboot ok: {v}");
        })
    }

    pub async fn get_all_vars(&mut self) -> Result<HashMap<String, String>, NusbFastBootError> {
        let cmd = FastBootCommand::GetVar("all");
        self.send_command(cmd).await?;
        let mut vars = HashMap::new();
        loop {
            let resp = self.read_response().await?;
            trace!("Response: {:?}", resp);
            match resp {
                FastBootResponse::Info(i) => {
                    let Some((key, value)) = i.rsplit_once(':') else {
                        warn!("Failed to parse variable: {i}");
                        continue;
                    };
                    vars.insert(key.trim().to_string(), value.trim().to_string());
                }
                FastBootResponse::Text(t) => info!("Text: {}", t),
                FastBootResponse::Data(_) => {
                    return Err(NusbFastBootError::FastbootUnexpectedReply);
                }
                FastBootResponse::Okay(_) => {
                    return Ok(vars);
                }
                FastBootResponse::Fail(fail) => {
                    return Err(NusbFastBootError::FastbootFailed(fail));
                }
            }
        }
    }

    pub async fn oem(&mut self, command: &str) -> Result<Vec<String>, NusbFastBootError> {
        let cmd = FastBootCommand::Oem(command);
        self.send_command(cmd).await?;
        let mut lines = Vec::new();
        loop {
            let resp = self.read_response().await?;
            trace!("Response: {:?}", resp);
            match resp {
                FastBootResponse::Info(i) => lines.push(i),
                FastBootResponse::Text(t) => lines.push(t),
                FastBootResponse::Data(_) => {
                    return Err(NusbFastBootError::FastbootUnexpectedReply);
                }
                FastBootResponse::Okay(_) => return Ok(lines),
                FastBootResponse::Fail(fail) => {
                    return Err(NusbFastBootError::FastbootFailed(fail));
                }
            }
        }
    }

    #[tracing::instrument(skip_all, err)]
    pub async fn flash_image(
        &mut self,
        target: &str,
        path: &Path,
        progress: &mut dyn FnMut(FlashEvent<'_>),
    ) -> Result<(), FlashError> {
        match self.ultraflash(target).await {
            Ok(()) => {
                progress(FlashEvent::Message("Using Ultraflash protocol"));
                let file = std::fs::File::open(path)?;
                let file_len = file.metadata()?.len();
                let file_size =
                    u32::try_from(file_len).map_err(|_| FlashError::TooLarge(file_len))?;
                let download_result = ultraflash_raw(self, file, file_size, progress).await;
                let stop_result = self.ultraflash_stop().await;
                download_result?;
                stop_result?;
                return Ok(());
            }
            Err(NusbFastBootError::FastbootFailed(_)) => {
                progress(FlashEvent::Message(
                    "Ultraflash is not supported; using standard fastboot flash",
                ));
            }
            Err(error) => return Err(error.into()),
        }
        let max_download = self.get_var("max-download-size").await?;
        let max_download = parse_u32(&max_download).map_err(|e| {
            NusbFastBootError::FastbootFailed(format!(
                "Failed to parse max download size: {max_download}: {e}"
            ))
        })?;
        progress(FlashEvent::Message(&format!(
            "Max download size: {max_download} bytes"
        )));

        let mut file = std::fs::File::open(path)?;
        let mut header_bytes = FileHeaderBytes::default();
        file.read_exact(&mut header_bytes)?;

        let splits = match FileHeader::from_bytes(&header_bytes) {
            Ok(header) => {
                progress(FlashEvent::Message(
                    "Preparing to flash Android sparse image",
                ));
                let mut chunks = vec![];
                for _ in 0..header.chunks {
                    let mut chunk_bytes = [0; CHUNK_HEADER_BYTES_LEN];
                    file.read_exact(&mut chunk_bytes)?;
                    let chunk = ChunkHeader::from_bytes(&chunk_bytes)?;
                    file.seek(SeekFrom::Current(chunk.data_size() as i64))?;
                    chunks.push(chunk);
                }
                split_image(&header, &chunks, max_download)?
            }
            Err(ParseError::UnknownMagic) => {
                file.seek(SeekFrom::Start(0))?;
                let file_size = file.seek(SeekFrom::End(0))?;
                if file_size < max_download.into() {
                    file.seek(SeekFrom::Start(0))?;
                    return flash_raw(self, target, file, file_size as u32, progress).await;
                }
                split_raw(file_size as usize, max_download)?
            }
            Err(e) => return Err(e.into()),
        };

        let total = splits.len();
        progress(FlashEvent::Part { index: 0, total });
        for (i, split) in splits.iter().enumerate() {
            progress(FlashEvent::Message(&format!("Downloading part {i}")));
            let mut sender = self.download(split.sparse_size() as u32).await?;

            sender.extend_from_slice(&split.header.to_bytes()).await?;
            for chunk in &split.chunks {
                sender.extend_from_slice(&chunk.header.to_bytes()).await?;
                file.seek(SeekFrom::Start(chunk.offset as u64))?;
                let mut left = chunk.size;
                while left > 0 {
                    let buf = sender.get_mut_data(left).await?;
                    left -= read_exact_padded(&mut file, buf)?;
                }
            }
            sender.finish().await?;

            progress(FlashEvent::Message(&format!("Flashing part {i}")));
            self.flash(target).await?;
            progress(FlashEvent::Part {
                index: i + 1,
                total,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashEvent<'a> {
    Message(&'a str),
    Part { index: usize, total: usize },
}

#[derive(Debug, Error)]
pub enum FlashError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse image: {0}")]
    Parse(#[from] ParseError),
    #[error("Failed to split image: {0}")]
    Split(#[from] SplitError),
    #[error("Download failed: {0}")]
    Download(#[from] DownloadError),
    #[error("Fastboot transfer failed: {0}")]
    Fastboot(#[from] NusbFastBootError),
    #[error("Image too large for ultraflash download: {0} bytes")]
    TooLarge(u64),
}

async fn flash_raw(
    fb: &mut NusbFastBoot,
    target: &str,
    mut file: std::fs::File,
    file_size: u32,
    progress: &mut dyn FnMut(FlashEvent<'_>),
) -> Result<(), FlashError> {
    progress(FlashEvent::Message("Uploading raw image directly"));
    let mut sender = fb.download(file_size).await?;
    loop {
        let left = sender.left();
        if left == 0 {
            break;
        }
        let buf = sender.get_mut_data(left as usize).await?;
        file.read_exact(buf)?;
    }
    sender.finish().await?;

    progress(FlashEvent::Message("Flashing data"));
    fb.flash(target).await?;
    Ok(())
}

async fn ultraflash_raw(
    fb: &mut NusbFastBoot,
    mut file: std::fs::File,
    file_size: u32,
    progress: &mut dyn FnMut(FlashEvent<'_>),
) -> Result<(), FlashError> {
    let mut sender = fb.download(file_size).await?;
    loop {
        let left = sender.left();
        if left == 0 {
            break;
        }
        let amount = left.min(DOWNLOAD_CHUNK_SIZE) as usize;
        let buf = sender.get_mut_data(amount).await?;
        file.read_exact(buf)?;
    }
    sender.finish().await?;
    progress(FlashEvent::Message("Flashing data"));
    Ok(())
}

fn read_exact_padded<R: Read>(input: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let total = buf.len();
    let mut offset = 0;
    while offset < total {
        match input.read(&mut buf[offset..]) {
            Ok(0) => {
                /* EOF, fill the remainder with 0 */
                buf[offset..].fill(0);
                break;
            }
            Ok(read) => offset += read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(total)
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("Trying to complete while nothing was Queued")]
    NothingQueued,
    #[error("Incorrect data length: expected {expected}, got {actual}")]
    IncorrectDataLength { actual: u32, expected: u32 },
    #[error(transparent)]
    Nusb(#[from] NusbFastBootError),
}

pub struct DataDownload<'s> {
    fastboot: &'s mut NusbFastBoot,
    size: u32,
    left: u32,
    current: Buffer,
}

impl<'s> DataDownload<'s> {
    fn new(fastboot: &'s mut NusbFastBoot, size: u32) -> DataDownload<'s> {
        let current = fastboot.allocate();
        Self {
            fastboot,
            size,
            left: size,
            current,
        }
    }
}

impl DataDownload<'_> {
    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn left(&self) -> u32 {
        self.left
    }

    pub async fn extend_from_slice(&mut self, mut data: &[u8]) -> Result<(), DownloadError> {
        self.update_size(data.len() as u32)?;
        loop {
            let left = self.current.capacity() - self.current.len();
            if left >= data.len() {
                self.current.extend_from_slice(data);
                break;
            } else {
                self.current.extend_from_slice(&data[0..left]);
                self.next_buffer().await?;
                data = &data[left..];
            }
        }
        Ok(())
    }

    pub async fn get_mut_data(&mut self, max: usize) -> Result<&mut [u8], DownloadError> {
        if self.current.capacity() == self.current.len() {
            self.next_buffer().await?;
        }

        let left = self.current.capacity() - self.current.len();
        let size = left.min(max);
        self.update_size(size as u32)?;

        let len = self.current.len();
        self.current.extend_fill(size, 0);
        Ok(&mut self.current[len..])
    }

    fn update_size(&mut self, size: u32) -> Result<(), DownloadError> {
        if size > self.left {
            return Err(DownloadError::IncorrectDataLength {
                expected: self.size,
                actual: size - self.left + self.size,
            });
        }
        self.left -= size;
        Ok(())
    }

    async fn next_buffer(&mut self) -> Result<(), DownloadError> {
        let mut next = if self.fastboot.ep_out.pending() < 3 {
            self.fastboot.allocate()
        } else {
            let mut completion = self.fastboot.ep_out.next_complete().await;
            completion.status.map_err(NusbFastBootError::from)?;
            completion.buffer.clear();
            completion.buffer
        };

        std::mem::swap(&mut next, &mut self.current);
        self.fastboot.ep_out.submit(next);

        Ok(())
    }

    #[instrument(skip_all, err)]
    pub async fn finish(self) -> Result<(), DownloadError> {
        if self.left != 0 {
            return Err(DownloadError::IncorrectDataLength {
                expected: self.size,
                actual: self.size - self.left,
            });
        }

        if !self.current.is_empty() {
            self.fastboot.ep_out.submit(self.current);
        }

        while self.fastboot.ep_out.pending() > 0 {
            let completion = self.fastboot.ep_out.next_complete().await;
            completion.status.map_err(NusbFastBootError::from)?;
        }

        self.fastboot.handle_responses().await?;
        Ok(())
    }
}
