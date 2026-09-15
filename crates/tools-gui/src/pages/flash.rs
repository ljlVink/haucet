use crate::app::HaucetApp;
use crate::pages::{Page, run_button};
use crate::worker::JobOp;
use eframe::egui;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    Validate,
    Run,
}

#[derive(Debug, Default)]
pub struct FlashPage {
    pub script: String,
    validated: bool,
    step_count: usize,
    confirm_open: bool,
    selected_port: String,
    run_requested: bool,
    pending: Option<PendingOp>,
}

impl FlashPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        egui::ScrollArea::vertical()
            .id_salt("flash-script-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.script_section(ui, app);
                ui.add_space(20.0);
            });
        if self.confirm_open {
            self.confirm_modal(ui, app);
        }
        if app.prompt.is_some() {
            self.prompt_modal(ui, app);
        }
    }

    fn script_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        let mut script_edited = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("flash-script-file")).strong());
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.script)
                    .hint_text("haucet-flash.json")
                    .desired_width(ui.available_width() - 170.0),
            );
            script_edited = edit.changed();
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) =
                    app.pick_file(&tr!("choose-flash-script"), &[("JSON", &["json"])])
            {
                self.set_script(&path, app);
            }
            ui.hyperlink_to(
                "?",
                format!(
                    "{}/blob/main/docs/fastboot_script.md",
                    common::version::REPOSITORY_URL
                ),
            )
            .on_hover_text(tr!("flash-help-hint"));
        });
        if script_edited {
            self.validated = false;
            self.run_requested = false;
        }
        if let Some(path) = app.take_drops(ui.ctx()).first().cloned() {
            self.set_script(&path, app);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let path_ready = !self.script.trim().is_empty();
            let run_ready = !app.job_running() && path_ready;
            if run_button(
                ui,
                &tr!("flash-script-run"),
                run_ready,
                Some(&tr!("flash-script-run-hint")),
            )
            .clicked()
                && run_ready
            {
                if self.validated {
                    self.confirm_open = true;
                } else {
                    // Run always validates first.
                    self.run_requested = true;
                    self.pending = Some(PendingOp::Validate);
                    app.start_job(JobOp::FlashScriptValidate {
                        path: self.script.trim().to_owned(),
                    });
                }
            }
            if app.job_running() {
                ui.add(egui::Spinner::new().size(16.0));
                if let Some(progress) = &app.job_progress {
                    let fraction = if progress.total > 0 {
                        (progress.step as f32 + 1.0) / progress.total as f32
                    } else {
                        0.0
                    };
                    ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).show_percentage());
                    ui.label(
                        egui::RichText::new(tr!(
                            "flash-progress",
                            "step" => progress.step + 1,
                            "total" => progress.total,
                            "label" => progress.label.clone(),
                        ))
                        .weak(),
                    );
                }
            }
        });
    }

    fn prompt_modal(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        let prompt = app.prompt.clone().expect("checked by caller");
        let select_port = prompt.kind == "select_port";
        if select_port
            && self.selected_port.trim().is_empty()
            && let Some(first) = prompt.options.first()
        {
            self.selected_port = first.name.clone();
        }
        let mut answered = false;
        let mut cancelled = false;
        egui::Modal::new(egui::Id::new("flash-prompt"))
            .frame(egui::Frame::popup(ui.style()).inner_margin(20))
            .show(ui.ctx(), |ui| {
                ui.set_width(460.0);
                ui.heading(if select_port {
                    tr!("flash-select-port")
                } else {
                    tr!("flash-prompt-title")
                });
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
                if select_port {
                    let choices = &prompt.options;
                    egui::ComboBox::from_id_salt("flash-prompt-port")
                        .width(320.0)
                        .selected_text(
                            choices
                                .iter()
                                .find(|choice| choice.name == self.selected_port)
                                .map(|choice| format!("{} - {}", choice.name, choice.description))
                                .unwrap_or_else(|| tr!("choose-serial-port")),
                        )
                        .show_ui(ui, |ui| {
                            for choice in choices {
                                ui.selectable_value(
                                    &mut self.selected_port,
                                    choice.name.clone(),
                                    format!("{} - {}", choice.name, choice.description),
                                );
                            }
                        });
                } else {
                    ui.label(egui::RichText::new(&prompt.message).strong());
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                    let confirm_ready = !select_port || !self.selected_port.trim().is_empty();
                    if ui
                        .add_enabled(
                            confirm_ready,
                            egui::Button::new(egui::RichText::new(tr!("prompt-confirm")).strong())
                                .min_size(egui::vec2(width, 34.0)),
                        )
                        .on_disabled_hover_text(tr!("choose-serial-port"))
                        .clicked()
                    {
                        answered = true;
                    }
                    if ui
                        .add_sized([width, 34.0], egui::Button::new(tr!("prompt-cancel-job")))
                        .clicked()
                    {
                        cancelled = true;
                    }
                });
            });
        if answered {
            let choice = select_port.then(|| self.selected_port.trim().to_owned());
            app.answer_prompt(choice);
        }
        if cancelled {
            app.answer_prompt(None);
            app.cancel_job();
        }
    }

    fn confirm_modal(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        let step_count = self.step_count;
        let response = egui::Modal::new(egui::Id::new("flash-run-confirm"))
            .frame(egui::Frame::popup(ui.style()).inner_margin(20))
            .show(ui.ctx(), |ui| {
                ui.set_width(460.0);
                ui.heading(tr!("flash-run-confirm-title"));
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(tr!(
                        "flash-run-confirm-body",
                        "count" => step_count,
                    ))
                    .color(ui.visuals().warn_fg_color),
                );
                ui.add_space(10.0);
                ui.label(tr!("flash-run-confirm-risk"));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                    if ui
                        .add_sized(
                            [width, 34.0],
                            egui::Button::new(
                                egui::RichText::new(tr!("flash-run-confirm-yes")).strong(),
                            ),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.pending = Some(PendingOp::Run);
                        app.start_job(JobOp::FlashScriptRun {
                            path: self.script.trim().to_owned(),
                        });
                    }
                    if ui
                        .add_sized(
                            [width, 34.0],
                            egui::Button::new(tr!("flash-run-confirm-no")),
                        )
                        .clicked()
                    {
                        ui.close();
                    }
                });
            });
        if response.should_close() {
            self.confirm_open = false;
        }
    }

    fn set_script(&mut self, path: &Path, app: &mut HaucetApp) {
        self.script = path.display().to_string();
        self.validated = false;
        if !app.job_running() {
            self.pending = Some(PendingOp::Validate);
            app.start_job(JobOp::FlashScriptValidate {
                path: self.script.clone(),
            });
        }
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_result(Page::Flash) else {
            return;
        };
        match self.pending.take().unwrap_or(PendingOp::Validate) {
            PendingOp::Validate => {
                if !result.ok {
                    self.run_requested = false;
                    self.validated = false;
                    app.notify_result(&result);
                } else {
                    self.validated = true;
                    self.step_count = result
                        .payload
                        .as_ref()
                        .and_then(|payload| payload.get("steps"))
                        .and_then(|steps| steps.as_array())
                        .map_or(0, Vec::len);
                    if self.run_requested {
                        self.run_requested = false;
                        self.confirm_open = true;
                    }
                }
            }
            PendingOp::Run => {
                app.notify_result(&result);
            }
        }
    }
}
