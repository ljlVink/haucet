use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCRIPT_VERSION: u32 = 1;
pub const MAX_STEPS: usize = 256;
pub const PORT_AUTO: &str = "auto";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashScript {
    pub version: u32,
    #[serde(default)]
    pub name: String,
    pub steps: Vec<FlashStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FlashStep {
    WaitVcom {
        timeout_secs: u64,
    },
    VcomUpload {
        port: String,
        address: String,
        file: String,
    },
    WaitFastboot {
        timeout_secs: u64,
    },
    FastbootAssert {
        variable: String,
        value: String,
    },
    FastbootFlash {
        partition: String,
        file: String,
    },
    FastbootErase {
        partition: String,
    },
    FastbootOem {
        command: String,
    },
    FastbootReboot {
        mode: RebootMode,
    },
    Alert {
        message: String,
    },
    Sleep {
        millis: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RebootMode {
    System,
    Bootloader,
    Fastboot,
    Recovery,
    Continue,
}

impl FlashStep {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::WaitVcom { .. } => "wait_vcom",
            Self::VcomUpload { .. } => "vcom_upload",
            Self::WaitFastboot { .. } => "wait_fastboot",
            Self::FastbootAssert { .. } => "fastboot_assert",
            Self::FastbootFlash { .. } => "fastboot_flash",
            Self::FastbootErase { .. } => "fastboot_erase",
            Self::FastbootOem { .. } => "fastboot_oem",
            Self::FastbootReboot { .. } => "fastboot_reboot",
            Self::Alert { .. } => "alert",
            Self::Sleep { .. } => "sleep",
        }
    }

    fn validate(&self, index: usize) -> Result<()> {
        let at = || format!("step {} ({}):", index + 1, self.kind());
        match self {
            Self::WaitVcom { timeout_secs } | Self::WaitFastboot { timeout_secs } => {
                ensure!(
                    *timeout_secs > 0,
                    "{} timeout must be at least 1 second",
                    at()
                );
            }
            Self::VcomUpload {
                port,
                address,
                file,
            } => {
                ensure!(!port.trim().is_empty(), "{} port must not be empty", at());
                ensure!(
                    parse_u32_address(address).is_some(),
                    "{} address must be a hexadecimal 32-bit value",
                    at()
                );
                ensure!(!file.trim().is_empty(), "{} file must not be empty", at());
            }
            Self::FastbootAssert { variable, .. } => {
                ensure!(
                    is_fastboot_token(variable),
                    "{} variable must be a single non-empty word",
                    at()
                );
            }
            Self::FastbootFlash { partition, file } => {
                ensure!(
                    is_fastboot_token(partition),
                    "{} partition must be a single non-empty word",
                    at()
                );
                ensure!(!file.trim().is_empty(), "{} file must not be empty", at());
            }
            Self::FastbootErase { partition } => {
                ensure!(
                    is_fastboot_token(partition),
                    "{} partition must be a single non-empty word",
                    at()
                );
            }
            Self::FastbootOem { command } => {
                ensure!(
                    !command.trim().is_empty() && !command.chars().any(char::is_control),
                    "{} command must not be empty or contain control characters",
                    at()
                );
            }
            Self::Alert { message } => {
                ensure!(
                    !message.trim().is_empty() && !message.chars().any(char::is_control),
                    "{} message must not be empty or contain control characters",
                    at()
                );
            }
            Self::Sleep { millis } => {
                ensure!(
                    *millis > 0,
                    "{} duration must be at least 1 millisecond",
                    at()
                );
            }
            Self::FastbootReboot { .. } => {}
        }
        Ok(())
    }
}

fn is_fastboot_token(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}

pub fn parse_u32_address(value: &str) -> Option<u32> {
    let value = value.trim();
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedScript {
    pub name: String,
    pub steps: Vec<ResolvedStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStep {
    pub step: FlashStep,
    pub file: Option<PathBuf>,
}

impl ResolvedStep {
    pub fn kind(&self) -> &'static str {
        self.step.kind()
    }
}

impl FlashScript {
    pub fn parse(text: &str) -> Result<Self> {
        let script: Self =
            serde_json::from_str(text).context("failed to parse flash script JSON")?;
        script.validate()?;
        Ok(script)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == SCRIPT_VERSION,
            "unsupported flash script version {} (expected {SCRIPT_VERSION})",
            self.version
        );
        ensure!(
            !self.steps.is_empty(),
            "flash script must contain at least one step"
        );
        ensure!(
            self.steps.len() <= MAX_STEPS,
            "flash script exceeds the maximum of {MAX_STEPS} steps"
        );
        for (index, step) in self.steps.iter().enumerate() {
            step.validate(index)?;
        }
        Ok(())
    }

    pub fn resolve(&self, base: &Path) -> Result<ResolvedScript> {
        self.validate()?;
        let mut steps = Vec::with_capacity(self.steps.len());
        for (index, step) in self.steps.iter().enumerate() {
            let file = match step {
                FlashStep::VcomUpload { file, .. } | FlashStep::FastbootFlash { file, .. } => {
                    Some(file)
                }
                _ => None,
            }
            .map(|file| {
                let relative = Path::new(file.trim());
                let joined = if relative.is_absolute() {
                    relative.to_path_buf()
                } else {
                    base.join(relative)
                };
                let path = joined.canonicalize().with_context(|| {
                    format!(
                        "step {} ({}): file not found: {}",
                        index + 1,
                        step.kind(),
                        joined.display()
                    )
                })?;
                ensure!(
                    path.is_file(),
                    "step {} ({}): not a regular file: {}",
                    index + 1,
                    step.kind(),
                    path.display()
                );
                Ok::<PathBuf, anyhow::Error>(path)
            })
            .transpose()?;
            steps.push(ResolvedStep {
                step: step.clone(),
                file,
            });
        }
        Ok(ResolvedScript {
            name: self.name.clone(),
            steps,
        })
    }
}
