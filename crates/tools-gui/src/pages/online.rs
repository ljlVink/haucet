use crate::app::HaucetApp;
use crate::pages::{Page, run_button};
use crate::util::{human_size, kv};
use crate::worker::JobOp;
use eframe::egui;
use online_fetcher::VersionInfo;

#[derive(Default)]
pub struct OnlinePage {
    url: String,
    pending_url: Option<String>,
    info: Option<VersionInfo>,
    text: String,

    saved: Option<String>,
}

impl OnlinePage {
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        egui::ScrollArea::vertical()
            .id_salt("online-info-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.strong(tr!("online-url"));
                let input = ui.add_enabled(
                    self.pending_url.is_none(),
                    egui::TextEdit::singleline(&mut self.url)
                        .hint_text("https://.../update_full_cust.zip")
                        .desired_width(f32::INFINITY),
                );
                if input.changed() {
                    self.info = None;
                    self.text.clear();

                    self.saved = None;
                }
                let submitted =
                    input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                let ready = !app.job_running() && !self.url.trim().is_empty();
                if (run_button(ui, &tr!("online-fetch"), ready, None).clicked() || submitted)
                    && ready
                {
                    let url = self.url.trim().to_owned();
                    self.pending_url = Some(url.clone());
                    self.info = None;
                    self.text.clear();

                    self.saved = None;
                    app.start_job(JobOp::OnlineFetch { url });
                }
                ui.add_space(12.0);
                if let Some(info) = &self.info {
                    egui::Grid::new("online-info-grid")
                        .num_columns(2)
                        .spacing([18.0, 8.0])
                        .show(ui, |ui| {
                            kv(ui, &tr!("online-entry"), &info.entry_name);
                            kv(
                                ui,
                                &tr!("online-archive-size"),
                                human_size(info.archive_size),
                            );
                        });
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.strong("VERSION.mbn");
                        if ui.button(tr!("online-copy")).clicked() {
                            ui.ctx().copy_text(self.text.clone());
                        }
                    });
                    if let Some(saved) = &self.saved {
                        ui.label(saved);
                    }
                    ui.add_space(6.0);
                    // A borrowed str keeps the result selectable but read-only.
                    let mut text = self.text.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .desired_rows(12),
                    );
                }
            });
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_result(Page::Online) else {
            return;
        };
        if self.pending_url.take().as_deref() != Some(self.url.trim()) {
            return;
        }
        if !result.ok {
            app.notify_result(&result);
            return;
        }
        let info = result
            .payload
            .and_then(|payload| serde_json::from_value::<VersionInfo>(payload).ok());
        match info {
            Some(info) => {
                self.text = info.text();
                self.info = Some(info);
            }
            None => {
                app.notify(egui_notify::ToastLevel::Error, tr!("worker-result-invalid"));
            }
        }
    }
}
