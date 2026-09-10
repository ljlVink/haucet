use crate::app::HaucetApp;
use crate::pages::images::ImageKind;
use crate::pages::{ResultView, badge_text, run_button};
use crate::util::{human_size, open_in_file_manager, sibling_output_path, update_derived_path};
use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RamdiskTab {
    #[default]
    Unpack,
    Repack,
    Patch,
}

#[derive(Debug)]
enum PendingOp {
    Unpack { output: String },
    Repack { output: String },
    Patch { output: String },
    Probe { image: String },
}

#[derive(Debug, Default)]
pub struct RamdiskPage {
    pub tab: RamdiskTab,
    pub unpack: UnpackState,
    pub repack: RepackState,
    pub patch: PatchState,
    pub result: Option<ResultView>,
    pending: Option<PendingOp>,
    probe_requested: bool,
}

#[derive(Debug, Default)]
pub struct UnpackState {
    pub image: String,
    pub output: String,
    pub force: bool,
    auto_output: Option<String>,
}

#[derive(Debug, Default)]
pub struct RepackState {
    pub workspace: String,
    pub original: String,
    pub output: String,
}

#[derive(Debug, Default)]
pub struct PatchState {
    pub image: String,
    pub binary: String,
    pub output: String,
    pub probe: Option<ProbeInfo>,

    auto_output: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProbeInfo {
    pub patched: bool,
    pub has_init_early: bool,
    pub layout_known: bool,
    pub payload_format: String,
    pub payload_len: u64,
    pub header_size: u64,
    pub cert_original_len: u64,
}

impl RamdiskPage {
    pub fn select_unpack_image(&mut self, image: String) {
        self.unpack.image = image;
        self.update_unpack_output();
    }

    pub fn select_patch_image(&mut self, image: String) {
        self.patch.image = image;
        self.patch.probe = None;

        self.update_patch_output();
        self.probe_requested = !self.patch.image.trim().is_empty();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("operation")).weak());
            ui.selectable_value(&mut self.tab, RamdiskTab::Unpack, tr!("unpack-image"));
            ui.selectable_value(&mut self.tab, RamdiskTab::Repack, tr!("rebuild-image"));
            ui.selectable_value(&mut self.tab, RamdiskTab::Patch, "Patch init_early");
        });
        ui.add_space(8.0);

        match self.tab {
            RamdiskTab::Unpack => self.unpack_tab(ui, app),
            RamdiskTab::Repack => self.repack_tab(ui, app),
            RamdiskTab::Patch => self.patch_tab(ui, app),
        }

        ui.add_space(10.0);
        self.show_result(ui);
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_image_result(ImageKind::Ramdisk) else {
            return;
        };
        let Some(pending) = self.pending.take() else {
            return;
        };
        let output = match pending {
            PendingOp::Probe { image } => {
                if image != self.patch.image.trim() {
                    return;
                }
                if !result.ok {
                    app.notify_result(&result);
                    self.patch.probe = None;
                } else {
                    match result
                        .payload
                        .and_then(|payload| serde_json::from_value::<ProbeInfo>(payload).ok())
                    {
                        Some(probe) => {
                            self.patch.probe = Some(probe);
                        }
                        None => {
                            self.patch.probe = None;
                            app.notify(
                                egui_notify::ToastLevel::Error,
                                tr!("ramdisk-probe-invalid"),
                            );
                        }
                    }
                }
                return;
            }
            PendingOp::Unpack { output }
            | PendingOp::Repack { output }
            | PendingOp::Patch { output } => output,
        };
        app.notify_result(&result);
        self.result = Some(ResultView {
            ok: result.ok,
            summary: result.summary,
            output: if result.ok { output } else { String::new() },
        });
    }

    fn unpack_tab(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.label(egui::RichText::new(tr!("ramdisk-unpack-help")).weak());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("image-file")).strong());
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.unpack.image)
                    .hint_text(tr!("ramdisk-image-hint"))
                    .desired_width(ui.available_width() - 240.0),
            );
            if response.changed() {
                self.update_unpack_output();
            }
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) = app.pick_file(
                    &tr!("choose-ramdisk-image"),
                    &[(tr!("filter-image").as_str(), &["img"])],
                )
            {
                self.select_unpack_image(path.display().to_string());
            }
        });
        self.handle_drops(ui, app);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("output-directory")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.unpack.output)
                    .desired_width(ui.available_width() - 240.0),
            );
            if ui.button(tr!("choose-directory")).clicked()
                && let Some(dir) = app.pick_dir(&tr!("choose-output-directory"))
            {
                self.unpack.output = dir.display().to_string();
            }
        });
        ui.add_space(6.0);
        ui.checkbox(&mut self.unpack.force, tr!("overwrite-existing-directory"));
        ui.add_space(8.0);
        let ready = !app.job_running()
            && !self.unpack.image.trim().is_empty()
            && !self.unpack.output.trim().is_empty();
        let output = self.unpack.output.trim().to_owned();
        if run_button(ui, &tr!("start-unpack"), ready, None).clicked() {
            self.pending = Some(PendingOp::Unpack {
                output: output.clone(),
            });
            self.result = None;
            app.start_job(crate::worker::JobOp::RamdiskUnpack {
                image: self.unpack.image.trim().to_owned(),
                output,
                force: self.unpack.force,
            });
        }
    }

    fn repack_tab(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.label(egui::RichText::new(tr!("ramdisk-repack-help")).weak());
        ui.add_space(6.0);
        input_path_edit(
            ui,
            app,
            &tr!("workspace-directory"),
            &mut self.repack.workspace,
            true,
            &tr!("choose-directory"),
        );
        ui.add_space(6.0);
        input_path_edit(
            ui,
            app,
            &tr!("original-image"),
            &mut self.repack.original,
            false,
            &tr!("choose-file"),
        );
        ui.add_space(6.0);
        save_path_edit(
            ui,
            app,
            &tr!("output-image"),
            &mut self.repack.output,
            &tr!("choose-save-location"),
            "ramdisk-repacked.img",
        );
        ui.add_space(8.0);
        let ready = !app.job_running()
            && !self.repack.workspace.trim().is_empty()
            && !self.repack.original.trim().is_empty()
            && !self.repack.output.trim().is_empty();
        let output = self.repack.output.trim().to_owned();
        if run_button(ui, &tr!("repack"), ready, None).clicked() {
            self.pending = Some(PendingOp::Repack {
                output: output.clone(),
            });
            self.result = None;
            app.start_job(crate::worker::JobOp::RamdiskRepack {
                workspace: self.repack.workspace.trim().to_owned(),
                original: self.repack.original.trim().to_owned(),
                output,
            });
        }
    }

    fn patch_tab(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.label(egui::RichText::new(tr!("ramdisk-patch-help")).weak());
        ui.add_space(4.0);

        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("ramdisk-image")).strong());
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.patch.image)
                    .hint_text(tr!("original-ramdisk-image"))
                    .desired_width(ui.available_width() - 260.0),
            );
            if response.changed() {
                self.patch.probe = None;

                self.update_patch_output();
                self.probe_requested = std::path::Path::new(self.patch.image.trim()).is_file();
            }
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) = app.pick_file(
                    &tr!("choose-ramdisk-image"),
                    &[(tr!("filter-image").as_str(), &["img"])],
                )
            {
                self.select_patch_image(path.display().to_string());
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.select_patch_image(path.display().to_string());
        }
        self.start_probe(app);

        if let Some(probe) = &self.patch.probe {
            ui.add_space(6.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tr!("patch-status")).strong());
                        if probe.patched {
                            badge_text(
                                ui,
                                &tr!("already-patched"),
                                egui::Color32::from_rgb(230, 170, 40),
                            );
                            ui.label(egui::RichText::new(tr!("patch-again-warning")).weak());
                        } else if probe.layout_known {
                            badge_text(
                                ui,
                                &tr!("stock-image-patchable"),
                                egui::Color32::from_rgb(90, 200, 120),
                            );
                        } else {
                            badge_text(
                                ui,
                                &tr!("unknown-layout"),
                                egui::Color32::from_rgb(230, 90, 90),
                            );
                        }
                    });
                    egui::Grid::new("probe-grid")
                        .num_columns(2)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            crate::util::kv(ui, &tr!("payload-compression"), &probe.payload_format);
                            crate::util::kv(
                                ui,
                                "bin/init_early",
                                if probe.has_init_early {
                                    tr!("present")
                                } else {
                                    tr!("absent")
                                },
                            );
                            crate::util::kv(
                                ui,
                                &tr!("payload-size"),
                                human_size(probe.payload_len),
                            );
                            crate::util::kv(
                                ui,
                                &tr!("certificate-max-image"),
                                human_size(probe.cert_original_len),
                            );
                            let growth = payload_growth_space(
                                probe.cert_original_len,
                                probe.header_size,
                                probe.payload_len,
                            );
                            if growth > 0 {
                                crate::util::kv(ui, &tr!("available-growth"), human_size(growth));
                            }
                        });
                });
        }

        ui.add_space(8.0);
        input_path_edit(
            ui,
            app,
            &tr!("new-init-early-binary"),
            &mut self.patch.binary,
            false,
            &tr!("choose-file"),
        );
        ui.add_space(6.0);
        save_path_edit(
            ui,
            app,
            &tr!("output-image"),
            &mut self.patch.output,
            &tr!("choose-save-location"),
            "ramdisk-patched.img",
        );
        ui.add_space(8.0);
        let patchable = self
            .patch
            .probe
            .as_ref()
            .is_some_and(|probe| !probe.patched && probe.layout_known && probe.has_init_early);
        let ready = !app.job_running()
            && !self.patch.image.trim().is_empty()
            && !self.patch.binary.trim().is_empty()
            && !self.patch.output.trim().is_empty()
            && patchable;
        let output = self.patch.output.trim().to_owned();
        if run_button(
            ui,
            "Patch init_early",
            ready,
            Some(&tr!("ramdisk-patch-requirement")),
        )
        .clicked()
        {
            self.pending = Some(PendingOp::Patch {
                output: output.clone(),
            });
            self.result = None;
            app.start_job(crate::worker::JobOp::RamdiskPatch {
                image: self.patch.image.trim().to_owned(),
                binary: self.patch.binary.trim().to_owned(),
                output,
            });
        }
    }

    fn start_probe(&mut self, app: &mut HaucetApp) {
        if !self.probe_requested || app.job_running() {
            return;
        }
        let image = self.patch.image.trim().to_owned();
        self.probe_requested = false;
        self.pending = Some(PendingOp::Probe {
            image: image.clone(),
        });
        app.start_job(crate::worker::JobOp::RamdiskProbe { image });
    }

    fn handle_drops(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.select_unpack_image(path.display().to_string());
        }
    }

    fn show_result(&self, ui: &mut egui::Ui) {
        let Some(result) = &self.result else {
            return;
        };
        ui.add_space(6.0);
        if result.ok
            && !result.output.is_empty()
            && ui.button(tr!("open-output-location")).clicked()
        {
            open_in_file_manager(std::path::Path::new(&result.output));
        }
    }

    fn update_unpack_output(&mut self) {
        let next = sibling_output_path(&self.unpack.image, "ramdisk", "-work");
        update_derived_path(&mut self.unpack.output, &mut self.unpack.auto_output, next);
    }

    fn update_patch_output(&mut self) {
        let next = sibling_output_path(&self.patch.image, "ramdisk", "-patched.img");
        update_derived_path(&mut self.patch.output, &mut self.patch.auto_output, next);
    }
}

fn payload_growth_space(cert_limit: u64, header_size: u64, payload_size: u64) -> u64 {
    cert_limit
        .checked_sub(header_size)
        .and_then(|limit| limit.checked_sub(payload_size))
        .unwrap_or(0)
}

fn input_path_edit(
    ui: &mut egui::Ui,
    app: &mut HaucetApp,
    label: &str,
    value: &mut String,
    is_dir: bool,
    button: &str,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).strong());
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(if is_dir {
                    tr!("directory-path")
                } else {
                    tr!("file-path")
                })
                .desired_width(ui.available_width() - 220.0),
        );
        if ui.button(button).clicked() {
            let picked = if is_dir {
                app.pick_dir(label)
            } else {
                app.pick_file(label, &[])
            };
            if let Some(path) = picked {
                *value = path.display().to_string();
            }
        }
    });
}

fn save_path_edit(
    ui: &mut egui::Ui,
    app: &mut HaucetApp,
    label: &str,
    value: &mut String,
    button: &str,
    fallback_name: &str,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).strong());
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(tr!("file-path"))
                .desired_width(ui.available_width() - 220.0),
        );
        if ui.button(button).clicked() {
            let suggested = std::path::Path::new(value.trim())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| fallback_name.to_owned());
            if let Some(path) = app.pick_save(label, &suggested) {
                *value = path.display().to_string();
            }
        }
    });
}
