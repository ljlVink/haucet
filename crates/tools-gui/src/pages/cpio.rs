use crate::app::HaucetApp;
use crate::pages::badge_text;
use crate::util::{human_size, mode_string};
use common::compress::decompress_vec;
use common::formats::cpio::{self, Cpio, S_IFDIR, S_IFMT};
use common::formats::harmony::HvbFrame;
use common::formats::header::check_fmt_full;
use eframe::egui;
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpioSource {
    #[default]
    File,
    Image,
    Workspace,
}

impl CpioSource {
    pub fn label(self) -> String {
        match self {
            Self::File => tr!("cpio-file"),
            Self::Image => tr!("ramdisk-image"),
            Self::Workspace => tr!("unpacked-workspace"),
        }
    }
}

#[derive(Debug, Default)]
pub struct CpioPage {
    pub source: CpioSource,
    pub path: String,
    pub loaded: Option<Loaded>,
    pub load_job: Option<LocalJob>,
    pub message: Option<(bool, String)>,
    pub selection: Option<String>,
    pub filter: String,
    pub expand: bool,
    pub dirty: bool,
    pub revision: u64,
    pub add_target: String,
    pub add_mode: String,
    pub mkdir_target: String,
    pub pending_add: Option<String>,
    load_requested: bool,
    active_load: Option<LoadRequest>,
    queued_load: Option<LoadRequest>,
}

pub struct Loaded {
    pub cpio: Cpio,
    pub source_path: String,
    pub from_image: bool,
    children: HashMap<String, Vec<String>>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("entries", &self.cpio.entries.len())
            .field("source_path", &self.source_path)
            .field("from_image", &self.from_image)
            .finish()
    }
}

impl Loaded {
    fn new(cpio: Cpio, source_path: String, from_image: bool) -> Self {
        let mut loaded = Self {
            cpio,
            source_path,
            from_image,
            children: HashMap::new(),
        };
        loaded.rebuild_tree();
        loaded
    }

    fn rebuild_tree(&mut self) {
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for path in self.cpio.entries.keys() {
            let mut parent = String::new();
            for name in path.split('/').filter(|part| !part.is_empty()) {
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(name.to_owned());
                parent = if parent.is_empty() {
                    name.to_owned()
                } else {
                    format!("{parent}/{name}")
                };
            }
        }
        for names in children.values_mut() {
            names.sort();
            names.dedup();
        }
        self.children = children;
    }

    fn stats(&self) -> (usize, usize) {
        let mut dirs = 0_usize;
        for entry in self.cpio.entries.values() {
            if entry.mode & S_IFMT == S_IFDIR {
                dirs += 1;
            }
        }
        (self.cpio.entries.len(), dirs)
    }

    fn snapshot(&self) -> Cpio {
        Cpio {
            entries: self.cpio.entries.clone(),
        }
    }
}

enum LocalOutcome {
    Loaded {
        request: LoadRequest,
        loaded: Loaded,
    },
    Done(String),
    Saved {
        path: String,
        revision: u64,
        update_source: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadRequest {
    source: CpioSource,
    path: String,
}

pub struct LocalJob {
    rx: Receiver<std::result::Result<LocalOutcome, String>>,
    pub label: String,
}

impl std::fmt::Debug for LocalJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalJob")
            .field("label", &self.label)
            .finish()
    }
}

impl LocalJob {
    fn poll(&mut self) -> Option<std::result::Result<LocalOutcome, String>> {
        match self.rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(
                tr!("local-job-disconnected", "job" => self.label.clone()),
            )),
        }
    }
}

fn spawn_local<F>(label: String, work: F) -> LocalJob
where
    F: FnOnce() -> anyhow::Result<LocalOutcome> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = work().map_err(|error| format!("{error:#}"));
        let _ = tx.send(result);
    });
    LocalJob { rx, label }
}

fn is_harmony_image(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0_u8; 8];
    file.read_exact(&mut head).is_ok() && &head == b"HARMONY!"
}

impl CpioPage {
    pub fn select_input(&mut self, path: String) {
        self.source = CpioSource::File;
        self.path = path;
        self.load_requested = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        if self.load_requested {
            self.load_requested = false;
            self.request_load(app);
        }
        let mut loaded = self.loaded.take();

        egui::ScrollArea::vertical()
            .id_salt("cpio-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.open_row(ui, app);
                if self.active_load.is_some() || self.queued_load.is_some() {
                    loaded = None;
                }
                ui.add_space(6.0);
                if let Some(loaded) = &loaded {
                    self.summary_row(ui, loaded);
                }
                ui.add_space(6.0);
                if let Some(loaded) = &mut loaded {
                    self.browser(ui, app, loaded);
                }
                ui.add_space(20.0);
            });

        self.notify_message(app);
        self.loaded = if self.active_load.is_some() || self.queued_load.is_some() {
            None
        } else {
            loaded
        };
        if self.load_job.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn open_row(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp) {
        let busy = self.load_job.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr!("source-type")).strong());
                for source in [CpioSource::File, CpioSource::Image, CpioSource::Workspace] {
                    ui.selectable_value(&mut self.source, source, source.label());
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("open-source")).strong());
            let field_width = (ui.available_width() - 190.0).max(120.0);
            let path_edit = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.path)
                    .hint_text(match self.source {
                        CpioSource::File => tr!("cpio-file-path"),
                        CpioSource::Image => tr!("ramdisk-image-path"),
                        CpioSource::Workspace => tr!("unpacked-workspace-path"),
                    })
                    .desired_width(field_width),
            );
            let mut load_requested =
                path_edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(
                    self.load_job.is_none(),
                    egui::Button::new(tr!("choose-source")),
                )
                .clicked()
            {
                let picked = match self.source {
                    CpioSource::File => {
                        app.pick_file(&tr!("choose-cpio-file"), &[("cpio", &["cpio"])])
                    }
                    CpioSource::Image => app.pick_file(
                        &tr!("choose-ramdisk-image"),
                        &[(tr!("filter-image").as_str(), &["img"])],
                    ),
                    CpioSource::Workspace => app.pick_dir(&tr!("choose-unpacked-workspace")),
                };
                if let Some(path) = picked {
                    self.path = path.display().to_string();
                    load_requested = true;
                }
            }
            if load_requested {
                self.request_load(app);
            }
        });
        let drops = app.take_drops(ui.ctx());
        if let Some(path) = drops.first() {
            self.path = path.display().to_string();
            if path.is_dir() {
                self.source = CpioSource::Workspace;
            } else if is_harmony_image(path) {
                self.source = CpioSource::Image;
            } else {
                self.source = CpioSource::File;
            }
            self.request_load(app);
        }
    }

    fn request_load(&mut self, app: &mut HaucetApp) {
        let request = LoadRequest {
            source: self.source,
            path: self.path.trim().to_owned(),
        };
        if request.path.is_empty() {
            return;
        }
        app.settings
            .remember_path(std::path::Path::new(&request.path));
        if self.load_job.is_some() {
            self.queued_load = Some(request);
            return;
        }
        self.begin_load(request);
    }

    fn begin_load(&mut self, request: LoadRequest) {
        self.source = request.source;
        self.path = request.path.clone();
        self.loaded = None;
        self.selection = None;
        self.message = None;
        self.dirty = false;
        self.revision = 0;
        self.active_load = Some(request.clone());
        let worker_request = request;
        self.load_job = Some(spawn_local(tr!("load-cpio"), move || {
            let source = worker_request.source;
            let path = worker_request.path.clone();
            let (cpio, from_image, source_path) = match source {
                CpioSource::File => (Cpio::load_from_file(&path)?, false, path.clone()),
                CpioSource::Workspace => {
                    let cpio_path = std::path::Path::new(&path).join("ramdisk.cpio");
                    let text = cpio_path
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!(tr!("path-not-utf8")))?;
                    (Cpio::load_from_file(text)?, false, text.to_owned())
                }
                CpioSource::Image => {
                    let frame = HvbFrame::load(std::path::Path::new(&path))?;
                    let payload = frame.extract_image_payload();
                    anyhow::ensure!(!payload.is_empty(), "{}", tr!("image-no-payload"));
                    let fmt = check_fmt_full(payload);
                    let bytes = if fmt.is_compressed() {
                        decompress_vec(fmt, payload).map_err(std::io::Error::other)?
                    } else {
                        payload.to_vec()
                    };
                    (Cpio::load_from_data(&bytes)?, true, path.clone())
                }
            };
            let loaded = Loaded::new(cpio, source_path, from_image);
            Ok(LocalOutcome::Loaded {
                request: worker_request,
                loaded,
            })
        }));
    }

    pub(crate) fn notify_message(&mut self, app: &mut HaucetApp) {
        if let Some((ok, text)) = self.message.take() {
            app.notify_outcome(ok, text);
        }
    }

    pub(crate) fn poll_local_job(&mut self, app: &mut HaucetApp) {
        let Some(job) = &mut self.load_job else {
            return;
        };
        let Some(result) = job.poll() else {
            return;
        };
        let label = job.label.clone();
        self.load_job = None;
        self.active_load = None;
        match result {
            Ok(LocalOutcome::Loaded { request, loaded }) => {
                let current = LoadRequest {
                    source: self.source,
                    path: self.path.trim().to_owned(),
                };
                if request == current {
                    self.message = Some((
                        true,
                        tr!("cpio-loaded", "count" => loaded.cpio.entries.len(), "source" => label),
                    ));
                    self.loaded = Some(loaded);
                } else {
                    self.message = Some((false, tr!("cpio-stale-result")));
                }
            }
            Ok(LocalOutcome::Done(text)) => {
                self.message = Some((true, text));
            }
            Ok(LocalOutcome::Saved {
                path,
                revision,
                update_source,
            }) => {
                if update_source {
                    if let Some(loaded) = &mut self.loaded {
                        loaded.source_path = path.clone();
                        loaded.from_image = false;
                    }
                    self.source = CpioSource::File;
                    self.path = path.clone();
                }
                if self.revision == revision {
                    self.dirty = false;
                }
                self.message = Some((true, tr!("saved-to", "path" => path)));
            }
            Err(error) => {
                self.message = Some((false, error));
            }
        }
        self.notify_message(app);
        if let Some(request) = self.queued_load.take() {
            self.begin_load(request);
        }
    }

    fn summary_row(&self, ui: &mut egui::Ui, loaded: &Loaded) {
        let (count, dirs) = loaded.stats();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(tr!("cpio-summary", "entries" => count, "directories" => dirs))
                    .weak(),
            );
            ui.separator();
            ui.label(egui::RichText::new(tr!("patch-status")).weak());
            match common::ramdisk::patch_status(&loaded.cpio) {
                common::ramdisk::RamdiskPatchStatus::Patched => {
                    badge_text(
                        ui,
                        &tr!("already-patched"),
                        egui::Color32::from_rgb(230, 170, 40),
                    );
                }
                common::ramdisk::RamdiskPatchStatus::Patchable => {
                    badge_text(
                        ui,
                        &tr!("stock-init-early-present"),
                        egui::Color32::from_rgb(90, 200, 120),
                    );
                }
                common::ramdisk::RamdiskPatchStatus::Unsupported => {
                    badge_text(
                        ui,
                        &tr!("unknown-layout"),
                        egui::Color32::from_rgb(230, 170, 40),
                    );
                }
            }
            if self.dirty {
                ui.separator();
                badge_text(
                    ui,
                    &tr!("unsaved-changes"),
                    egui::Color32::from_rgb(230, 170, 40),
                );
            }
            if !loaded.from_image {
                ui.separator();
                ui.label(egui::RichText::new(&loaded.source_path).weak().monospace());
            }
        });
        ui.add_space(4.0);
    }

    fn browser(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp, loaded: &mut Loaded) {
        ui.horizontal(|ui| {
            ui.label(tr!("filter"));
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr!("filter-path-hint"))
                    .desired_width(220.0),
            );
            if ui.button(tr!("expand-all")).clicked() {
                self.expand = true;
            }
            if ui.button(tr!("collapse-all")).clicked() {
                self.expand = false;
            }
        });
        ui.add_space(2.0);

        let mut clicked: Option<String> = None;
        let filter = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("cpio-tree")
            .max_height(300.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if filter.is_empty() {
                    self.draw_dir(ui, loaded, "", &mut clicked);
                } else {
                    for path in loaded.cpio.entries.keys() {
                        if path.to_lowercase().contains(&filter) {
                            let selected = self.selection.as_deref() == Some(path.as_str());
                            let response = ui.selectable_label(selected, format!("  {path}"));
                            if response.clicked() {
                                clicked = Some(path.clone());
                            }
                            if response.double_clicked() {
                                ui.ctx().copy_text(path.clone());
                            }
                        }
                    }
                }
            });
        if let Some(path) = clicked {
            self.selection = Some(path);
        }

        ui.add_space(6.0);
        self.actions_row(ui, app, loaded);
        ui.add_space(6.0);
        self.detail_panel(ui, loaded);
    }

    #[allow(clippy::only_used_in_recursion)]
    fn draw_dir(
        &self,
        ui: &mut egui::Ui,
        loaded: &Loaded,
        dir: &str,
        clicked: &mut Option<String>,
    ) {
        let Some(names) = loaded.children.get(dir) else {
            return;
        };
        for name in names {
            let full = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let is_dir = loaded
                .cpio
                .entries
                .get(&full)
                .map(|entry| entry.mode & S_IFMT == S_IFDIR)
                .unwrap_or(false)
                || loaded.children.contains_key(&full);
            if is_dir {
                let default_open = self.expand || dir.is_empty();
                egui::CollapsingHeader::new(format!("📁 {name}"))
                    .id_salt(("cpio-dir", full.as_str(), self.expand))
                    .default_open(default_open)
                    .show(ui, |ui| self.draw_dir(ui, loaded, &full, clicked));
            } else {
                let selected = self.selection.as_deref() == Some(full.as_str());
                let response = ui.selectable_label(selected, format!("   {name}"));
                if response.clicked() {
                    *clicked = Some(full.clone());
                }
                if response.double_clicked() {
                    ui.ctx().copy_text(full.clone());
                }
            }
        }
    }

    fn actions_row(&mut self, ui: &mut egui::Ui, app: &mut HaucetApp, loaded: &mut Loaded) {
        let selection = self.selection.clone();
        let busy = self.load_job.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !busy && selection.is_some(),
                    egui::Button::new(tr!("extract-selected")),
                )
                .clicked()
                && let Some(dir) = app.pick_dir(&tr!("choose-extract-directory"))
            {
                let entry = selection.clone().unwrap_or_default();
                let dir = dir.display().to_string();
                let snapshot = loaded.snapshot();
                self.message = None;
                self.load_job = Some(spawn_local(tr!("extract-entry-job"), move || {
                    extract_entries(&snapshot, std::slice::from_ref(&entry), &dir)?;
                    Ok(LocalOutcome::Done(tr!("extracted-entry", "entry" => entry)))
                }));
            }
            if ui
                .add_enabled(!busy, egui::Button::new(tr!("extract-all")))
                .clicked()
                && let Some(dir) = app.pick_dir(&tr!("choose-extract-directory"))
            {
                let paths = loaded.cpio.entries.keys().cloned().collect::<Vec<_>>();
                let dir = dir.display().to_string();
                let snapshot = loaded.snapshot();
                self.message = None;
                self.load_job = Some(spawn_local(tr!("extract-all-job"), move || {
                    let count = extract_entries(&snapshot, &paths, &dir)?;
                    Ok(LocalOutcome::Done(
                        tr!("extracted-all", "count" => count, "directory" => dir),
                    ))
                }));
            }
            if ui
                .add_enabled(
                    selection.is_some(),
                    egui::Button::new(tr!("delete-selected")),
                )
                .clicked()
                && let Some(entry) = selection.clone()
            {
                let is_dir = loaded
                    .cpio
                    .entries
                    .get(&entry)
                    .map(|entry| entry.mode & S_IFMT == S_IFDIR)
                    .unwrap_or(false);
                loaded.cpio.rm(&entry, is_dir);
                loaded.rebuild_tree();
                self.selection = None;
                self.mark_dirty();
                self.message = Some((true, tr!("deleted-entry", "entry" => entry)));
            }
            if ui.button(tr!("add-file")).clicked()
                && let Some(file) = app.pick_file(&tr!("choose-file-to-add"), &[])
            {
                let suggested = file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.pending_add = Some(file.display().to_string());
                self.add_target = suggested;
                self.add_mode = "0750".to_owned();
            }
            if ui.button(tr!("new-directory")).clicked() {
                self.mkdir_target = "new/dir".to_owned();
            }
        });

        if self.pending_add.is_some() {
            ui.add_space(4.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr!("archive-path"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.add_target)
                                .hint_text(tr!("example-bin-init-early"))
                                .desired_width(240.0),
                        );
                        ui.label(tr!("mode"));
                        ui.add(egui::TextEdit::singleline(&mut self.add_mode).desired_width(64.0));
                        if ui.button(tr!("confirm-add")).clicked() {
                            let mode = cpio::parse_cpio_mode(self.add_mode.trim());
                            let target = validate_archive_path(&self.add_target);
                            match (mode, target, self.pending_add.clone()) {
                                (Ok(mode), Ok(target), Some(file)) => {
                                    match loaded.cpio.add(mode, &target, &file) {
                                        Ok(()) => {
                                            loaded.rebuild_tree();
                                            self.mark_dirty();
                                            self.message =
                                                Some((true, tr!("added-file", "file" => file, "target" => target)));
                                            self.pending_add = None;
                                        }
                                        Err(error) => {
                                            self.message =
                                                Some((false, tr!("add-failed", "error" => error.to_string())));
                                        }
                                    }
                                }
                                (Err(error), _, _) => {
                                    self.message = Some((false, tr!("invalid-mode", "error" => error.to_string())));
                                }
                                (_, Err(error), _) => {
                                    self.message = Some((false, error));
                                }
                                (_, _, None) => {
                                    self.message = Some((false, tr!("choose-file-to-add-first")));
                                }
                            }
                        }
                        if ui.button(tr!("cancel")).clicked() {
                            self.pending_add = None;
                        }
                    });
                });
        }
        if !self.mkdir_target.is_empty() {
            ui.add_space(4.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr!("directory-path"));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.mkdir_target)
                                .hint_text(tr!("example-new-dir"))
                                .desired_width(240.0),
                        );
                        if ui.button(tr!("create")).clicked() {
                            match validate_archive_path(&self.mkdir_target) {
                                Ok(target) => {
                                    loaded.cpio.mkdir(0o750, &target);
                                    loaded.rebuild_tree();
                                    self.mark_dirty();
                                    self.message = Some((
                                        true,
                                        tr!("created-directory", "directory" => target),
                                    ));
                                    self.mkdir_target.clear();
                                }
                                Err(error) => {
                                    self.message = Some((false, error));
                                }
                            }
                        }
                    });
                });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy && !loaded.from_image && self.dirty,
                    egui::Button::new(tr!("save-changes")),
                )
                .on_hover_text(tr!("save-source-hint"))
                .clicked()
            {
                let path = loaded.source_path.clone();
                let snapshot = loaded.snapshot();
                let revision = self.revision;
                self.message = None;
                self.load_job = Some(spawn_local(tr!("save-job"), move || {
                    let mut bytes = Vec::new();
                    snapshot.dump_to(&mut bytes)?;
                    std::fs::write(&path, bytes)?;
                    Ok(LocalOutcome::Saved {
                        path,
                        revision,
                        update_source: false,
                    })
                }));
            }
            if ui
                .add_enabled(!busy, egui::Button::new(tr!("save-as")))
                .clicked()
                && let Some(path) = app.pick_save(&tr!("save-cpio-file"), "ramdisk.cpio")
            {
                let path = path.display().to_string();
                let snapshot = loaded.snapshot();
                let revision = self.revision;
                self.message = None;
                self.load_job = Some(spawn_local(tr!("save-as-job"), move || {
                    let mut bytes = Vec::new();
                    snapshot.dump_to(&mut bytes)?;
                    std::fs::write(&path, bytes)?;
                    Ok(LocalOutcome::Saved {
                        path,
                        revision,
                        update_source: true,
                    })
                }));
            }
            if loaded.from_image {
                ui.label(egui::RichText::new(tr!("cpio-from-image-help")).weak());
            }
        });
    }

    fn detail_panel(&self, ui: &mut egui::Ui, loaded: &Loaded) {
        let Some(selection) = &self.selection else {
            return;
        };
        let Some(entry) = loaded.cpio.entries.get(selection) else {
            return;
        };
        ui.add_space(4.0);
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(selection).strong());
                egui::Grid::new("cpio-entry-detail")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        crate::util::kv(ui, &tr!("permissions"), mode_string(entry.mode));
                        crate::util::kv(ui, "uid / gid", format!("{} / {}", entry.uid, entry.gid));
                        crate::util::kv(ui, &tr!("size"), human_size(entry.data.len() as u64));
                    });
            });
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }
}

fn extract_entries(cpio: &Cpio, paths: &[String], dir: &str) -> anyhow::Result<usize> {
    let mut count = 0;
    for path in paths {
        let output = common::fs_util::safe_join(std::path::Path::new(dir), path)
            .map_err(|error| anyhow::anyhow!(tr!("unsafe-cpio-entry", "path" => format!("{path:?}"), "error" => format!("{error:#}"))))?;
        cpio.extract_entry(path, &output.display().to_string())
            .map_err(|error| {
                anyhow::anyhow!(
                    tr!("extract-failed", "path" => path.clone(), "error" => error.to_string())
                )
            })?;
        count += 1;
    }
    Ok(count)
}

fn validate_archive_path(value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(tr!("archive-path-empty"));
    }
    if value.contains('\\') || common::fs_util::safe_join(std::path::Path::new("."), value).is_err()
    {
        return Err(tr!("unsafe-cpio-path", "path" => format!("{value:?}")));
    }
    Ok(cpio::norm_path(value))
}
