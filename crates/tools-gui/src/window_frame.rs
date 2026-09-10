use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DwmSetWindowAttribute,
};

pub fn apply(window: &impl HasWindowHandle, visuals: &egui::Visuals) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let hwnd = handle.hwnd.get() as HWND;
    for (attribute, color) in [
        (DWMWA_CAPTION_COLOR, visuals.panel_fill),
        (DWMWA_BORDER_COLOR, visuals.panel_fill),
        (DWMWA_TEXT_COLOR, visuals.text_color()),
    ] {
        let color =
            u32::from(color.r()) | (u32::from(color.g()) << 8) | (u32::from(color.b()) << 16);
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                attribute as u32,
                (&color as *const u32).cast(),
                std::mem::size_of_val(&color) as u32,
            );
        }
    }
}
