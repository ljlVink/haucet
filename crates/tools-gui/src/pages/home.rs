use crate::app::HaucetApp;
use crate::detect::{self, FileKind};
use crate::pages::Page;
use eframe::egui;

#[derive(Debug, Default)]
pub struct HomePage {
    pub detection: Option<detect::Detection>,
}

impl HomePage {
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        egui::ScrollArea::vertical()
            .id_salt("home-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.header(ui, app);
                ui.add_space(10.0);
                self.drop_zone(ui, app);
                ui.add_space(14.0);
                if let Some(detection) = self.detection.clone() {
                    self.detection_card(ui, app, &detection);
                }
                ui.add_space(14.0);
                self.quick_actions(ui, app);
                ui.add_space(14.0);
                self.recent_files(ui, app);
                ui.add_space(30.0);
            });
    }

    fn header(&self, ui: &mut egui::Ui, app: &HaucetApp) {
        ui.horizontal(|ui| {
            if let Some(logo) = &app.logo {
                ui.add(
                    egui::Image::new(logo)
                        .fit_to_exact_size(egui::vec2(72.0, 72.0))
                        .corner_radius(8),
                );
            }
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 72.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(egui::RichText::new(tr!("home-welcome")).strong().size(22.0));
                },
            );
        });
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.detection = Some(detect::detect(path));
            app.settings.remember_path(path);
        }

        let height = 130.0;
        let (rect, _response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let hovered = ui.ctx().input(|input| {
            let has_hovered_file = input
                .raw
                .hovered_files
                .iter()
                .any(|file| file.path.is_some());
            has_hovered_file
                && input
                    .pointer
                    .hover_pos()
                    .is_none_or(|pos| rect.contains(pos))
        });
        let fill = if hovered {
            ui.visuals().selection.bg_fill.gamma_multiply(0.6)
        } else {
            ui.visuals().extreme_bg_color
        };
        ui.painter().rect(
            rect,
            egui::CornerRadius::same(10),
            fill,
            egui::Stroke::new(
                1.5_f32,
                if hovered {
                    ui.visuals().selection.stroke.color
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                },
            ),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            if hovered {
                tr!("home-drop-release")
            } else {
                tr!("home-drop-prompt")
            },
            egui::FontId::proportional(18.0),
            if hovered {
                ui.visuals().selection.stroke.color
            } else {
                ui.visuals().text_color()
            },
        );
    }

    fn detection_card(
        &self,
        ui: &mut egui::Ui,
        app: &mut HaucetApp,
        detection: &detect::Detection,
    ) {
        ui.label(
            egui::RichText::new(tr!("home-detection-result"))
                .strong()
                .size(15.0),
        );
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(detection.kind.label())
                            .strong()
                            .color(accent()),
                    );
                    ui.label(egui::RichText::new(&detection.path).weak().monospace());
                });
                ui.label(&detection.human);
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for (label, page, kind) in suggested_actions(detection.kind) {
                        if ui.button(label).clicked() {
                            apply_action(app, page, kind, &detection.path);
                        }
                    }
                });
            });
    }

    fn quick_actions(&self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        ui.label(
            egui::RichText::new(tr!("home-common-tasks"))
                .strong()
                .size(15.0),
        );
        ui.add_space(4.0);
        let spacing = 12.0;
        let available_width = ui.available_width();
        let columns = if available_width >= 624.0 { 2 } else { 1 };
        let card_width = if columns == 2 {
            (available_width - spacing) / 2.0
        } else {
            available_width
        };

        egui::Grid::new("quick-actions")
            .num_columns(columns)
            .spacing([spacing, spacing])
            .show(ui, |ui| {
                quick_card(
                    ui,
                    &tr!("home-task-unpack-package"),
                    Page::Package,
                    card_width,
                    app,
                );
                if columns == 1 {
                    ui.end_row();
                }
                quick_card(ui, &tr!("page-images-title"), Page::Images, card_width, app);
                ui.end_row();
                if quick_card(
                    ui,
                    &tr!("home-task-identify-image"),
                    Page::Images,
                    card_width,
                    app,
                ) {
                    app.images.kind = crate::pages::images::ImageKind::Partition;
                }
                if columns == 1 {
                    ui.end_row();
                }
                quick_card(
                    ui,
                    &tr!("home-task-browse-cpio"),
                    Page::Cpio,
                    card_width,
                    app,
                );
                ui.end_row();
                quick_card(
                    ui,
                    &tr!("page-fastboot-title"),
                    Page::Fastboot,
                    card_width,
                    app,
                );
                if columns == 1 {
                    ui.end_row();
                }
                quick_card(ui, &tr!("page-vcom-title"), Page::Vcom, card_width, app);
                ui.end_row();
            });
    }

    fn recent_files(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        if app.settings.recent.is_empty() {
            return;
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tr!("home-recent")).strong().size(15.0));
        let recents = app.settings.recent.clone();
        for recent in recents {
            let path = std::path::PathBuf::from(&recent);
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| recent.clone());
            if ui
                .add(egui::Button::new(format!("📄 {label}")).truncate())
                .on_hover_text(&recent)
                .clicked()
            {
                let detection = detect::detect(&path);
                self.detection = Some(detection);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionKind {
    Input,
    ErofsInput,
    Ext4Input,
    ErofsWorkspace,
    RamdiskInput,
    RamdiskWorkspace,
    PartitionInput,
    NvmeInput,
    OemInfoInput,
}

fn suggested_actions(kind: FileKind) -> Vec<(String, Page, ActionKind)> {
    match kind {
        FileKind::ZipPackage | FileKind::UpdateApp => vec![(
            tr!("action-unpack-package"),
            Page::Package,
            ActionKind::Input,
        )],
        FileKind::Erofs => vec![
            (
                tr!("action-unpack-erofs"),
                Page::Images,
                ActionKind::ErofsInput,
            ),
            (
                tr!("action-view-partition"),
                Page::Images,
                ActionKind::PartitionInput,
            ),
        ],
        FileKind::Ext4 => vec![
            (
                tr!("action-unpack-ext4"),
                Page::Images,
                ActionKind::Ext4Input,
            ),
            (
                tr!("action-view-partition"),
                Page::Images,
                ActionKind::PartitionInput,
            ),
        ],
        FileKind::HarmonyFrame => vec![
            (
                tr!("action-ramdisk"),
                Page::Images,
                ActionKind::RamdiskInput,
            ),
            (
                tr!("action-view-partition"),
                Page::Images,
                ActionKind::PartitionInput,
            ),
        ],
        FileKind::Rvt => vec![(
            tr!("action-view-rvt"),
            Page::Images,
            ActionKind::PartitionInput,
        )],
        FileKind::Gpt => vec![(
            tr!("action-view-gpt"),
            Page::Images,
            ActionKind::PartitionInput,
        )],
        FileKind::SecImage => {
            vec![(
                tr!("action-view-sec-image"),
                Page::Images,
                ActionKind::PartitionInput,
            )]
        }
        FileKind::HvbWrapped => {
            vec![(
                tr!("action-view-partition"),
                Page::Images,
                ActionKind::PartitionInput,
            )]
        }
        FileKind::Nve => vec![(tr!("action-open-nve"), Page::Nvme, ActionKind::NvmeInput)],
        FileKind::OemInfo => vec![(
            tr!("action-browse-oeminfo"),
            Page::OemInfo,
            ActionKind::OemInfoInput,
        )],
        FileKind::Cpio => vec![(tr!("action-browse-cpio"), Page::Cpio, ActionKind::Input)],
        FileKind::ErofsWorkspace => {
            vec![(
                tr!("action-repack-image"),
                Page::Images,
                ActionKind::ErofsWorkspace,
            )]
        }
        FileKind::RamdiskWorkspace => {
            vec![(
                tr!("action-repack-image"),
                Page::Images,
                ActionKind::RamdiskWorkspace,
            )]
        }
        FileKind::Unknown => vec![(
            tr!("action-try-partition"),
            Page::Images,
            ActionKind::PartitionInput,
        )],
    }
}

fn apply_action(app: &mut HaucetApp, page: Page, kind: ActionKind, path: &str) {
    match (page, kind) {
        (Page::Package, ActionKind::Input) => {
            app.package.select_input(path.to_owned());
        }
        (Page::Images, ActionKind::ErofsInput) => {
            app.images.kind = crate::pages::images::ImageKind::Erofs;
            app.images.erofs.select_unpack_image(path.to_owned());
            app.images.erofs.tab = crate::pages::erofs::ErofsTab::Unpack;
        }
        (Page::Images, ActionKind::ErofsWorkspace) => {
            app.images.kind = crate::pages::images::ImageKind::Erofs;
            app.images.erofs.select_workspace(path.to_owned());
            app.images.erofs.tab = crate::pages::erofs::ErofsTab::Repack;
        }
        (Page::Images, ActionKind::Ext4Input) => {
            app.images.kind = crate::pages::images::ImageKind::Ext4;
            app.images.ext4.select_image(path.to_owned());
        }
        (Page::Images, ActionKind::RamdiskInput) => {
            app.images.kind = crate::pages::images::ImageKind::Ramdisk;
            app.images.ramdisk.select_patch_image(path.to_owned());
            app.images.ramdisk.tab = crate::pages::ramdisk::RamdiskTab::Patch;
        }
        (Page::Images, ActionKind::RamdiskWorkspace) => {
            app.images.kind = crate::pages::images::ImageKind::Ramdisk;
            app.images.ramdisk.repack.workspace = path.to_owned();
            app.images.ramdisk.tab = crate::pages::ramdisk::RamdiskTab::Repack;
        }
        (Page::Images, ActionKind::PartitionInput) => {
            app.images.kind = crate::pages::images::ImageKind::Partition;
            app.images.partition.select_input(path.to_owned());
        }
        (Page::Nvme, ActionKind::NvmeInput) => {
            app.nvme.select_input(path.to_owned());
        }
        (Page::OemInfo, ActionKind::OemInfoInput) => {
            app.oeminfo.select_input(path.to_owned());
        }
        (Page::Cpio, _) => {
            app.cpio.select_input(path.to_owned());
        }
        _ => {}
    }
    app.nav(page);
}

fn quick_card(
    ui: &mut egui::Ui,
    title: &str,
    page: Page,
    outer_width: f32,
    app: &mut HaucetApp,
) -> bool {
    let predicted_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(outer_width, 48.0));
    let hovered = ui.input(|input| {
        input
            .pointer
            .hover_pos()
            .is_some_and(|pos| predicted_rect.contains(pos))
    });
    let visuals = ui.visuals();
    let fill = if hovered {
        visuals.widgets.hovered.bg_fill
    } else {
        visuals.extreme_bg_color
    };
    let stroke = if hovered {
        visuals.widgets.hovered.bg_stroke
    } else {
        visuals.widgets.noninteractive.bg_stroke
    };
    let response = egui::Frame::group(ui.style())
        .fill(fill)
        .stroke(stroke)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width((outer_width - 24.0).max(0.0));
            ui.label(egui::RichText::new(title).strong().size(15.0));
        })
        .response
        .interact(egui::Sense::click());
    let clicked = response.clicked();
    if clicked {
        app.nav(page);
    }
    clicked
}

fn accent() -> egui::Color32 {
    egui::Color32::from_rgb(90, 170, 255)
}
