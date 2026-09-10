use eframe::egui;
use std::path::Path;

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn hex64(value: u64) -> String {
    format!("0x{value:X}")
}

pub fn open_in_file_manager(path: &Path) {
    let result = open_command(path);
    if let Some(mut command) = result {
        let _ = command.spawn();
    }
}

fn open_command(path: &Path) -> Option<std::process::Command> {
    #[cfg(target_os = "windows")]
    {
        let mut command = std::process::Command::new("explorer");
        if path.is_dir() {
            command.arg(path);
        } else {
            command.arg("/select,").arg(path);
        }
        return Some(command);
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("open");
        if path.is_dir() {
            command.arg(path);
        } else {
            command.arg("-R").arg(path);
        }
        return Some(command);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = std::process::Command::new("xdg-open");
        let target = if path.is_dir() {
            path
        } else {
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(path)
        };
        command.arg(target);
        return Some(command);
    }
    #[allow(unreachable_code)]
    {
        let _ = path;
        None
    }
}

pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.clone().next() == Some('[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn kv(ui: &mut egui::Ui, key: &str, value: impl Into<egui::WidgetText>) {
    ui.label(egui::RichText::new(key).weak());
    ui.label(value);
    ui.end_row();
}

const NOTIFICATION_FONT_SIZE: f32 = 16.0;

pub struct Notifications {
    toasts: egui_notify::Toasts,
    pending: Vec<(egui_notify::ToastLevel, String)>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            toasts: egui_notify::Toasts::default()
                .with_anchor(egui_notify::Anchor::TopRight)
                .with_margin(egui::vec2(12.0, 12.0)),
            pending: Vec::new(),
        }
    }
}

impl Notifications {
    // Enqueue only from events; identical results from separate operations are intentional.
    pub fn push(&mut self, level: egui_notify::ToastLevel, text: String) {
        self.pending.push((level, text));
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        for (level, text) in self.pending.drain(..) {
            let seconds = match level {
                egui_notify::ToastLevel::Error | egui_notify::ToastLevel::Warning => 8,
                _ => 5,
            };
            self.toasts
                .add(egui_notify::Toast::custom(
                    notification_caption(ctx, text),
                    level,
                ))
                .closable(true)
                .duration(std::time::Duration::from_secs(seconds));
        }
        // A solid, zero-blur shadow supplies the outline through egui-notify's
        // public styling API. Resolve its color each frame to follow theme changes.
        self.toasts = std::mem::take(&mut self.toasts).with_shadow(egui::Shadow {
            offset: [0, 0],
            blur: 0,
            spread: 1,
            color: ctx.global_style().visuals.window_stroke.color,
        });
        self.toasts.show(ctx);
    }
}

fn notification_caption(ctx: &egui::Context, text: String) -> egui::RichText {
    // egui-notify otherwise measures captions without wrapping. Keep long paths and
    // command output on screen; HaucetApp retains the complete message in its log.
    let width = (ctx.content_rect().width() - 100.0).clamp(80.0, 420.0);
    let mut job = egui::text::LayoutJob::simple(
        text,
        egui::FontId::proportional(NOTIFICATION_FONT_SIZE),
        egui::Color32::PLACEHOLDER,
        width,
    );
    job.wrap.max_rows = 8;
    let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
    // Keep only the wrapped characters, including the overflow ellipsis. Theme
    // changes rebuild egui's font atlas, invalidating a Galley's cached glyph UVs.
    // RichText lets egui-notify lay out against the current atlas on every frame.
    let wrapped_text = galley
        .rows
        .iter()
        .map(|row| row.glyphs.iter().map(|glyph| glyph.chr).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    egui::RichText::new(wrapped_text).size(NOTIFICATION_FONT_SIZE)
}

pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).strong().size(16.0));
    ui.add_space(2.0);
}

pub fn sibling_output_path(input: &str, fallback_stem: &str, suffix: &str) -> String {
    let path = Path::new(input);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback_stem.to_owned());
    let name = format!("{stem}{suffix}");
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join(&name).display().to_string())
        .unwrap_or(name)
}

pub fn update_derived_path(value: &mut String, previous: &mut Option<String>, next: String) {
    if value.trim().is_empty() || previous.as_deref() == Some(value.as_str()) {
        *value = next.clone();
        *previous = Some(next);
    }
}

pub fn mode_string(mode: u32) -> String {
    use common::formats::cpio::*;
    let type_char = match mode & S_IFMT {
        S_IFDIR => 'd',
        S_IFREG => '-',
        S_IFLNK => 'l',
        S_IFBLK => 'b',
        S_IFCHR => 'c',
        _ => '?',
    };
    let mut out = String::with_capacity(10);
    out.push(type_char);
    for (bit, ch) in [
        (S_IRUSR, 'r'),
        (S_IWUSR, 'w'),
        (S_IXUSR, 'x'),
        (S_IRGRP, 'r'),
        (S_IWGRP, 'w'),
        (S_IXGRP, 'x'),
        (S_IROTH, 'r'),
        (S_IWOTH, 'w'),
        (S_IXOTH, 'x'),
    ] {
        out.push(if mode & bit != 0 { ch } else { '-' });
    }
    out
}
