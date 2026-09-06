use std::fs::File;

use crate::error::Result;

pub struct Device {
    pub file: File,
    pub offset: u64,
}

impl Device {
    pub fn open(path: &str, offset: u64) -> Result<Device> {
        let file = File::open(path)?;
        Ok(Device { file, offset })
    }

    pub fn read_at(&self, buf: &mut [u8], pos: u64) -> Result<()> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }
        let mut read = 0usize;
        while read < len {
            let n = crate::platform::read_at(
                &self.file,
                &mut buf[read..],
                self.offset + pos + read as u64,
            )?;
            if n == 0 {
                break;
            }
            read += n;
        }
        if read < len {
            buf[read..].fill(0);
        }
        Ok(())
    }

    pub fn read_block(&self, blksz: u32, pos: u64, out: &mut Vec<u8>) -> Result<()> {
        out.resize(blksz as usize, 0);
        self.read_at(out, pos)?;
        Ok(())
    }
}

pub fn dev_read_vec(dev: &Device, pos: u64, len: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; len];
    dev.read_at(&mut v, pos)?;
    Ok(v)
}

pub fn read_meta_block(dev: &Device, blksz: u32, offset: u64) -> Result<Vec<u8>> {
    let mut v = vec![0u8; blksz as usize];
    dev.read_at(&mut v, crate::erofs_fs::round_down(offset, blksz as u64))?;
    Ok(v)
}
