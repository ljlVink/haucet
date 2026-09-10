//! Native window transparency and frame colors.

pub const TRANSPARENCY_SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

pub fn apply_transparency(cc: &eframe::CreationContext<'_>) -> bool {
    #[cfg(windows)]
    {
        window_vibrancy::apply_acrylic(cc, None).is_ok()
    }

    #[cfg(target_os = "macos")]
    {
        window_vibrancy::apply_vibrancy(
            cc,
            window_vibrancy::NSVisualEffectMaterial::UnderWindowBackground,
            Some(window_vibrancy::NSVisualEffectState::FollowsWindowActiveState),
            None,
        )
        .is_ok()
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = cc;
        false
    }
}

#[cfg(windows)]
pub fn apply_frame_colors(
    window: &impl raw_window_handle::HasWindowHandle,
    visuals: &eframe::egui::Visuals,
) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DwmSetWindowAttribute,
    };

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
