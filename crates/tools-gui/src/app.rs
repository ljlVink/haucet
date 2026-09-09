use crate::i18n::{self, Language};
use crate::job::{self, JobEvent, JobResult, RunningJob};
use crate::pages::images::ImageKind;
use crate::pages::{self, Page};
use crate::settings::Settings;
use crate::worker::JobOp;
use eframe::egui;
use std::path::PathBuf;

const LICENSE_SPDX: &str = env!("CARGO_PKG_LICENSE");
const REPOSITORY_URL: &str = "https://github.com/ljlVink/haucet";

pub(crate) struct HaucetApp {
    pub current: Page,
    pub home: pages::home::HomePage,
    pub package: pages::package::PackagePage,
    pub online: pages::online::OnlinePage,
    pub images: pages::images::ImagesPage,
    pub fastboot: pages::fastboot::FastbootPage,
    pub vcom: pages::vcom::VcomPage,
    pub cpio: pages::cpio::CpioPage,
    pub nvme: pages::nvme::NvmePage,
    pub oeminfo: pages::oeminfo::OemInfoPage,

    pub job: Option<RunningJob>,
    job_owner: ResultOwner,
    pub logs: Vec<String>,
    pub settings: Settings,
    pub font_loaded: bool,
    pub logo: Option<egui::TextureHandle>,
    vibrancy_enabled: bool,
    transparent_window_at_startup: bool,
    native_theme: Option<egui::Theme>,
    dialog: Option<AppDialog>,
    results: ResultStore,
}

#[derive(Clone, Copy)]
enum AppDialog {
    About,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultOwner {
    Page(Page),
    Image(ImageKind),
}

#[derive(Debug, Default)]
struct ResultStore {
    pending: Vec<(ResultOwner, JobResult)>,
}

impl ResultStore {
    fn insert(&mut self, owner: ResultOwner, result: JobResult) {
        self.remove(owner);
        self.pending.push((owner, result));
    }

    fn remove(&mut self, owner: ResultOwner) {
        self.pending.retain(|(stored, _)| *stored != owner);
    }

    fn take(&mut self, owner: ResultOwner) -> Option<JobResult> {
        let index = self
            .pending
            .iter()
            .position(|(stored, _)| *stored == owner)?;
        Some(self.pending.remove(index).1)
    }
}

impl HaucetApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        font_loaded: bool,
        logo_rgba: Option<(Vec<u8>, [usize; 2])>,
    ) -> Self {
        let logo = logo_rgba.and_then(|(rgba, [width, height])| {
            if rgba.len() != width * height * 4 || width == 0 || height == 0 {
                return None;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
            Some(
                cc.egui_ctx
                    .load_texture("haucet-logo", image, egui::TextureOptions::LINEAR),
            )
        });
        cc.egui_ctx.set_theme(if settings.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let transparent_window_at_startup = settings.transparent_window;
        let vibrancy_enabled = transparent_window_at_startup && crate::vibrancy::apply(cc);
        Self {
            current: Page::Home,
            home: pages::home::HomePage::default(),
            package: pages::package::PackagePage::default(),
            online: pages::online::OnlinePage::default(),
            images: pages::images::ImagesPage::default(),
            fastboot: pages::fastboot::FastbootPage::default(),
            vcom: pages::vcom::VcomPage::default(),
            cpio: pages::cpio::CpioPage::default(),
            nvme: pages::nvme::NvmePage::default(),
            oeminfo: pages::oeminfo::OemInfoPage::default(),
            job: None,
            job_owner: ResultOwner::Page(Page::Home),
            logs: Vec::new(),
            settings,
            font_loaded,
            logo,
            vibrancy_enabled,
            transparent_window_at_startup,
            native_theme: None,
            dialog: None,
            results: ResultStore::default(),
        }
    }
}

impl eframe::App for HaucetApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.window_title()));
        let theme = ctx.theme();
        if self.native_theme != Some(theme) {
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(match theme {
                egui::Theme::Dark => egui::SystemTheme::Dark,
                egui::Theme::Light => egui::SystemTheme::Light,
            }));
            self.native_theme = Some(theme);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.vibrancy_enabled {
            // Avoid tinting only the client area on top of the native backdrop.
            ui.visuals_mut().panel_fill = egui::Color32::TRANSPARENT;
        }

        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(200.0)
            .show_inside(ui, |ui| self.nav_panel(ui));
        egui::Panel::bottom("log-panel").show_inside(ui, |ui| self.log_panel(ui));
        self.page_header_panel(ui);
        egui::CentralPanel::default().show_inside(ui, |ui| self.central(ui));

        if !self.font_loaded {
            egui::Area::new("font-warning".into())
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 46.0))
                .show(&ctx, |ui| {
                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(90, 60, 10).gamma_multiply(0.9))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(tr!("font-warning"))
                                    .color(egui::Color32::from_rgb(255, 220, 160)),
                            );
                        });
                });
        }

        self.show_dialog(&ctx);

        if self.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.vibrancy_enabled {
            egui::Color32::TRANSPARENT.to_normalized_gamma_f32()
        } else {
            visuals.panel_fill.to_normalized_gamma_f32()
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.save();
    }
}

impl HaucetApp {
    fn poll_job(&mut self) {
        let mut events = Vec::new();
        if let Some(job) = &mut self.job {
            while let Some(event) = job.poll() {
                events.push(event);
            }
        }
        let mut finished = false;
        for event in events {
            match event {
                JobEvent::Log(line) => self.push_log(line),
                JobEvent::Done(result) => {
                    let owner = self.job_owner;
                    let mark = if result.cancelled {
                        tr!("job-status-cancelled")
                    } else if result.ok {
                        tr!("job-status-success")
                    } else {
                        tr!("job-status-failed")
                    };
                    self.push_log(format!("{mark} {}", result.summary));
                    self.results.insert(owner, result);
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
            self.settings.save();
        }
    }

    pub fn job_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn start_job(&mut self, op: JobOp) -> bool {
        if self.job.is_some() {
            return false;
        }
        let owner = result_owner(&op, self.current);
        match job::start(op) {
            Ok(running) => {
                let label = job_label(&running.op);
                self.job_owner = owner;
                self.push_log(tr!("job-start", "task" => label));
                self.job = Some(running);
                self.results.remove(owner);
                true
            }
            Err(error) => {
                let error = format!("{error:#}");
                let message = tr!("job-start-error", "error" => error.clone());
                self.push_log(tr!("job-error-prefix", "message" => message.clone()));
                self.results.insert(
                    owner,
                    JobResult {
                        ok: false,
                        cancelled: false,
                        summary: message,
                        payload: None,
                    },
                );
                false
            }
        }
    }

    pub fn cancel_job(&mut self) {
        if let Some(job) = &mut self.job {
            job.cancel();
        }
    }

    pub fn take_result(&mut self, page: Page) -> Option<JobResult> {
        self.results.take(ResultOwner::Page(page))
    }

    pub fn take_image_result(&mut self, kind: ImageKind) -> Option<JobResult> {
        self.results.take(ResultOwner::Image(kind))
    }

    pub fn nav(&mut self, page: Page) {
        if self.current != page {
            self.current = page;
        }
    }

    pub fn push_log(&mut self, line: String) {
        self.logs.push(line);
        if self.logs.len() > 2000 {
            let overflow = self.logs.len() - 2000;
            self.logs.drain(..overflow);
        }
    }

    pub fn pick_file(&mut self, title: &str, filters: &[(&str, &[&str])]) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some(dir) = &self.settings.last_dir {
            dialog = dialog.set_directory(dir);
        }
        for (name, extensions) in filters {
            dialog = dialog.add_filter(*name, extensions);
        }
        let picked = dialog.pick_file();
        if let Some(path) = &picked {
            self.settings.remember_path(path);
        }
        picked
    }

    pub fn pick_dir(&mut self, title: &str) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some(dir) = &self.settings.last_dir {
            dialog = dialog.set_directory(dir);
        }
        let picked = dialog.pick_folder();
        if let Some(path) = &picked {
            self.settings.remember_path(path);
        }
        picked
    }

    pub fn pick_save(&mut self, title: &str, file_name: &str) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new()
            .set_title(title)
            .set_file_name(file_name);
        if let Some(dir) = &self.settings.last_dir {
            dialog = dialog.set_directory(dir);
        }
        let picked = dialog.save_file();
        if let Some(path) = &picked {
            self.settings.remember_path(path);
        }
        picked
    }

    pub fn take_drops(&mut self, ctx: &egui::Context) -> Vec<PathBuf> {
        ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect::<Vec<_>>()
        })
    }

    fn window_title(&self) -> String {
        format_window_title(self.job.as_ref().map(RunningJob::elapsed))
    }

    fn nav_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if nav_button(ui, self.current, Page::Home) {
            self.nav(Page::Home);
        }

        nav_group_label(ui, &tr!("nav-files-images"));
        for page in [
            Page::Package,
            Page::Online,
            Page::Images,
            Page::Cpio,
            Page::OemInfo,
            Page::Nvme,
        ] {
            if nav_button(ui, self.current, page) {
                self.nav(page);
            }
        }

        nav_group_label(ui, &tr!("nav-devices-flashing"));
        for page in [Page::Fastboot, Page::Vcom] {
            if nav_button(ui, self.current, page) {
                self.nav(page);
            }
        }
        nav_group_label(ui, &tr!("nav-other"));
        for (dialog, label) in [
            (AppDialog::About, tr!("about-heading")),
            (AppDialog::Settings, tr!("settings-heading")),
        ] {
            if ui
                .add_sized(
                    [ui.available_width(), 40.0],
                    egui::Button::selectable(false, egui::RichText::new(label).size(15.0)),
                )
                .clicked()
            {
                self.dialog = Some(dialog);
            }
        }
        if common::version::GIT_DIRTY {
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(tr!("unstable-version"))
                        .size(16.0)
                        .color(egui::Color32::ORANGE),
                );
            });
        }
    }

    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog else {
            return;
        };
        let (id, title) = match dialog {
            AppDialog::About => ("about-dialog", tr!("about-heading")),
            AppDialog::Settings => ("settings-dialog", tr!("settings-heading")),
        };
        let response = egui::Modal::new(egui::Id::new(id))
            .frame(egui::Frame::popup(&ctx.global_style()).inner_margin(20))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(48.0, 48.0)));
                        ui.add_space(8.0);
                    }
                    ui.heading(title);
                });
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);
                match dialog {
                    AppDialog::About => {
                        ui.heading("Haucet");
                        ui.label(tr!("about-description"));
                        ui.add_space(8.0);
                        ui.label(tr!("about-version", "version" => common::version::VERSION));
                        ui.label(LICENSE_SPDX);
                        ui.hyperlink_to(tr!("repository-label"), REPOSITORY_URL);
                    }
                    AppDialog::Settings => {
                        ui.horizontal(|ui| {
                            ui.label(tr!("language-label"));
                            let previous = self.settings.language;
                            egui::ComboBox::from_id_salt("language-select")
                                .selected_text(self.settings.language.native_name())
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for language in Language::ALL {
                                        ui.selectable_value(
                                            &mut self.settings.language,
                                            language,
                                            language.native_name(),
                                        );
                                    }
                                });
                            if self.settings.language != previous {
                                i18n::set_language(self.settings.language);
                                self.settings.save();
                                ctx.request_repaint();
                            }
                        });
                        ui.add_space(8.0);
                        let transparency_changed = ui
                            .add_enabled(
                                crate::vibrancy::SUPPORTED,
                                egui::Checkbox::new(
                                    &mut self.settings.transparent_window,
                                    tr!("settings-transparent-window"),
                                ),
                            )
                            .changed();
                        if transparency_changed && self.settings.transparent_window {
                            self.settings.dark = false;
                        }
                        let dark_changed = ui
                            .add_enabled(
                                !self.settings.transparent_window && !self.vibrancy_enabled,
                                egui::Checkbox::new(
                                    &mut self.settings.dark,
                                    tr!("settings-dark-mode"),
                                ),
                            )
                            .on_disabled_hover_text(tr!("settings-dark-mode-unavailable"))
                            .changed();
                        if transparency_changed || dark_changed {
                            ctx.set_theme(if self.settings.dark {
                                egui::Theme::Dark
                            } else {
                                egui::Theme::Light
                            });
                            self.settings.save();
                            ctx.request_repaint();
                        }
                        if self.settings.transparent_window != self.transparent_window_at_startup {
                            ui.label(tr!("settings-transparency-restart"));
                        }
                    }
                }
                ui.add_space(20.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_sized([88.0, 32.0], egui::Button::new(tr!("dialog-close")))
                        .clicked()
                    {
                        ui.close();
                    }
                });
            });
        if response.should_close() {
            self.dialog = None;
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(tr!("log-clear")).clicked() {
                self.logs.clear();
            }
            if ui.button(tr!("log-copy")).clicked() {
                let text = self.logs.join("\n");
                ui.ctx().copy_text(text);
            }
            if let Some(job) = &self.job {
                ui.label(egui::RichText::new(job_label(&job.op)).weak());
            }
            if self.job.is_some() && ui.button(tr!("job-cancel")).clicked() {
                self.cancel_job();
            }
        });
        if !self.logs.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("job-log")
                .max_height(170.0)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.logs {
                        ui.label(egui::RichText::new(line).monospace().size(12.0));
                    }
                });
        }
    }

    fn page_header_panel(&self, ui: &mut egui::Ui) {
        let Some((title, description)) = self.current.header() else {
            return;
        };
        let busy = self.job.is_some()
            && match self.job_owner {
                ResultOwner::Page(owner) => owner == self.current,
                ResultOwner::Image(_) => self.current == Page::Images,
            };

        egui::Panel::top("page-header")
            .resizable(false)
            .exact_size(68.0)
            .show_separator_line(true)
            .show_inside(ui, |ui| {
                apply_content_text_style(ui);
                pages::page_header(ui, &title, &description, busy);
            });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let current = self.current;
        ui.scope(|ui| {
            apply_content_text_style(ui);
            match current {
                Page::Home => {
                    let mut page = std::mem::take(&mut self.home);
                    page.ui(ui, self);
                    self.home = page;
                }
                Page::Package => {
                    let mut page = std::mem::take(&mut self.package);
                    page.ui(ui, self);
                    self.package = page;
                }
                Page::Online => {
                    let mut page = std::mem::take(&mut self.online);
                    page.ui(ui, self);
                    self.online = page;
                }
                Page::Images => {
                    let mut page = std::mem::take(&mut self.images);
                    page.ui(ui, self);
                    self.images = page;
                }
                Page::Fastboot => {
                    let mut page = std::mem::take(&mut self.fastboot);
                    page.ui(ui, self);
                    self.fastboot = page;
                }
                Page::Vcom => {
                    let mut page = std::mem::take(&mut self.vcom);
                    page.ui(ui, self);
                    self.vcom = page;
                }
                Page::Cpio => {
                    let mut page = std::mem::take(&mut self.cpio);
                    page.ui(ui, self);
                    self.cpio = page;
                }
                Page::Nvme => {
                    let mut page = std::mem::take(&mut self.nvme);
                    page.ui(ui, self);
                    self.nvme = page;
                }
                Page::OemInfo => {
                    let mut page = std::mem::take(&mut self.oeminfo);
                    page.ui(ui, self);
                    self.oeminfo = page;
                }
            }
        });
    }
}

fn format_window_title(elapsed: Option<std::time::Duration>) -> String {
    match elapsed {
        Some(elapsed) => tr!("app-title-running", "seconds" => elapsed.as_secs()),
        None => tr!("app-title-idle"),
    }
}

fn nav_group_label(ui: &mut egui::Ui, label: &str) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(egui::RichText::new(label).weak().size(12.0));
    });
    ui.add_space(3.0);
}

fn nav_button(ui: &mut egui::Ui, current: Page, page: Page) -> bool {
    ui.add_sized(
        [ui.available_width(), 34.0],
        egui::Button::selectable(
            current == page,
            egui::RichText::new(page.title()).size(15.0),
        ),
    )
    .clicked()
}

fn apply_content_text_style(ui: &mut egui::Ui) {
    let text_styles = &mut ui.style_mut().text_styles;
    text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(15.5));
    text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(15.5));
    text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(14.5));
    text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(13.0));
}

fn job_label(op: &JobOp) -> String {
    use crate::worker::JobOp::*;
    match op {
        NvmeInspect { .. } => tr!("job-nvme-inspect"),
        NvmeEdit { .. } => tr!("job-nvme-edit"),
        OemInfoInspect { .. } => tr!("job-oeminfo-inspect"),
        OemInfoExportImage { .. } => tr!("job-oeminfo-export"),
        PackageInspect { .. } => tr!("job-package-inspect"),
        OnlineFetch { .. } => tr!("online-fetch"),
        PackageUnpack { .. } => tr!("job-package-unpack"),
        ErofsUnpack { .. } => tr!("job-erofs-unpack"),
        ErofsRepack { .. } => tr!("job-erofs-repack"),
        Ext4Unpack { .. } => tr!("job-ext4-unpack"),
        RamdiskUnpack { .. } => tr!("job-ramdisk-unpack"),
        RamdiskRepack { .. } => tr!("job-ramdisk-repack"),
        RamdiskPatch { .. } => tr!("job-ramdisk-patch"),
        RamdiskProbe { .. } => tr!("job-ramdisk-probe"),
        PartitionInfo { .. } => tr!("job-partition-info"),
        FastbootStatus { .. } => tr!("job-fastboot-status"),
        FastbootReboot { .. } => tr!("job-fastboot-reboot"),
        FastbootFlash { .. } => tr!("job-fastboot-flash"),
        FastbootExtract { .. } => tr!("job-fastboot-extract"),
        VcomStatus { .. } => tr!("job-vcom-status"),
        VcomFlash { .. } => tr!("job-vcom-flash"),
    }
}

fn result_owner(op: &JobOp, current: Page) -> ResultOwner {
    match op {
        JobOp::OnlineFetch { .. } => ResultOwner::Page(Page::Online),
        JobOp::PackageInspect { .. } | JobOp::PackageUnpack { .. } => {
            ResultOwner::Page(Page::Package)
        }
        JobOp::ErofsUnpack { .. } | JobOp::ErofsRepack { .. } => {
            ResultOwner::Image(ImageKind::Erofs)
        }
        JobOp::Ext4Unpack { .. } => ResultOwner::Image(ImageKind::Ext4),
        JobOp::RamdiskUnpack { .. }
        | JobOp::RamdiskRepack { .. }
        | JobOp::RamdiskPatch { .. }
        | JobOp::RamdiskProbe { .. } => ResultOwner::Image(ImageKind::Ramdisk),
        JobOp::PartitionInfo { .. } => ResultOwner::Image(ImageKind::Partition),
        _ => ResultOwner::Page(current),
    }
}
