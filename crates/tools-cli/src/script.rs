use anyhow::{Context, Result, bail};
use common::flash::FlashStep;
use flash_script::{FlashHost, PortChoice};
use std::io::{self, IsTerminal, Write};
use std::path::Path;

pub fn run(path: &Path) -> Result<()> {
    let (_, resolved) = flash_script::load(path)?;
    let mut host = TerminalHost {
        interactive: io::stdin().is_terminal(),
        progress_active: false,
        last_bucket: 0,
    };
    let summary = flash_script::run(&resolved, &mut host)?;
    host.end_progress();
    println!(
        "Flash script finished: {}/{} steps completed.",
        summary.completed, summary.total
    );
    Ok(())
}

struct TerminalHost {
    interactive: bool,
    progress_active: bool,
    last_bucket: u64,
}

const BAR_WIDTH: usize = 30;

impl TerminalHost {
    fn read_line(&mut self, prompt: &str) -> Result<String> {
        self.end_progress();
        print!("{prompt}");
        io::stdout().flush().ok();
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line);
        match read {
            Ok(0) => bail!("standard input closed"),
            Ok(_) => Ok(line.trim().to_owned()),
            Err(error) => Err(error).context("reading standard input"),
        }
    }

    fn end_progress(&mut self) {
        if self.progress_active {
            println!();
            self.progress_active = false;
        }
    }

    fn print_progress_bar(&mut self, sent: u64, total: u64) {
        let sent = sent.min(total);
        let total_nonzero = std::num::NonZeroU64::new(total);
        let percent = total_nonzero.map_or(100, |total| sent.saturating_mul(100) / total);
        let filled = total_nonzero.map_or(BAR_WIDTH, |total| {
            (sent.saturating_mul(BAR_WIDTH as u64) / total) as usize
        });
        let bar = format!("{}{}", "#".repeat(filled), "-".repeat(BAR_WIDTH - filled));
        print!("\r  [{bar}] {percent:3}%  {sent}/{total} bytes");
        let _ = io::stdout().flush();
        self.progress_active = true;
    }
}

impl FlashHost for TerminalHost {
    fn log(&mut self, message: &str) {
        self.end_progress();
        println!("* {message}");
    }

    fn transfer(&mut self, sent: u64, total: u64) {
        if self.interactive {
            self.print_progress_bar(sent, total);
        } else if flash_script::report_bucket(sent, total, &mut self.last_bucket) {
            println!("  {sent}/{total} bytes");
        }
    }

    fn progress(&mut self, step: usize, total: usize, step_ref: &FlashStep) {
        println!(
            "[{}/{}] {}",
            step + 1,
            total,
            flash_script::describe(step_ref)
        );
    }

    fn select_port(&mut self, ports: &[PortChoice]) -> Result<String> {
        let list = ports
            .iter()
            .map(|port| format!("{} ({})", port.name, port.description))
            .collect::<Vec<_>>()
            .join(", ");
        if !self.interactive {
            bail!(
                "multiple VCOM ports found ({list}); pin one port in the script or run in a terminal"
            );
        }
        loop {
            println!("Multiple VCOM ports found:");
            for (index, port) in ports.iter().enumerate() {
                println!("  {}. {} ({})", index + 1, port.name, port.description);
            }
            let answer = self.read_line(&format!("Select a port [1-{}]: ", ports.len()))?;
            if answer.is_empty() {
                continue;
            }
            if let Ok(number) = answer.parse::<usize>()
                && (1..=ports.len()).contains(&number)
            {
                return Ok(ports[number - 1].name.clone());
            }
            if let Some(port) = ports
                .iter()
                .find(|port| port.name.eq_ignore_ascii_case(&answer))
            {
                return Ok(port.name.clone());
            }
            println!("Invalid selection: {answer}");
        }
    }

    fn alert(&mut self, message: &str) -> Result<()> {
        println!("ALERT: {message}");
        println!("continuing in 5 seconds...");
        std::thread::sleep(std::time::Duration::from_secs(5));
        Ok(())
    }
}
