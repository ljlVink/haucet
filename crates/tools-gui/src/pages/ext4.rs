use crate::app::HaucetApp;
use crate::pages::images::ImageKind;
use crate::pages::{ResultView, run_button};
use crate::util::{open_in_file_manager, sibling_output_path, update_derived_path};
use eframe::egui;

#[derive(Debug, Default)]
pub struct Ext4Page {
    pub image: String,
    pub output: String,
    pub force: bool,
    pub result: Option<ResultView>,
    auto_output: Option<String>,
    pending_output: Option<String>,
}

impl Ext4Page {
    pub fn select_image(&mut self, image: String) {
        self.image = image;
        self.update_output();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tr!("ext4-unpack-help")).weak());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("image-file")).strong());
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.image)
                    .hint_text(tr!("ext4-image-hint"))
                    .desired_width(ui.available_width() - 240.0),
            );
            if response.changed() {
                self.update_output();
            }
            if ui.button(tr!("choose-file")).clicked()
                && let Some(path) = app.pick_file(
                    &tr!("choose-ext4-image"),
                    &[(tr!("filter-image").as_str(), &["img", "bin"])],
                )
            {
                self.select_image(path.display().to_string());
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.select_image(path.display().to_string());
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("output-directory")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.output)
                    .desired_width(ui.available_width() - 240.0),
            );
            if ui.button(tr!("choose-directory")).clicked()
                && let Some(dir) = app.pick_dir(&tr!("choose-output-directory"))
            {
                self.output = dir.display().to_string();
            }
        });
        ui.add_space(6.0);
        ui.checkbox(&mut self.force, tr!("overwrite-existing-directory"));
        ui.add_space(8.0);

        let ready =
            !app.job_running() && !self.image.trim().is_empty() && !self.output.trim().is_empty();
        let output = self.output.trim().to_owned();
        if run_button(ui, &tr!("start-unpack"), ready, None).clicked() {
            self.pending_output = Some(output.clone());
            self.result = None;
            app.start_job(crate::worker::JobOp::Ext4Unpack {
                image: self.image.trim().to_owned(),
                output,
                force: self.force,
            });
        }

        self.show_result(ui);
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_image_result(ImageKind::Ext4) else {
            return;
        };
        let Some(output) = self.pending_output.take() else {
            return;
        };
        app.notify_result(&result);
        self.result = Some(ResultView {
            ok: result.ok,
            summary: result.summary,
            output: if result.ok { output } else { String::new() },
        });
    }

    fn show_result(&self, ui: &mut egui::Ui) {
        let Some(result) = &self.result else {
            return;
        };
        ui.add_space(10.0);
        if result.ok
            && !result.output.is_empty()
            && ui.button(tr!("open-output-location")).clicked()
        {
            open_in_file_manager(std::path::Path::new(&result.output));
        }
    }

    fn update_output(&mut self) {
        let next = sibling_output_path(&self.image, "ext4", "-root");
        update_derived_path(&mut self.output, &mut self.auto_output, next);
    }
}
