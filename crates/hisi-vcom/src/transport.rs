use std::collections::VecDeque;
use std::fmt;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use nusb::descriptors::TransferType;
use nusb::transfer::{
    Buffer, Bulk, ControlOut, ControlType, Direction, In, Out, Recipient, TransferError,
};
use nusb::{Device, DeviceInfo, Endpoint, Interface, MaybeFuture};
use serialport::{ClearBuffer, SerialPort};

use crate::error::Error;

const VCOM_MARKERS: &[&str] = &["DBAdapter", "USB COM", "PCUI", "PC UI", "VCOM"];
const HUAWEI_VID: u16 = 0x12D1;
const HUAWEI_USB_COM_PID: u16 = 0x3609;

const DISCARD_MAX_READS: usize = 64;

#[derive(Debug, Clone, Default)]
pub struct DeviceFilter {
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub substring: Option<String>,
}

impl DeviceFilter {
    fn matches(&self, info: &DeviceInfo) -> bool {
        if let Some(v) = self.vid {
            if info.vendor_id() != v {
                return false;
            }
        }
        if let Some(p) = self.pid {
            if info.product_id() != p {
                return false;
            }
        }
        if let Some(needle) = &self.substring {
            let needle = needle.to_ascii_lowercase();
            let product_ok = info
                .product_string()
                .is_some_and(|s| s.to_ascii_lowercase().contains(&needle));
            let iface_ok = info.interfaces().any(|i| {
                i.interface_string()
                    .is_some_and(|s| s.to_ascii_lowercase().contains(&needle))
            });
            if !product_ok && !iface_ok {
                return false;
            }
        }
        true
    }
}

impl fmt::Display for DeviceFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vid={:04X}", self.vid.map(|v| v as u32).unwrap_or(0))?;
        if let Some(p) = self.pid {
            write!(f, " pid={:04X}", p)?;
        }
        if let Some(s) = &self.substring {
            write!(f, " match={s:?}")?;
        }
        Ok(())
    }
}

pub trait Transport {
    fn write_all(&mut self, data: &[u8], timeout: Duration) -> Result<(), Error>;
    fn read_raw_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, Error>;
    fn discard_input(&mut self);
}

pub struct SerialVcomDevice {
    port: Box<dyn SerialPort>,
}

impl SerialVcomDevice {
    pub fn open(port_name: &str, baud: u32) -> Result<Self, Error> {
        let mut port = serialport::new(port_name, baud)
            .timeout(Duration::from_secs(10))
            .open()?;

        // Match the control-line state used by the USB CDC implementation.
        port.write_data_terminal_ready(true)?;
        port.write_request_to_send(true)?;

        Ok(Self { port })
    }
}

impl Transport for SerialVcomDevice {
    fn write_all(&mut self, data: &[u8], timeout: Duration) -> Result<(), Error> {
        self.port.set_timeout(timeout)?;
        self.port.write_all(data)?;
        self.port.flush()?;
        Ok(())
    }

    fn read_raw_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, Error> {
        let deadline = Instant::now() + timeout;
        let mut filled = 0;
        while filled < buf.len() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Timeout)?;
            self.port.set_timeout(remaining)?;
            match self.port.read(&mut buf[filled..]) {
                Ok(0) => return Err(Error::Timeout),
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(Error::Timeout);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(filled)
    }

    fn discard_input(&mut self) {
        let _ = self.port.clear(ClearBuffer::Input);
    }
}

#[derive(Debug, Clone)]
pub struct SerialPortCandidate {
    pub name: String,
    pub description: String,
    pub usb: Option<SerialUsbInfo>,
}

#[derive(Debug, Clone)]
pub struct SerialUsbInfo {
    pub vid: u16,
    pub pid: u16,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

impl SerialPortCandidate {
    fn is_vcom(&self) -> bool {
        let Some(usb) = &self.usb else {
            return false;
        };
        usb.vid == HUAWEI_VID
            && (usb.pid == HUAWEI_USB_COM_PID
                || usb.product.as_deref().is_some_and(|product| {
                    let product = product.to_ascii_lowercase();
                    VCOM_MARKERS
                        .iter()
                        .any(|marker| product.contains(&marker.to_ascii_lowercase()))
                }))
    }
}

/// List recognized Huawei VCOM serial ports without opening or probing them.
pub fn list_vcom_serial_ports() -> Result<Vec<SerialPortCandidate>, Error> {
    Ok(list_serial_ports()?
        .into_iter()
        .filter(SerialPortCandidate::is_vcom)
        .collect())
}

pub fn list_serial_ports() -> Result<Vec<SerialPortCandidate>, Error> {
    Ok(serialport::available_ports()?
        .into_iter()
        .map(|port| {
            let (description, usb) = match port.port_type {
                serialport::SerialPortType::UsbPort(info) => {
                    let description = format!(
                        "USB {:04X}:{:04X}{}{}",
                        info.vid,
                        info.pid,
                        info.product
                            .as_deref()
                            .map(|s| format!(" {}", s))
                            .unwrap_or_default(),
                        info.serial_number
                            .as_deref()
                            .map(|s| format!(" serial={s}"))
                            .unwrap_or_default(),
                    );
                    let usb = SerialUsbInfo {
                        vid: info.vid,
                        pid: info.pid,
                        serial_number: info.serial_number,
                        manufacturer: info.manufacturer,
                        product: info.product,
                    };
                    (description, Some(usb))
                }
                serialport::SerialPortType::BluetoothPort => ("Bluetooth".to_owned(), None),
                serialport::SerialPortType::PciPort => ("PCI serial".to_owned(), None),
                serialport::SerialPortType::Unknown => ("Unknown serial device".to_owned(), None),
            };
            SerialPortCandidate {
                name: port.port_name,
                description,
                usb,
            }
        })
        .collect())
}

pub fn cdc_line_coding(baud: u32) -> [u8; 7] {
    let mut lc = [0u8; 7];
    lc[..4].copy_from_slice(&baud.to_le_bytes());
    lc[4] = 0;
    lc[5] = 0;
    lc[6] = 8;
    lc
}

pub struct VcomDevice {
    _device: Device,
    _interface: Interface,
    ep_in: Endpoint<Bulk, In>,
    ep_out: Endpoint<Bulk, Out>,
    in_buf: VecDeque<u8>,
    read_len: usize,
}

impl VcomDevice {
    pub fn open(filter: &DeviceFilter, baud: u32) -> Result<Self, Error> {
        let info = nusb::list_devices()
            .wait()?
            .find(|d| filter.matches(d))
            .ok_or_else(|| Error::NoDevice(filter.to_string()))?;

        let device = info.open().wait()?;
        let (ifnum, in_addr, out_addr, max_packet) = pick_interface(&device, &info, filter)?;

        let interface = device.detach_and_claim_interface(ifnum).wait()?;

        let line_coding = cdc_line_coding(baud);
        let _ = interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: 0x20, // SET_LINE_CODING
                    value: 0,
                    index: ifnum as u16,
                    data: &line_coding,
                },
                Duration::from_millis(200),
            )
            .wait();
        let _ = interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: 0x22, // SET_CONTROL_LINE_STATE: DTR | RTS
                    value: 0x03,
                    index: ifnum as u16,
                    data: &[],
                },
                Duration::from_millis(200),
            )
            .wait();

        let ep_in = interface.endpoint::<Bulk, In>(in_addr)?;
        let ep_out = interface.endpoint::<Bulk, Out>(out_addr)?;
        let read_len = max_packet.saturating_mul(16).max(max_packet);

        Ok(Self {
            _device: device,
            _interface: interface,
            ep_in,
            ep_out,
            in_buf: VecDeque::new(),
            read_len,
        })
    }

    fn bulk_read(&mut self, timeout: Duration) -> Result<Vec<u8>, Error> {
        let completion = self
            .ep_in
            .transfer_blocking(Buffer::new(self.read_len), timeout);
        match completion.into_result() {
            Ok(buf) => Ok(buf.into_vec()),
            Err(TransferError::Cancelled) => Err(Error::Timeout),
            Err(e) => Err(e.into()),
        }
    }
}

impl Transport for VcomDevice {
    fn write_all(&mut self, data: &[u8], timeout: Duration) -> Result<(), Error> {
        let completion = self.ep_out.transfer_blocking(Buffer::from(data), timeout);
        match completion.into_result() {
            Ok(_) => Ok(()),
            Err(TransferError::Cancelled) => Err(Error::Timeout),
            Err(e) => Err(e.into()),
        }
    }

    fn read_raw_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, Error> {
        let deadline = Instant::now() + timeout;
        let mut filled = 0;
        while filled < buf.len() {
            if let Some(b) = self.in_buf.pop_front() {
                buf[filled] = b;
                filled += 1;
                continue;
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Timeout)?;
            let data = self.bulk_read(remaining)?;
            if data.is_empty() {
                return Err(Error::Timeout);
            }
            self.in_buf.extend(data);
        }
        Ok(filled)
    }

    fn discard_input(&mut self) {
        self.in_buf.clear();
        let mut sink = [0u8; 512];
        for _ in 0..DISCARD_MAX_READS {
            match self.read_raw_timeout(&mut sink, Duration::from_millis(20)) {
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
}

fn pick_interface(
    device: &Device,
    info: &DeviceInfo,
    filter: &DeviceFilter,
) -> Result<(u8, u8, u8, usize), Error> {
    let mut best: Option<(i32, u8, u8, u8, usize)> = None;

    for config in device.configurations() {
        for alt in config.interface_alt_settings() {
            let ifnum = alt.interface_number();

            let mut in_ep: Option<(u8, usize)> = None;
            let mut out_ep: Option<u8> = None;
            for ep in alt.endpoints() {
                if ep.transfer_type() != TransferType::Bulk {
                    continue;
                }
                match ep.direction() {
                    Direction::In => {
                        in_ep.get_or_insert((ep.address(), ep.max_packet_size()));
                    }
                    Direction::Out => {
                        out_ep.get_or_insert(ep.address());
                    }
                }
            }
            let (Some((in_addr, max_packet)), Some(out_addr)) = (in_ep, out_ep) else {
                continue;
            };

            let iface_string = info
                .interfaces()
                .find(|i| i.interface_number() == ifnum)
                .and_then(|i| i.interface_string().map(str::to_owned));

            let mut score: i32 = 0;
            if let Some(needle) = &filter.substring {
                let needle = needle.to_ascii_lowercase();
                let product_ok = info
                    .product_string()
                    .is_some_and(|s| s.to_ascii_lowercase().contains(&needle));
                let iface_ok = iface_string
                    .as_deref()
                    .is_some_and(|s| s.to_ascii_lowercase().contains(&needle));
                if product_ok || iface_ok {
                    score += 3;
                } else {
                    continue;
                }
            } else {
                let iface_ok = iface_string.as_deref().is_some_and(|s| {
                    VCOM_MARKERS
                        .iter()
                        .any(|m| s.to_ascii_lowercase().contains(&m.to_ascii_lowercase()))
                });
                let product_ok = info.product_string().is_some_and(|s| {
                    VCOM_MARKERS
                        .iter()
                        .any(|m| s.to_ascii_lowercase().contains(&m.to_ascii_lowercase()))
                });
                if iface_ok {
                    score += 3;
                } else if product_ok {
                    score += 2;
                } else {
                    match alt.class() {
                        0x0A => score += 1, // CDC data
                        0xFF => {}          // vendor-specific
                        _ => continue,      // unrelated Huawei interface
                    }
                }
                if score == 0 {
                    continue;
                }
            }

            let candidate = (score, ifnum, in_addr, out_addr, max_packet);
            if best.is_none() || candidate > best.unwrap() {
                best = Some(candidate);
            }
        }
    }

    let (_, ifnum, in_addr, out_addr, max_packet) = best.ok_or(Error::NoVcomInterface)?;
    Ok((ifnum, in_addr, out_addr, max_packet))
}

pub fn list_candidates(filter: &DeviceFilter) -> Result<Vec<String>, Error> {
    let mut lines = Vec::new();
    for info in nusb::list_devices().wait()? {
        if !filter.matches(&info) {
            continue;
        }
        let interfaces: Vec<String> = info
            .interfaces()
            .map(|i| {
                format!(
                    "if{} class=0x{:02X} {}",
                    i.interface_number(),
                    i.class(),
                    i.interface_string().unwrap_or("")
                )
            })
            .collect();
        lines.push(format!(
            "{:04X}:{:04X} {} [{}]",
            info.vendor_id(),
            info.product_id(),
            info.product_string().unwrap_or(""),
            interfaces.join(", ")
        ));
    }
    Ok(lines)
}
