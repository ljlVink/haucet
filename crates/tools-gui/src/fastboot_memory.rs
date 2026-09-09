use anyhow::{Context, Result, ensure};
use hm_fastboot::nusb::DeviceInfo;
use serde::{Deserialize, Serialize};

const CHUNK_SIZE: u32 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDevice {
    bus: String,
    address: u8,
    vid: u16,
    pid: u16,
    serial: Option<String>,
}

impl MemoryDevice {
    pub fn from_info(info: &DeviceInfo) -> Self {
        Self {
            bus: info.bus_id().to_owned(),
            address: info.device_address(),
            vid: info.vendor_id(),
            pid: info.product_id(),
            serial: info.serial_number().map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMap {
    pub device: MemoryDevice,
    pub regions: Vec<MemoryRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRegion {
    pub name: String,
    pub base: u64,
    pub size: u32,
}

impl MemoryRegion {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.is_empty() && !self.name.chars().any(char::is_control),
            "{}",
            tr!("fastboot-memory-invalid-name")
        );
        ensure!(
            self.size != 0 && self.base.is_multiple_of(4),
            "{}",
            tr!("fastboot-memory-invalid-range")
        );
        self.base
            .checked_add(u64::from(self.size))
            .context(tr!("fastboot-memory-invalid-range"))?;
        Ok(())
    }

    /// Call after validation; every request starts on a four-byte boundary.
    pub fn chunks(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        (0..self.size).step_by(CHUNK_SIZE as usize).map(|offset| {
            (
                self.base + u64::from(offset),
                (self.size - offset).min(CHUNK_SIZE),
            )
        })
    }

    pub fn suggested_filename(&self) -> String {
        // Device-provided names must never become paths or Windows device names.
        let name: String = self
            .name
            .chars()
            .take(80)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("{name}_{:x}.bin", self.base)
    }
}

pub fn parse_ddrdump(output: &str) -> Result<Vec<MemoryRegion>> {
    let mut regions = Vec::new();
    for (index, line) in output.lines().enumerate() {
        let base_start = field_start(line, "base");
        if base_start.is_none()
            && field_start(line, "size").is_none()
            && field_start(line, "mem").is_none()
        {
            continue;
        }
        let parse = || -> Option<MemoryRegion> {
            let (base, rest) = line[base_start?..].split_once(',')?;
            let mem_start = field_start(rest, "mem")?;
            let size = rest[..mem_start].trim().trim_end_matches(',');
            Some(MemoryRegion {
                name: field_value(&rest[mem_start..], "mem")?.to_owned(),
                base: hex_value(field_value(base, "base")?)?,
                size: u32::try_from(hex_value(field_value(size, "size")?)?).ok()?,
            })
        };
        let error = || tr!("fastboot-memory-parse-line", "line" => index + 1);
        let region = parse().with_context(error)?;
        region.validate().with_context(error)?;
        regions.push(region);
    }
    ensure!(!regions.is_empty(), "{}", tr!("fastboot-memory-empty"));
    Ok(regions)
}

fn field_start(text: &str, name: &str) -> Option<usize> {
    text.match_indices(name).find_map(|(index, _)| {
        let before = text[..index].chars().next_back();
        let boundary = before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
            || text[..index].ends_with("INFO")
            || text[..index].ends_with("TEXT");
        (boundary && text[index + name.len()..].trim_start().starts_with(':')).then_some(index)
    })
}

fn field_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.trim()
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix(':')
        .map(str::trim)
}

fn hex_value(text: &str) -> Option<u64> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))?;
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}
