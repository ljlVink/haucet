pub mod cpio;
pub mod entropy;
pub mod erofs;
pub mod ext4;
pub mod fastboot;
pub mod flash;
pub mod home;
pub mod images;
pub mod nvme;
pub mod oeminfo;
pub mod online;
pub mod package;
pub mod partition;
pub mod ramdisk;
pub mod vcom;

use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Package,
    Online,
    Images,
    Fastboot,
    Flash,
    Vcom,
    Cpio,
    Nvme,
    OemInfo,
}

impl Page {
    pub fn title(self) -> String {
        match self {
            Self::Home => tr!("page-home-title"),
            Self::Package => tr!("page-package-title"),
            Self::Online => tr!("page-online-title"),
            Self::Images => tr!("page-images-title"),
            Self::Fastboot => tr!("page-fastboot-title"),
            Self::Flash => tr!("page-flash-title"),
            Self::Vcom => tr!("page-vcom-title"),
            Self::Cpio => tr!("page-cpio-title"),
            Self::Nvme => tr!("page-nvme-editor-title"),
            Self::OemInfo => tr!("page-oeminfo-title"),
        }
    }

    pub fn header(self) -> Option<(String, String)> {
        match self {
            Self::Home => None,
            Self::Package => Some((tr!("page-package-title"), tr!("page-package-description"))),
            Self::Online => Some((tr!("page-online-title"), "VERSION.mbn".to_owned())),
            Self::Images => Some((tr!("page-images-title"), tr!("page-images-description"))),
            Self::Fastboot => Some((tr!("page-fastboot-title"), tr!("page-fastboot-description"))),
            Self::Flash => Some((tr!("page-flash-title"), tr!("page-flash-description"))),
            Self::Vcom => Some((tr!("page-vcom-title"), tr!("page-vcom-description"))),
            Self::Cpio => Some((tr!("page-cpio-title"), tr!("page-cpio-description"))),
            Self::Nvme => Some((tr!("page-nvme-editor-title"), tr!("page-nvme-description"))),
            Self::OemInfo => Some((tr!("page-oeminfo-title"), tr!("page-oeminfo-description"))),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResultView {
    pub ok: bool,
    pub summary: String,
    pub output: String,
}

pub fn badge_text(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(egui::RichText::new(text).color(color).strong());
}

pub(crate) fn page_header(ui: &mut egui::Ui, title: &str, description: &str, busy: bool) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).strong().size(22.0));
            ui.label(egui::RichText::new(description).weak());
        });
        if busy {
            ui.add_space(8.0);
            ui.add(egui::Spinner::new().size(16.0));
        }
    });
}

pub(crate) fn run_button(
    ui: &mut egui::Ui,
    text: &str,
    enabled: bool,
    hint: Option<&str>,
) -> egui::Response {
    let response = ui
        .allocate_ui_with_layout(
            egui::vec2(140.0, 32.0),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.add_enabled(
                    enabled,
                    egui::Button::new(egui::RichText::new(text).strong())
                        .min_size(egui::vec2(140.0, 32.0)),
                )
            },
        )
        .inner;
    match hint {
        Some(hint) => response.on_hover_text(hint),
        None => response,
    }
}
