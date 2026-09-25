use anyhow::{Context, Result, ensure};
use hisi_vcom::transport::{self, SerialVcomDevice};
use hisi_vcom::vcom;
use std::cell::Cell;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::num::NonZeroU64;
use std::path::Path;

pub fn devices() -> Result<()> {
    let mut found = false;

    for port in
        transport::list_vcom_serial_ports().context("failed to enumerate VCOM serial ports")?
    {
        found = true;
        println!("{:<8}  {}", port.name, port.description);
    }

    if !found {
        println!("No VCOM devices found.");
    }
    Ok(())
}

pub fn flash(port: &str, address: u32, file: &Path) -> Result<()> {
    let data = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    validate_loader(&data, file)?;
    let mut device = SerialVcomDevice::open(port, 115200)
        .with_context(|| format!("opening VCOM port {port}"))?;
    let reporter = UploadReporter::new();

    vcom::upload(
        &mut device,
        &data,
        address,
        &mut |message| reporter.log(message),
        &mut |sent, total| reporter.progress(sent, total),
    )?;
    reporter.finish_line();
    println!("Flash finished.");
    Ok(())
}

struct UploadReporter {
    interactive: bool,
    progress_active: Cell<bool>,
    last_bucket: Cell<u64>,
}

impl UploadReporter {
    fn new() -> Self {
        Self {
            interactive: io::stdout().is_terminal(),
            progress_active: Cell::new(false),
            last_bucket: Cell::new(0),
        }
    }

    fn log(&self, message: &str) {
        self.finish_line();
        println!("* {message}");
    }

    fn progress(&self, sent: u64, total: u64) {
        if self.interactive {
            print_progress(sent, total);
            self.progress_active.set(true);
        } else if should_report_progress(sent, total, &self.last_bucket) {
            println!("  {sent}/{total} bytes");
        }
    }

    fn finish_line(&self) {
        if self.progress_active.replace(false) {
            println!();
        }
    }
}

fn print_progress(sent: u64, total: u64) {
    const BAR_WIDTH: usize = 30;

    let sent = sent.min(total);
    let total_nonzero = NonZeroU64::new(total);
    let percent = total_nonzero.map_or(100, |total| sent.saturating_mul(100) / total.get());
    let filled = total_nonzero.map_or(BAR_WIDTH, |total| {
        (sent.saturating_mul(BAR_WIDTH as u64) / total.get()) as usize
    });
    let bar = format!("{}{}", "#".repeat(filled), "-".repeat(BAR_WIDTH - filled));

    print!("\r  [{bar}] {percent:3}%  {sent}/{total} bytes");
    let _ = io::stdout().flush();
}

fn validate_loader(data: &[u8], file: &Path) -> Result<()> {
    ensure!(!data.is_empty(), "loader file is empty: {}", file.display());
    Ok(())
}

fn should_report_progress(sent: u64, total: u64, last_bucket: &Cell<u64>) -> bool {
    if total == 0 {
        return false;
    }

    let bucket = sent.min(total).saturating_mul(10) / total;
    if bucket <= last_bucket.get() {
        return false;
    }
    last_bucket.set(bucket);
    true
}
