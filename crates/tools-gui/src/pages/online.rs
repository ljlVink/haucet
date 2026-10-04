use crate::app::HaucetApp;
use crate::pages::{Page, run_button};
use crate::util::{human_size, kv, section};
use crate::worker::JobOp;
use eframe::egui;
use online_fetcher::VersionInfo;
use online_fetcher::api::{
    FirmwareAction, FirmwareRequest, FirmwareRequests, NewFirmwareRequest, RequestsQuery,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingOp {
    Zip(String),
    List(AppliedQuery),
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CatalogFilter {
    #[default]
    All,
    Upgrade,
    Recovery,
}

impl CatalogFilter {
    fn label(self) -> String {
        match self {
            Self::All => tr!("online-action-all"),
            Self::Upgrade => tr!("online-action-upgrade"),
            Self::Recovery => tr!("online-action-recovery"),
        }
    }

    fn action(self) -> Option<FirmwareAction> {
        match self {
            Self::All => None,
            Self::Upgrade => Some(FirmwareAction::Upgrade),
            Self::Recovery => Some(FirmwareAction::Recovery),
        }
    }
}

fn action_label(action: FirmwareAction) -> String {
    match action {
        FirmwareAction::Upgrade => tr!("online-action-upgrade"),
        FirmwareAction::Recovery => tr!("online-action-recovery"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedQuery {
    q: Option<String>,
    action: Option<FirmwareAction>,
}

#[derive(Debug, Clone)]
struct SubmitForm {
    dev: String,
    base: String,
    cust: String,
    preload: String,
    patch: String,
    vendor: String,
    action: FirmwareAction,
}

impl Default for SubmitForm {
    fn default() -> Self {
        Self {
            dev: String::new(),
            base: String::new(),
            cust: String::new(),
            preload: String::new(),
            patch: String::new(),
            vendor: String::new(),
            action: FirmwareAction::Upgrade,
        }
    }
}

impl SubmitForm {
    fn complete(&self) -> bool {
        [&self.dev, &self.base, &self.cust, &self.preload]
            .into_iter()
            .all(|field| !field.trim().is_empty())
    }

    fn request(&self) -> NewFirmwareRequest {
        NewFirmwareRequest {
            dev: self.dev.trim().to_owned(),
            base: self.base.trim().to_owned(),
            cust: self.cust.trim().to_owned(),
            preload: self.preload.trim().to_owned(),
            patch: non_empty_trimmed(&self.patch),
            vendor: non_empty_trimmed(&self.vendor),
            upgrade_or_erec: self.action,
        }
    }
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn total_pages(catalog: &FirmwareRequests) -> u64 {
    if catalog.per_page == 0 {
        1
    } else {
        catalog.total.div_ceil(catalog.per_page).max(1)
    }
}

fn record_heading(record: &FirmwareRequest) -> String {
    format!("{} · {}", record.dev, record.target_bl_version_number)
}

fn show_request(ui: &mut egui::Ui, record: &FirmwareRequest, grid_id: impl std::hash::Hash) {
    egui::Grid::new(grid_id)
        .num_columns(2)
        .spacing([18.0, 8.0])
        .show(ui, |ui| {
            kv(ui, &tr!("online-device"), &record.dev);
            kv(
                ui,
                &tr!("online-device-name"),
                record.device_name.as_deref().unwrap_or("-"),
            );
            kv(ui, &tr!("online-vendor"), &record.vendor);
            kv(
                ui,
                &tr!("online-action"),
                action_label(record.upgrade_or_erec),
            );
            kv(
                ui,
                &tr!("online-target-version"),
                &record.target_bl_version_number,
            );
            kv(ui, &tr!("online-created-at"), &record.created_at);
        });
    ui.add_space(6.0);
    ui.strong(tr!("online-packages"));
    for package in &record.packages {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&package.version_number).monospace());
            if ui.button(tr!("online-copy-link")).clicked() {
                ui.ctx().copy_text(package.url.clone());
            }
            ui.hyperlink_to("↗", &package.url);
        });
    }
}

#[derive(Default)]
pub struct OnlinePage {
    url: String,
    info: Option<VersionInfo>,
    text: String,
    saved: Option<String>,

    keyword: String,
    filter: CatalogFilter,
    applied: Option<AppliedQuery>,
    catalog: Option<FirmwareRequests>,
    initial_query_started: bool,

    form: SubmitForm,
    created: Option<FirmwareRequest>,

    pending: Option<PendingOp>,
}

impl OnlinePage {
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        if !self.initial_query_started && !app.job_running() {
            self.initial_query_started = true;
            self.start_list(
                app,
                AppliedQuery {
                    q: None,
                    action: None,
                },
                None,
            );
        }
        egui::ScrollArea::vertical()
            .id_salt("online-info-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.zip_section(ui, app);
                ui.add_space(14.0);
                self.catalog_section(ui, app);
                ui.add_space(14.0);
                self.submit_section(ui, app);
            });
    }

    fn zip_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("online-url"));
        let input = ui.add_enabled(
            !matches!(self.pending, Some(PendingOp::Zip(_))),
            egui::TextEdit::singleline(&mut self.url)
                .hint_text("https://.../update_full_cust.zip")
                .desired_width(f32::INFINITY),
        );
        if input.changed() {
            self.info = None;
            self.text.clear();
            self.saved = None;
        }
        let submitted = input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        ui.add_space(8.0);
        let ready = !app.job_running() && !self.url.trim().is_empty();
        if (run_button(ui, &tr!("online-fetch"), ready, None).clicked() || submitted) && ready {
            let url = self.url.trim().to_owned();
            self.pending = Some(PendingOp::Zip(url.clone()));
            self.info = None;
            self.text.clear();
            self.saved = None;
            app.start_job(JobOp::OnlineFetch { url });
        }
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
    }

    fn start_list(&mut self, app: &mut HaucetApp, applied: AppliedQuery, page: Option<u64>) {
        let query = RequestsQuery {
            q: applied.q.clone(),
            action: applied.action,
            page,
        };
        self.pending = Some(PendingOp::List(applied));
        app.start_job(JobOp::OnlineList { query });
    }

    fn catalog_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("online-catalog"));
        let list_pending = matches!(self.pending, Some(PendingOp::List(_)));
        let ready = !app.job_running();
        ui.horizontal(|ui| {
            ui.label(tr!("online-keyword"));
            let keyword = ui.add_enabled(
                !list_pending,
                egui::TextEdit::singleline(&mut self.keyword).desired_width(220.0),
            );
            let submitted =
                keyword.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            ui.add_enabled_ui(!list_pending, |ui| {
                egui::ComboBox::from_id_salt("online-action-filter")
                    .selected_text(self.filter.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for filter in [
                            CatalogFilter::All,
                            CatalogFilter::Upgrade,
                            CatalogFilter::Recovery,
                        ] {
                            ui.selectable_value(&mut self.filter, filter, filter.label());
                        }
                    });
            });
            if (run_button(ui, &tr!("online-search"), ready, None).clicked() || submitted) && ready
            {
                let applied = AppliedQuery {
                    q: non_empty_trimmed(&self.keyword),
                    action: self.filter.action(),
                };
                self.start_list(app, applied, Some(1));
            }
        });
        if list_pending {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label(tr!("task-running"));
            });
        }
        let pager = self
            .catalog
            .as_ref()
            .map(|catalog| (catalog.page, total_pages(catalog)));
        if let Some((page, pages)) = pager {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let prev_ready = !app.job_running() && page > 1;
                if run_button(ui, &tr!("online-prev"), prev_ready, None).clicked()
                    && prev_ready
                    && let Some(applied) = self.applied.clone()
                {
                    self.start_list(app, applied, Some(page - 1));
                }
                ui.label(tr!("online-page-summary", "page" => page, "total" => pages));
                let next_ready = !app.job_running() && page < pages;
                if run_button(ui, &tr!("online-next"), next_ready, None).clicked()
                    && next_ready
                    && let Some(applied) = self.applied.clone()
                {
                    self.start_list(app, applied, Some(page + 1));
                }
            });
            ui.add_space(8.0);
        }
        if let Some(catalog) = &self.catalog {
            if catalog.requests.is_empty() {
                ui.label(egui::RichText::new(tr!("online-empty")).weak());
            } else {
                for record in &catalog.requests {
                    egui::CollapsingHeader::new(record_heading(record))
                        .id_salt(("online-request", record.id))
                        .show(ui, |ui| {
                            show_request(ui, record, ("online-record", record.id))
                        });
                }
            }
        }
    }

    fn submit_section(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        section(ui, &tr!("online-submit-title"));
        let submit_pending = matches!(self.pending, Some(PendingOp::Submit));
        ui.add_enabled_ui(!submit_pending, |ui| {
            egui::Grid::new("online-submit-grid")
                .num_columns(2)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    ui.label(tr!("online-device"));
                    ui.add(egui::TextEdit::singleline(&mut self.form.dev).desired_width(260.0));
                    ui.end_row();
                    ui.label(tr!("online-base"));
                    ui.add(egui::TextEdit::singleline(&mut self.form.base).desired_width(260.0));
                    ui.end_row();
                    ui.label(tr!("online-cust"));
                    ui.add(egui::TextEdit::singleline(&mut self.form.cust).desired_width(260.0));
                    ui.end_row();
                    ui.label(tr!("online-preload"));
                    ui.add(egui::TextEdit::singleline(&mut self.form.preload).desired_width(260.0));
                    ui.end_row();
                    ui.label(tr!("online-patch"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.patch)
                            .hint_text("patch00")
                            .desired_width(260.0),
                    );
                    ui.end_row();
                    ui.label(tr!("online-vendor"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.vendor)
                            .hint_text("all_cn")
                            .desired_width(260.0),
                    );
                    ui.end_row();
                    ui.label(tr!("online-action"));
                    egui::ComboBox::from_id_salt("online-submit-action")
                        .selected_text(action_label(self.form.action))
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for action in [FirmwareAction::Upgrade, FirmwareAction::Recovery] {
                                ui.selectable_value(
                                    &mut self.form.action,
                                    action,
                                    action_label(action),
                                );
                            }
                        });
                    ui.end_row();
                });
        });
        ui.add_space(8.0);
        let ready = !app.job_running() && self.form.complete();
        if run_button(ui, &tr!("online-submit"), ready, None).clicked() && ready {
            let request = self.form.request();
            self.pending = Some(PendingOp::Submit);
            app.start_job(JobOp::OnlineSubmit { request });
        }
        if submit_pending {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(16.0));
                ui.label(tr!("task-running"));
            });
        }
        if let Some(created) = &self.created {
            ui.add_space(8.0);
            egui::CollapsingHeader::new(record_heading(created))
                .id_salt(("online-created", created.id))
                .default_open(true)
                .show(ui, |ui| {
                    show_request(ui, created, ("online-created-record", created.id))
                });
        }
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_result(Page::Online) else {
            return;
        };
        let Some(op) = self.pending.take() else {
            return;
        };
        match op {
            PendingOp::Zip(url) => {
                if url != self.url.trim() {
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
            PendingOp::List(applied) => {
                if !result.ok {
                    app.notify_result(&result);
                    return;
                }
                let list = result
                    .payload
                    .and_then(|payload| serde_json::from_value::<FirmwareRequests>(payload).ok());
                match list {
                    Some(list) => {
                        self.applied = Some(applied);
                        self.catalog = Some(list);
                    }
                    None => {
                        app.notify(egui_notify::ToastLevel::Error, tr!("worker-result-invalid"));
                    }
                }
            }
            PendingOp::Submit => {
                app.notify_result(&result);
                if !result.ok {
                    return;
                }
                let record = result
                    .payload
                    .and_then(|payload| serde_json::from_value::<FirmwareRequest>(payload).ok());
                match record {
                    Some(record) => self.created = Some(record),
                    None => {
                        app.notify(egui_notify::ToastLevel::Error, tr!("worker-result-invalid"));
                    }
                }
            }
        }
    }
}
