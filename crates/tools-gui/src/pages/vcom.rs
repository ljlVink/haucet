use crate::app::HaucetApp;
use crate::pages::{Page, ResultView, badge_text, run_button};
use crate::util::{kv, message_box, section};
use eframe::egui;
use hisi_vcom::vcom::parse_address;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VcomPortInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VcomStatusPayload {
    #[serde(default)]
    pub ports: Vec<VcomPortInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    Status,
    Flash,
}

#[derive(Debug, Default)]
pub struct VcomPage {
    pub status: Option<VcomStatusPayload>,
    pub status_error: Option<String>,
    pub auto_checked: bool,
    pub port: String,
    pub address: String,
    pub file: String,
    pub result: Option<ResultView>,
    pending: Option<PendingOp>,
}

impl VcomPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        self.poll_result(app);
        if !self.auto_checked && !app.job_running() {
            self.auto_checked = true;
            self.start_status(app);
        }

        egui::ScrollArea::vertical()
            .id_salt("vcom-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.status_section(ui, app);
                ui.add_space(12.0);
                self.flash_section(ui, app);
                ui.add_space(20.0);
            });
    }

    fn status_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("vcom-devices"));
        ui.horizontal(|ui| {
            if run_button(
                ui,
                &tr!("detect-device"),
                !app.job_running(),
                Some(&tr!("vcom-detect-hint")),
            )
            .clicked()
            {
                self.start_status(app);
            }
            if app.job_running() {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label(egui::RichText::new(tr!("detecting")).weak());
            }
        });
        ui.add_space(6.0);

        if let Some(error) = &self.status_error {
            message_box(ui, egui::Color32::from_rgb(230, 90, 90), error);
            return;
        }
        let Some(status) = &self.status else {
            ui.label(egui::RichText::new(tr!("vcom-no-scan-result")).weak());
            return;
        };

        if status.ports.is_empty() {
            message_box(
                ui,
                egui::Color32::from_rgb(230, 170, 40),
                tr!("vcom-not-found"),
            );
            return;
        }

        if !status.ports.is_empty() {
            ui.horizontal(|ui| {
                badge_text(
                    ui,
                    &tr!("serial-port-count"),
                    egui::Color32::from_rgb(90, 200, 120),
                );
                ui.label(status.ports.len().to_string());
            });
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    egui::Grid::new("vcom-port-grid")
                        .num_columns(2)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            for port in &status.ports {
                                kv(ui, &port.name, &port.description);
                            }
                        });
                });
        }
    }

    fn flash_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("page-vcom-title"));

        let choices = self
            .status
            .as_ref()
            .map(|status| status.ports.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("serial-port")).strong());
            egui::ComboBox::from_id_salt("vcom-port-select")
                .width(180.0)
                .selected_text(
                    choices
                        .iter()
                        .find(|choice| choice.name == self.port)
                        .map(|choice| choice.name.clone())
                        .unwrap_or_else(|| tr!("choose-serial-port")),
                )
                .show_ui(ui, |ui| {
                    for choice in &choices {
                        ui.selectable_value(
                            &mut self.port,
                            choice.name.clone(),
                            format!("{} - {}", choice.name, choice.description),
                        );
                    }
                });
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tr!("address")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("0x80000000")
                    .desired_width(180.0),
            );
        });
        if !self.address.trim().is_empty() && parse_address(self.address.trim()).is_err() {
            ui.label(
                egui::RichText::new(tr!("hex-address-required"))
                    .color(egui::Color32::from_rgb(230, 170, 40)),
            );
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("loader-file")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.file)
                    .hint_text(tr!("loader-file-path"))
                    .desired_width(ui.available_width() - 170.0),
            );
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) = app.pick_file(&tr!("choose-vcom-loader"), &[])
            {
                self.set_file(&path);
            }
        });
        if let Some(path) = app.take_drops(ui.ctx()).first().cloned() {
            self.set_file(&path);
        }

        ui.add_space(6.0);
        let parsed_address = parse_address(self.address.trim());
        let ready = !app.job_running()
            && choices.iter().any(|choice| choice.name == self.port)
            && !self.file.trim().is_empty()
            && parsed_address.is_ok();
        if run_button(
            ui,
            &tr!("flash-loader"),
            ready,
            Some(&tr!("flash-loader-hint")),
        )
        .clicked()
        {
            self.result = None;
            self.pending = Some(PendingOp::Flash);
            app.start_job(crate::worker::JobOp::VcomFlash {
                port: self.port.trim().to_owned(),
                address: parsed_address.expect("ready requires a valid address"),
                file: self.file.trim().to_owned(),
            });
        }
        if app.job_running() {
            ui.label(egui::RichText::new(tr!("flash-task-running")).weak());
        }

        if let Some(result) = &self.result {
            let color = if result.ok {
                egui::Color32::from_rgb(90, 200, 120)
            } else {
                egui::Color32::from_rgb(230, 90, 90)
            };
            message_box(ui, color, &result.summary);
        }
    }

    fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_result(Page::Vcom) else {
            return;
        };
        match self.pending.take().unwrap_or(PendingOp::Status) {
            PendingOp::Status => {
                if !result.ok {
                    self.status = None;
                    self.status_error = Some(result.summary);
                } else if let Some(payload) = result.payload {
                    match serde_json::from_value::<VcomStatusPayload>(payload) {
                        Ok(status) => {
                            if self.port.trim().is_empty()
                                && let Some(port) = status.ports.first()
                            {
                                self.port = port.name.clone();
                            }
                            self.status = Some(status);
                            self.status_error = None;
                        }
                        Err(error) => {
                            self.status = None;
                            self.status_error =
                                Some(tr!("vcom-status-parse-error", "error" => error.to_string()));
                        }
                    }
                }
            }
            PendingOp::Flash => {
                self.result = Some(ResultView {
                    ok: result.ok,
                    summary: result.summary,
                    output: String::new(),
                });
            }
        }
    }

    fn start_status(&mut self, app: &mut HaucetApp) {
        self.status_error = None;
        self.pending = Some(PendingOp::Status);
        app.start_job(crate::worker::JobOp::VcomStatus {});
    }

    fn set_file(&mut self, path: &Path) {
        self.file = path.display().to_string();
    }
}
