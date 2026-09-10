use crate::app::HaucetApp;
use crate::pages::{Page, ResultView, run_button};
use crate::util::{
    human_size, open_in_file_manager, section, sibling_output_path, update_derived_path,
};
use common::package::{PackageFormat, PackageIndex, UpdateLayout};
use eframe::egui;
use egui_extras::{Column, TableBuilder};

#[derive(Debug)]
enum PendingOp {
    Inspect { input: String, layout: UpdateLayout },
    Unpack { input: String, output: String },
}

#[derive(Debug, Default)]
pub struct PackagePage {
    pub input: String,
    pub output: String,
    pub layout: UpdateLayout,
    pub force: bool,
    pub all_erofs: bool,
    pub custom_partitions: String,

    pub index: Option<PackageIndex>,
    pub checked: Vec<bool>,
    pub result: Option<ResultView>,
    inspect_pending: bool,
    input_initialized: bool,
    input_dirty: bool,
    pending: Option<PendingOp>,
    auto_output: Option<String>,
}

impl PackagePage {
    pub fn select_input(&mut self, input: String) {
        self.input = input;
        self.input_dirty = false;
        self.update_auto_output();
        self.queue_inspect();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        if !self.input_initialized {
            self.input_initialized = true;
            if !self.input.trim().is_empty() {
                self.queue_inspect();
            }
        }

        egui::ScrollArea::vertical()
            .id_salt("package-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let input_response = path_row(
                    ui,
                    app,
                    &tr!("update-file"),
                    &mut self.input,
                    &tr!("choose-file"),
                    Some(&["zip", "bin", "app"]),
                );
                if input_response.changed {
                    self.input_dirty = true;
                    self.invalidate_inspection();
                }
                if input_response.committed && self.input_dirty {
                    self.input_dirty = false;
                    self.update_auto_output();
                    self.queue_inspect();
                }
                ui.add_space(6.0);

                path_row(
                    ui,
                    app,
                    &tr!("output-directory"),
                    &mut self.output,
                    &tr!("choose-directory"),
                    None,
                );

                let drops = app.take_drops(ui.ctx());
                if let Some(path) = drops.first() {
                    self.select_input(path.display().to_string());
                }

                if self.inspect_pending && !app.job_running() && !self.input.trim().is_empty() {
                    self.start_inspect(app);
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready = !app.job_running() && self.can_unpack();
                    if run_button(ui, &tr!("start-unpack"), ready, None).clicked() {
                        let partitions = self.selected_partitions();
                        let output = self.output.trim().to_owned();
                        self.pending = Some(PendingOp::Unpack {
                            input: self.input.trim().to_owned(),
                            output: output.clone(),
                        });
                        self.result = None;
                        app.start_job(crate::worker::JobOp::PackageUnpack {
                            input: self.input.trim().to_owned(),
                            output,
                            partitions,
                            all_erofs: self.all_erofs,
                            layout: self.layout,
                            force: self.force,
                        });
                    }
                    if app.job_running() {
                        ui.label(egui::RichText::new(tr!("task-running")).weak());
                    }
                });

                ui.add_space(4.0);
                let layout_before = self.layout;
                let is_app = self.is_update_app();
                egui::CollapsingHeader::new(tr!("advanced-options"))
                    .id_salt("package-advanced")
                    .show(ui, |ui| {
                        egui::Grid::new("package-advanced-grid")
                            .num_columns(2)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                if !is_app {
                                    ui.label(tr!("update-bin-layout"));
                                    egui::ComboBox::from_id_salt("package-layout")
                                        .selected_text(layout_label(self.layout))
                                        .show_ui(ui, |ui| {
                                            for layout in [
                                                UpdateLayout::Auto,
                                                UpdateLayout::L1,
                                                UpdateLayout::L2,
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.layout,
                                                    layout,
                                                    layout_label(layout),
                                                );
                                            }
                                        });
                                    ui.end_row();
                                }
                                ui.label(tr!("custom-partitions"));
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.custom_partitions)
                                        .hint_text(tr!("custom-partitions-hint"))
                                        .desired_width(380.0),
                                );
                                ui.end_row();
                                ui.label(tr!("options"));
                                ui.horizontal_wrapped(|ui| {
                                    ui.checkbox(&mut self.force, tr!("overwrite-existing-output"));
                                    if !is_app {
                                        ui.checkbox(
                                            &mut self.all_erofs,
                                            tr!("only-erofs-partitions"),
                                        );
                                    }
                                });
                                ui.end_row();
                            });
                    });

                if self.layout != layout_before && !self.input.trim().is_empty() {
                    self.queue_inspect();
                }

                ui.add_space(8.0);
                self.show_result(ui);
                if let Some(index) = self.index.clone() {
                    ui.horizontal(|ui| {
                        ui.strong(format!("{}:", tr!("package-format")));
                        ui.monospace(index.format.to_string());
                    });
                    if let Some(version) = &index.package_version {
                        ui.horizontal(|ui| {
                            ui.strong(format!("{}:", tr!("package-version")));
                            ui.monospace(version);
                        });
                        ui.add_space(6.0);
                    }
                    self.partition_table(ui, &index);
                }
            });
    }

    fn queue_inspect(&mut self) {
        self.inspect_pending = !self.input.trim().is_empty();
        self.clear_inspection();
    }

    fn update_auto_output(&mut self) {
        let next = sibling_output_path(&self.input, "package", "-work");
        update_derived_path(&mut self.output, &mut self.auto_output, next);
    }

    fn invalidate_inspection(&mut self) {
        self.inspect_pending = false;
        self.clear_inspection();
    }

    fn clear_inspection(&mut self) {
        self.index = None;
        self.checked.clear();
        self.result = None;
    }

    fn start_inspect(&mut self, app: &mut HaucetApp) {
        let input = self.input.trim().to_owned();
        let layout = self.layout;
        self.inspect_pending = false;
        self.clear_inspection();
        self.pending = Some(PendingOp::Inspect {
            input: input.clone(),
            layout,
        });
        app.start_job(crate::worker::JobOp::PackageInspect {
            input: input.clone(),
            layout,
        });
    }

    pub(crate) fn poll_result(&mut self, app: &mut HaucetApp) {
        let Some(result) = app.take_result(Page::Package) else {
            return;
        };
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            PendingOp::Inspect { input, layout } => {
                if input != self.input.trim() || layout != self.layout {
                    return;
                }
                if !result.ok {
                    app.notify_result(&result);
                    self.result = Some(ResultView {
                        ok: false,
                        summary: result.summary,
                        output: String::new(),
                    });
                    return;
                }
                let index = match result
                    .payload
                    .and_then(|payload| serde_json::from_value::<PackageIndex>(payload).ok())
                {
                    Some(index) => index,
                    None => {
                        let summary = tr!("package-index-invalid");
                        app.notify(egui_notify::ToastLevel::Error, summary.clone());
                        self.result = Some(ResultView {
                            ok: false,
                            summary,
                            output: String::new(),
                        });
                        return;
                    }
                };
                let image_count = index
                    .components
                    .iter()
                    .filter(|component| component.component_type == 0)
                    .count();
                self.checked = index
                    .components
                    .iter()
                    .map(|component| component.component_type == 0)
                    .collect();
                app.notify(
                    egui_notify::ToastLevel::Info,
                    tr!(
                        "package-component-count",
                        "components" => index.components.len(),
                        "images" => image_count,
                    ),
                );
                if index.format == PackageFormat::UpdateApp {
                    self.layout = UpdateLayout::Auto;
                    self.all_erofs = false;
                }
                self.index = Some(index);
            }
            PendingOp::Unpack { input, output } => {
                if input != self.input.trim() {
                    return;
                }
                app.notify_result(&result);
                self.result = Some(ResultView {
                    ok: result.ok,
                    summary: result.summary,
                    output: if result.ok { output } else { String::new() },
                });
            }
        }
    }

    fn is_update_app(&self) -> bool {
        self.index
            .as_ref()
            .is_some_and(|index| index.format == PackageFormat::UpdateApp)
    }

    fn can_unpack(&self) -> bool {
        !self.input.trim().is_empty()
            && !self.output.trim().is_empty()
            && !self.input_dirty
            && !self.inspect_pending
            && self.index.is_some()
            && ((!self.is_update_app() && self.all_erofs) || !self.selected_partitions().is_empty())
    }

    fn selected_partitions(&self) -> Vec<String> {
        let custom = self
            .custom_partitions
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !custom.is_empty() {
            return custom;
        }
        let Some(index) = &self.index else {
            return Vec::new();
        };
        index
            .components
            .iter()
            .zip(self.checked.iter())
            .filter(|(_, checked)| **checked)
            .map(|(component, _)| component.name.clone())
            .collect()
    }

    fn partition_table(&mut self, ui: &mut egui::Ui, index: &PackageIndex) {
        section(ui, &tr!("package-partitions"));
        ui.horizontal(|ui| {
            if ui.button(tr!("select-all")).clicked() {
                for (component, checked) in index.components.iter().zip(&mut self.checked) {
                    *checked = component.component_type == 0;
                }
            }
            if ui.button(tr!("select-none")).clicked() {
                for checked in &mut self.checked {
                    *checked = false;
                }
            }
        });
        ui.add_space(4.0);
        let components = &index.components;
        let mut checked = self.checked.clone();
        let mut changed_selection = None;
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(50.0))
            .column(Column::auto().at_least(200.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::remainder().at_least(120.0))
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong(tr!("unpack"));
                });
                header.col(|ui| {
                    ui.strong(tr!("partition"));
                });
                header.col(|ui| {
                    ui.strong(tr!("type"));
                });
                header.col(|ui| {
                    ui.strong(tr!("size"));
                });
                header.col(|ui| {
                    ui.strong(tr!("data-offset"));
                });
            })
            .body(|mut body| {
                for (position, component) in components.iter().enumerate() {
                    let selectable = component.component_type == 0;
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            if selectable {
                                if ui.checkbox(&mut checked[position], "").changed() {
                                    changed_selection = Some((position, checked[position]));
                                }
                            } else {
                                ui.weak("—");
                            }
                        });
                        row.col(|ui| {
                            ui.label(&component.output_name);
                        });
                        row.col(|ui| {
                            ui.label(component_type_label(component.component_type));
                        });
                        row.col(|ui| {
                            ui.label(human_size(component.size));
                        });
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(crate::util::hex64(component.data_offset))
                                    .monospace()
                                    .weak(),
                            );
                        });
                    });
                }
            });
        self.checked = checked;
        if let Some((position, selected)) = changed_selection {
            self.set_partition_checked(position, selected);
        }
    }

    fn set_partition_checked(&mut self, position: usize, selected: bool) {
        let Some(index) = &self.index else {
            return;
        };
        let name = &index.components[position].name;
        // APP selection is by header name, so duplicate records form one group.
        for (other, (component, checked)) in
            index.components.iter().zip(&mut self.checked).enumerate()
        {
            if other == position
                || (index.format == PackageFormat::UpdateApp && component.name == *name)
            {
                *checked = selected;
            }
        }
    }

    fn show_result(&self, ui: &mut egui::Ui) {
        let Some(result) = &self.result else {
            return;
        };
        ui.add_space(6.0);
        if result.ok
            && !result.output.is_empty()
            && ui.button(tr!("open-output-directory")).clicked()
        {
            open_in_file_manager(std::path::Path::new(&result.output));
        }
    }
}

fn layout_label(layout: UpdateLayout) -> String {
    match layout {
        UpdateLayout::Auto => tr!("auto-detect"),
        UpdateLayout::L1 => "L1".to_owned(),
        UpdateLayout::L2 => "L2".to_owned(),
    }
}

#[derive(Debug, Default)]
struct PathRowResponse {
    changed: bool,
    committed: bool,
}

fn path_row(
    ui: &mut egui::Ui,
    app: &mut HaucetApp,
    label: &str,
    value: &mut String,
    button: &str,
    filter: Option<&[&str]>,
) -> PathRowResponse {
    let mut result = PathRowResponse::default();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).strong());
        let response = ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(tr!("path-or-drop-hint"))
                .desired_width(ui.available_width() - 150.0),
        );
        result.changed |= response.changed();
        result.committed |= response.lost_focus();
        if ui.button(button).clicked() {
            let filter_name = tr!("filter-update-package");
            let filters: &[(&str, &[&str])] = match filter {
                Some(extensions) => &[(filter_name.as_str(), extensions)],
                None => &[],
            };
            let picked = if filter.is_some() {
                app.pick_file(label, filters)
            } else {
                app.pick_dir(label)
            };
            if let Some(path) = picked {
                *value = path.display().to_string();
                result.changed = true;
                result.committed = true;
            }
        }
    });
    result
}

fn component_type_label(component_type: u8) -> String {
    match component_type {
        0 => tr!("image"),
        1 => tr!("archive"),
        other => tr!("type-number", "number" => other),
    }
}
