use crate::app::HaucetApp;
use crate::pages::images::ImageKind;
use crate::pages::{ResultView, run_button};
use crate::util::{
    human_size, message_box, open_in_file_manager, sibling_output_path, update_derived_path,
};
use common::formats::erofs::ErofsManifest;
use eframe::egui;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErofsTab {
    #[default]
    Unpack,
    Repack,
}

#[derive(Debug)]
enum PendingOp {
    Unpack { output: String },
    Repack { output: String },
}

#[derive(Debug, Default)]
pub struct ErofsPage {
    pub tab: ErofsTab,
    pub unpack: UnpackState,
    pub repack: RepackState,
    pub result: Option<ResultView>,
    pending: Option<PendingOp>,
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
    pub output: String,
    pub allow_grow: bool,
    pub manifest: Option<ErofsManifest>,
    pub manifest_error: Option<String>,
    pub manifest_from: String,
    manifest_stamp: Option<ManifestStamp>,
    auto_output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManifestStamp {
    Missing,
    Present {
        len: u64,
        modified: Option<SystemTime>,
    },
}

impl ErofsPage {
    pub fn select_unpack_image(&mut self, image: String) {
        self.unpack.image = image;
        self.update_unpack_output();
    }

    pub fn select_workspace(&mut self, workspace: String) {
        self.repack.workspace = workspace;
        self.repack.manifest = None;
        self.repack.manifest_error = None;
        self.repack.manifest_from.clear();
        self.repack.manifest_stamp = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        self.poll_result(app);
        self.poll_manifest();

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("operation")).weak());
            ui.selectable_value(&mut self.tab, ErofsTab::Unpack, tr!("unpack-image"));
            ui.selectable_value(&mut self.tab, ErofsTab::Repack, tr!("rebuild-image"));
        });
        ui.add_space(8.0);

        match self.tab {
            ErofsTab::Unpack => self.unpack_tab(ui, app),
            ErofsTab::Repack => self.repack_tab(ui, app),
        }

        ui.add_space(10.0);
        self.show_result(ui);
    }

    fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_image_result(ImageKind::Erofs) else {
            return;
        };
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.apply_result(pending, result);
    }

    fn apply_result(&mut self, pending: PendingOp, result: crate::job::JobResult) {
        if !result.ok {
            self.result = Some(ResultView {
                ok: false,
                summary: result.summary,
                output: String::new(),
            });
            return;
        }
        self.result = Some(ResultView {
            ok: true,
            summary: result.summary,
            output: match pending {
                PendingOp::Unpack { output } | PendingOp::Repack { output } => output,
            },
        });
    }

    fn poll_manifest(&mut self) {
        let workspace = self.repack.workspace.trim().to_owned();
        if workspace.is_empty() {
            self.repack.manifest = None;
            self.repack.manifest_error = None;
            self.repack.manifest_from.clear();
            self.repack.manifest_stamp = None;
            return;
        }
        let stamp = manifest_stamp(Path::new(&workspace));
        if workspace == self.repack.manifest_from
            && self.repack.manifest_stamp.as_ref() == Some(&stamp)
        {
            return;
        }
        self.repack.manifest_from = workspace.clone();
        self.repack.manifest_stamp = Some(stamp);
        match common::formats::erofs::read_manifest(std::path::Path::new(&workspace)) {
            Ok(manifest) => {
                let next = default_repack_output(&workspace, &manifest.original_file_name);
                update_derived_path(&mut self.repack.output, &mut self.repack.auto_output, next);
                self.repack.manifest_error = None;
                self.repack.manifest = Some(manifest);
            }
            Err(error) => {
                self.repack.manifest = None;
                self.repack.manifest_error = Some(format!("{error:#}"));
            }
        }
    }

    fn unpack_tab(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("image-file")).strong());
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.unpack.image)
                    .hint_text(tr!("erofs-image-hint"))
                    .desired_width(ui.available_width() - 240.0),
            );
            if response.changed() {
                self.update_unpack_output();
            }
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) = app.pick_file(
                    &tr!("choose-erofs-image"),
                    &[(tr!("filter-image").as_str(), &["img"])],
                )
            {
                self.select_unpack_image(path.display().to_string());
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.select_unpack_image(path.display().to_string());
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("output-workspace")).strong());
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
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.unpack.force, tr!("overwrite-existing-workspace"));
        });
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
            app.start_job(crate::worker::JobOp::ErofsUnpack {
                image: self.unpack.image.trim().to_owned(),
                output,
                force: self.unpack.force,
            });
        }
    }

    fn repack_tab(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.label(egui::RichText::new(tr!("erofs-repack-help")).weak());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("workspace-directory")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.repack.workspace)
                    .hint_text(tr!("erofs-workspace-hint"))
                    .desired_width(ui.available_width() - 240.0),
            );
            if ui.button(tr!("choose-directory")).clicked()
                && let Some(dir) = app.pick_dir(&tr!("choose-erofs-workspace"))
            {
                self.select_workspace(dir.display().to_string());
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first()
            && path.is_dir()
        {
            self.select_workspace(path.display().to_string());
        }
        ui.add_space(6.0);
        if let Some(manifest) = &self.repack.manifest {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    egui::Grid::new("erofs-manifest-grid")
                        .num_columns(2)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            crate::util::kv(ui, &tr!("partition"), &manifest.partition);
                            crate::util::kv(
                                ui,
                                &tr!("original-file"),
                                &manifest.original_file_name,
                            );
                            crate::util::kv(
                                ui,
                                &tr!("original-size"),
                                human_size(manifest.original_size),
                            );
                            crate::util::kv(ui, &tr!("original-sha256"), &manifest.original_sha256);
                            crate::util::kv(
                                ui,
                                &tr!("hvb-certificate"),
                                if manifest.hvb.is_some() {
                                    tr!("preserved-not-resigned")
                                } else {
                                    tr!("none")
                                },
                            );
                        });
                });
            ui.add_space(6.0);
            message_box(
                ui,
                egui::Color32::from_rgb(230, 170, 40),
                tr!("erofs-signature-warning"),
            );
        } else if let Some(error) = &self.repack.manifest_error {
            message_box(ui, egui::Color32::from_rgb(230, 90, 90), error);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("output-image")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.repack.output)
                    .hint_text(tr!("erofs-output-hint"))
                    .desired_width(ui.available_width() - 260.0),
            );
            if ui.button(tr!("choose-save-location")).clicked()
                && let Some(path) = app.pick_save(&tr!("save-repacked-image"), "new-partition.img")
            {
                self.repack.output = path.display().to_string();
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.repack.allow_grow, tr!("allow-image-growth"));
        });
        ui.add_space(8.0);
        let ready = !app.job_running()
            && !self.repack.workspace.trim().is_empty()
            && !self.repack.output.trim().is_empty()
            && self.repack.manifest.is_some();
        let output = self.repack.output.trim().to_owned();
        if run_button(
            ui,
            &tr!("repack"),
            ready,
            Some(&tr!("valid-workspace-required")),
        )
        .clicked()
        {
            self.pending = Some(PendingOp::Repack {
                output: output.clone(),
            });
            self.result = None;
            app.start_job(crate::worker::JobOp::ErofsRepack {
                workspace: self.repack.workspace.trim().to_owned(),
                output,
                allow_grow: self.repack.allow_grow,
            });
        }
    }

    fn show_result(&self, ui: &mut egui::Ui) {
        let Some(result) = &self.result else {
            return;
        };
        ui.add_space(6.0);
        if result.ok {
            message_box(ui, egui::Color32::from_rgb(90, 200, 120), &result.summary);
            if !result.output.is_empty() && ui.button(tr!("open-output-location")).clicked() {
                open_in_file_manager(std::path::Path::new(&result.output));
            }
        } else {
            message_box(ui, egui::Color32::from_rgb(230, 90, 90), &result.summary);
        }
    }

    fn update_unpack_output(&mut self) {
        let next = sibling_output_path(&self.unpack.image, "erofs", "-work");
        update_derived_path(&mut self.unpack.output, &mut self.unpack.auto_output, next);
    }
}

fn manifest_stamp(workspace: &Path) -> ManifestStamp {
    match std::fs::metadata(workspace.join("haucet-erofs.json")) {
        Ok(metadata) => ManifestStamp::Present {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        },
        Err(_) => ManifestStamp::Missing,
    }
}

fn default_repack_output(workspace: &str, original_file_name: &str) -> String {
    let workspace = std::path::Path::new(workspace);
    let Some(parent) = workspace
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return String::new();
    };

    let original = std::path::Path::new(original_file_name);
    let Some(file_name) = original.file_name() else {
        return String::new();
    };
    let mut patched_name = original.file_stem().unwrap_or(file_name).to_os_string();
    patched_name.push("_patched");
    if let Some(extension) = original.extension() {
        patched_name.push(".");
        patched_name.push(extension);
    }

    parent.join(patched_name).display().to_string()
}
