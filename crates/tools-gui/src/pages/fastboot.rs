use crate::app::HaucetApp;
use crate::fastboot_memory::MemoryMap;
use crate::pages::{Page, ResultView, run_button};
use crate::util::{human_size, kv, message_box, section};
use eframe::egui;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FastbootDeviceInfo {
    pub bus: String,
    pub addr: u8,
    pub vid: String,
    pub pid: String,
    pub product: String,
    pub serial: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FastbootStatusPayload {
    pub connected: bool,
    #[serde(default)]
    pub devices: Vec<FastbootDeviceInfo>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    Status,
    Reboot,
    Extract,
    Flash,
    MemoryList,
    UploadMemory,
}

#[derive(Debug, Default)]
pub struct FastbootPage {
    pub status: Option<FastbootStatusPayload>,
    pub status_error: Option<String>,
    pub auto_checked: bool,
    pub image: String,
    pub target: String,
    pub extract_partition: String,
    pub extract_result: Option<ResultView>,
    pub result: Option<ResultView>,
    pub reboot_result: Option<ResultView>,
    memory_map: Option<MemoryMap>,
    selected_memory: Option<usize>,
    memory_result: Option<ResultView>,
    pending: Option<PendingOp>,
}

impl FastbootPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        self.poll_result(app);
        if !self.auto_checked && !app.job_running() {
            self.auto_checked = true;
            self.start_status(app);
        }

        egui::ScrollArea::vertical()
            .id_salt("fastboot-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.status_section(ui, app);
                ui.add_space(10.0);
                self.extract_section(ui, app);
                ui.add_space(10.0);
                self.memory_section(ui, app);
                ui.add_space(10.0);
                self.flash_section(ui, app);
                ui.add_space(20.0);
            });
    }

    fn status_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("fastboot-devices"));
        let reboot_ready = !app.job_running()
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.connected && status.devices.len() == 1);
        ui.horizontal(|ui| {
            if run_button(
                ui,
                &tr!("detect-device"),
                !app.job_running(),
                Some(&tr!("fastboot-detect-hint")),
            )
            .clicked()
            {
                self.start_status(app);
            }
            if run_button(
                ui,
                &tr!("reboot-device"),
                reboot_ready,
                Some(&tr!("fastboot-reboot-hint")),
            )
            .clicked()
            {
                self.start_reboot(app);
            }
            if app.job_running() {
                ui.add(egui::Spinner::new().size(16.0));
                let text = match self.pending {
                    Some(PendingOp::Status) => tr!("detecting"),
                    Some(PendingOp::Reboot) => tr!("rebooting"),
                    _ => tr!("task-running"),
                };
                ui.label(egui::RichText::new(text).weak());
            }
        });
        ui.add_space(6.0);

        if let Some(result) = &self.reboot_result {
            let color = if result.ok {
                egui::Color32::from_rgb(90, 200, 120)
            } else {
                egui::Color32::from_rgb(230, 90, 90)
            };
            message_box(ui, color, &result.summary);
            ui.add_space(6.0);
        }

        if let Some(error) = &self.status_error {
            message_box(ui, egui::Color32::from_rgb(230, 90, 90), error);
            return;
        }
        let Some(status) = &self.status else {
            ui.label(egui::RichText::new(tr!("fastboot-not-checked")).weak());
            return;
        };
        if !status.connected {
            let message = match status.devices.len() {
                0 => tr!("fastboot-not-found"),
                1 => tr!("fastboot-cannot-open"),
                _ => {
                    tr!("fastboot-multiple")
                }
            };
            message_box(ui, egui::Color32::from_rgb(230, 170, 40), message);
            return;
        }

        ui.horizontal(|ui| {
            crate::pages::badge_text(ui, &tr!("connected"), egui::Color32::from_rgb(90, 200, 120));
            if let Some(device) = status.devices.first() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} ({}:{})",
                        device.product, device.bus, device.addr
                    ))
                    .strong(),
                );
            }
        });
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                if let Some(device) = status.devices.first() {
                    egui::Grid::new("fastboot-device-grid")
                        .num_columns(2)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            kv(ui, &tr!("product"), &device.product);
                            kv(ui, &tr!("serial-number"), &device.serial);
                            kv(
                                ui,
                                &tr!("usb-address"),
                                format!("{}:{}", device.bus, device.addr),
                            );
                            kv(ui, "VID:PID", format!("{}:{}", device.vid, device.pid));
                        });
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                }
                if status.vars.is_empty() {
                    ui.label(egui::RichText::new(tr!("fastboot-no-vars")).weak());
                } else {
                    egui::Grid::new("fastboot-vars-grid")
                        .num_columns(2)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            for (key, value) in &status.vars {
                                kv(ui, key, value);
                            }
                        });
                }
            });
    }

    fn extract_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("extract-partition"));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("partition-name")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.extract_partition)
                    .hint_text(tr!("extract-partition-name-hint"))
                    .desired_width(ui.available_width() - 170.0),
            );
        });
        ui.add_space(6.0);

        let ready = !app.job_running()
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.connected && status.devices.len() == 1)
            && !self.extract_partition.trim().is_empty();
        if run_button(
            ui,
            &tr!("extract-partition"),
            ready,
            Some(&tr!("extract-partition-hint")),
        )
        .clicked()
        {
            let partition = self.extract_partition.trim().to_owned();
            let suggested = format!("{partition}.img");
            if let Some(output) = app.pick_save(&tr!("choose-extracted-image"), &suggested) {
                self.extract_result = None;
                self.pending = Some(PendingOp::Extract);
                app.start_job(crate::worker::JobOp::FastbootExtract {
                    partition,
                    output: output.display().to_string(),
                });
            }
        }
        if app.job_running() && self.pending == Some(PendingOp::Extract) {
            ui.label(egui::RichText::new(tr!("task-running")).weak());
        }

        ui.add_space(10.0);
        if let Some(result) = &self.extract_result {
            let color = if result.ok {
                egui::Color32::from_rgb(90, 200, 120)
            } else {
                egui::Color32::from_rgb(230, 90, 90)
            };
            message_box(ui, color, &result.summary);
        }
    }

    fn memory_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("fastboot-memory-title"));
        let ready = !app.job_running()
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.connected && status.devices.len() == 1);
        ui.horizontal(|ui| {
            if run_button(ui, &tr!("fastboot-memory-get-list"), ready, None).clicked() {
                self.clear_memory();
                self.pending = Some(PendingOp::MemoryList);
                app.start_job(crate::worker::JobOp::FastbootMemoryList {});
            }
            let selected = self.memory_map.as_ref().and_then(|map| {
                map.regions
                    .get(self.selected_memory?)
                    .map(|region| (map.device.clone(), region.clone()))
            });
            if run_button(
                ui,
                &tr!("fastboot-memory-download"),
                ready && selected.is_some(),
                None,
            )
            .clicked()
                && let Some((device, region)) = selected
                && let Some(output) =
                    app.pick_save(&tr!("fastboot-memory-save"), &region.suggested_filename())
            {
                self.memory_result = None;
                self.pending = Some(PendingOp::UploadMemory);
                app.start_job(crate::worker::JobOp::FastbootUploadMemory {
                    device,
                    region,
                    output: output.display().to_string(),
                });
            }
        });
        ui.add_space(6.0);

        if let Some(map) = &self.memory_map {
            egui::ScrollArea::both()
                .id_salt("fastboot-memory-list")
                .max_height(280.0)
                .show(ui, |ui| {
                    egui::Grid::new("fastboot-memory-grid")
                        .num_columns(3)
                        .striped(true)
                        .spacing([20.0, 6.0])
                        .show(ui, |ui| {
                            ui.strong(tr!("name"));
                            ui.strong(tr!("fastboot-memory-base"));
                            ui.strong(tr!("fastboot-memory-size"));
                            ui.end_row();
                            for (index, region) in map.regions.iter().enumerate() {
                                if ui
                                    .add_enabled(
                                        !app.job_running(),
                                        egui::Button::selectable(
                                            self.selected_memory == Some(index),
                                            &region.name,
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.selected_memory = Some(index);
                                }
                                ui.monospace(format!("0x{:016X}", region.base));
                                ui.monospace(format!(
                                    "0x{:08X} ({})",
                                    region.size,
                                    human_size(u64::from(region.size)),
                                ))
                                .on_hover_text(format!("{} B", region.size));
                                ui.end_row();
                            }
                        });
                });
        } else if self.memory_result.is_none() {
            ui.label(egui::RichText::new(tr!("fastboot-memory-not-loaded")).weak());
        }
        if let Some(result) = &self.memory_result {
            let color = if result.ok {
                egui::Color32::from_rgb(90, 200, 120)
            } else {
                egui::Color32::from_rgb(230, 90, 90)
            };
            message_box(ui, color, &result.summary);
        }
    }

    fn flash_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("flash-image"));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("image-file")).strong());
            let image_response = ui.add(
                egui::TextEdit::singleline(&mut self.image)
                    .hint_text(tr!("image-path-drop-hint"))
                    .desired_width(ui.available_width() - 170.0),
            );
            if image_response.changed()
                && let Some(target) = partition_name_from_image(Path::new(self.image.trim()))
            {
                self.target = target;
            }
            if ui.button(tr!("choose-image")).clicked()
                && let Some(path) = app.pick_file(
                    &tr!("choose-image-file"),
                    &[(tr!("filter-image-file").as_str(), &["img", "bin"])],
                )
            {
                self.set_image(&path);
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.set_image(path);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("target-partition")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.target)
                    .hint_text(tr!("target-partition-hint"))
                    .desired_width(ui.available_width() - 170.0),
            );
        });

        ui.label(
            egui::RichText::new(tr!("flash-data-warning"))
                .color(egui::Color32::from_rgb(230, 170, 40)),
        );
        ui.add_space(6.0);
        let ready = !app.job_running()
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.connected && status.devices.len() == 1)
            && !self.image.trim().is_empty()
            && !self.target.trim().is_empty();
        if run_button(
            ui,
            &tr!("flash-image"),
            ready,
            Some(&tr!("flash-image-hint")),
        )
        .clicked()
        {
            self.result = None;
            self.pending = Some(PendingOp::Flash);
            app.start_job(crate::worker::JobOp::FastbootFlash {
                image: self.image.trim().to_owned(),
                target: self.target.trim().to_owned(),
            });
        }
        if app.job_running() {
            ui.label(egui::RichText::new(tr!("task-running")).weak());
        }

        ui.add_space(10.0);
        if let Some(result) = &self.result {
            if result.ok {
                message_box(ui, egui::Color32::from_rgb(90, 200, 120), &result.summary);
            } else {
                message_box(ui, egui::Color32::from_rgb(230, 90, 90), &result.summary);
            }
        }
    }

    fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_result(Page::Fastboot) else {
            return;
        };
        let op = self.pending.take().unwrap_or(PendingOp::Status);
        match op {
            PendingOp::Status => {
                self.result = None;
                if !result.ok {
                    self.status = None;
                    self.status_error = Some(result.summary);
                } else if let Some(payload) = result.payload {
                    match serde_json::from_value::<FastbootStatusPayload>(payload) {
                        Ok(status) => {
                            self.status = Some(status);
                            self.status_error = None;
                        }
                        Err(error) => {
                            self.status = None;
                            self.status_error =
                                Some(tr!("status-parse-error", "error" => error.to_string()));
                        }
                    }
                }
            }
            PendingOp::Reboot => {
                self.reboot_result = Some(ResultView {
                    ok: result.ok,
                    summary: result.summary,
                    output: String::new(),
                });
                if result.ok {
                    self.status = None;
                    self.status_error = None;
                }
            }
            PendingOp::Extract => {
                self.extract_result = Some(ResultView {
                    ok: result.ok,
                    summary: result.summary,
                    output: String::new(),
                });
            }
            PendingOp::Flash => {
                self.result = Some(ResultView {
                    ok: result.ok,
                    summary: result.summary,
                    output: String::new(),
                });
            }
            PendingOp::MemoryList => {
                self.accept_memory_list(result);
            }
            PendingOp::UploadMemory => {
                if !result.ok {
                    self.memory_map = None;
                    self.selected_memory = None;
                }
                self.memory_result = Some(ResultView {
                    ok: result.ok,
                    summary: result.summary,
                    output: String::new(),
                });
            }
        }
    }

    fn accept_memory_list(&mut self, result: crate::job::JobResult) {
        self.clear_memory();
        let mut view = ResultView {
            ok: result.ok,
            summary: result.summary,
            output: String::new(),
        };
        if result.ok {
            match serde_json::from_value::<MemoryMap>(result.payload.unwrap_or_default()) {
                Ok(map) if !map.regions.is_empty() => {
                    self.selected_memory = Some(0);
                    self.memory_map = Some(map);
                }
                Ok(_) => {
                    view.ok = false;
                    view.summary = tr!("fastboot-memory-empty");
                }
                Err(error) => {
                    view.ok = false;
                    view.summary =
                        tr!("fastboot-memory-payload-error", "error" => error.to_string());
                }
            }
        }
        self.memory_result = Some(view);
    }

    fn clear_memory(&mut self) {
        self.memory_map = None;
        self.selected_memory = None;
        self.memory_result = None;
    }

    fn start_status(&mut self, app: &mut HaucetApp) {
        self.clear_memory();
        self.status_error = None;
        self.reboot_result = None;
        self.pending = Some(PendingOp::Status);
        app.start_job(crate::worker::JobOp::FastbootStatus {});
    }

    fn start_reboot(&mut self, app: &mut HaucetApp) {
        self.clear_memory();
        self.reboot_result = None;
        self.pending = Some(PendingOp::Reboot);
        app.start_job(crate::worker::JobOp::FastbootReboot {});
    }

    fn set_image(&mut self, path: &Path) {
        self.image = path.display().to_string();
        if let Some(target) = partition_name_from_image(path) {
            self.target = target;
        }
    }
}

fn partition_name_from_image(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?.trim();
    if file_name.eq_ignore_ascii_case("ptable") {
        return Some("ptable".to_owned());
    }

    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("img") && !extension.eq_ignore_ascii_case("bin") {
        return None;
    }

    let stem = path.file_stem()?.to_str()?.trim();
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}
