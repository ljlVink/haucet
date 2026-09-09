#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;

mod app;
mod detect;
mod fastboot_memory;
mod fonts;
mod job;
mod pages;
mod settings;
mod util;
mod vibrancy;
mod worker;

use app::HaucetApp;
use eframe::egui;
use std::sync::Arc;

fn main() -> eframe::Result<()> {
    i18n::init_from_env();
    if worker::is_worker_mode() {
        std::process::exit(worker::run_worker());
    }

    let settings = settings::Settings::load();
    i18n::set_language(settings.language);
    let (icon, logo_rgba) = load_logo();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1400.0, 1000.0])
        .with_min_inner_size([980.0, 640.0])
        .with_title(tr!("app-title-idle"))
        .with_transparent(settings.transparent_window)
        .with_decorations(true);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Haucet",
        options,
        Box::new(move |cc| {
            let font_loaded = fonts::install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(HaucetApp::new(
                cc,
                settings,
                font_loaded,
                logo_rgba,
            )))
        }),
    )
}

type LogoData = (Vec<u8>, [usize; 2]);

fn load_logo() -> (Option<egui::IconData>, Option<LogoData>) {
    let bytes = include_bytes!("../../../assets/logo-icon.png");
    let Ok(image) = image::load_from_memory_with_format(bytes, image::ImageFormat::Png) else {
        return (None, None);
    };
    let rgba = image.to_rgba8();
    let raw = rgba.as_raw();
    let raw_width = rgba.width();
    let height = rgba.height();
    if raw_width == 0 || height == 0 {
        return (None, None);
    }
    let width = (raw_width + 3) & !3;
    let mut pixels = vec![0_u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..raw_width {
            let src = ((y * raw_width + x) * 4) as usize;
            let dst = ((y * width + x) * 4) as usize;
            pixels[dst..dst + 4].copy_from_slice(&raw[src..src + 4]);
        }
    }
    let icon = egui::IconData {
        rgba: pixels.clone(),
        width,
        height,
    };
    (
        Some(icon),
        Some((pixels, [width as usize, height as usize])),
    )
}
