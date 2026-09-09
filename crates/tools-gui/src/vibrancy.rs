pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

pub fn apply(cc: &eframe::CreationContext<'_>) -> bool {
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
