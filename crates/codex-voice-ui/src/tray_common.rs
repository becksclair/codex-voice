//! Shared tray contract used by every platform tray implementation.
//!
//! The three platform tray modules (`linux_tray`, `macos_tray`, `windows_tray`)
//! present an identical command enum, menu-item identifiers, and status-icon
//! rendering. Those pieces live here so a menu change is made once rather than
//! copied three times.

use codex_voice_core::DictationState;
#[cfg(not(target_os = "linux"))]
use std::collections::HashMap;
#[cfg(not(target_os = "linux"))]
use tray_icon::Icon;

// The menu-item id constants identify tray-menu activations on platforms whose
// tray backend is id-based (`tray-icon` on macOS/Windows). The Linux backend
// (ksni) is closure-based and never references them, so they are gated off the
// Linux build to avoid dead-code warnings there.
#[cfg(not(target_os = "linux"))]
pub const MENU_STATUS: &str = "status";
#[cfg(not(target_os = "linux"))]
pub const MENU_TEST_RECORDING: &str = "test-recording";
#[cfg(not(target_os = "linux"))]
pub const MENU_SPEAK_TEXT: &str = "speak-text";
#[cfg(not(target_os = "linux"))]
pub const MENU_SETTINGS: &str = "settings";
#[cfg(not(target_os = "linux"))]
pub const MENU_LOGS: &str = "logs";
#[cfg(not(target_os = "linux"))]
pub const MENU_DIAGNOSTICS: &str = "diagnostics";
#[cfg(not(target_os = "linux"))]
pub const MENU_QUIT: &str = "quit";
pub const ICON_SIZE: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    StartTestRecording,
    SpeakText(String),
    PlayLastSpeech,
    OpenLogs,
    RunDiagnostics,
    Quit,
}

/// Errors raised while constructing or driving a platform status tray.
///
/// Each message payload already carries a descriptive prefix at its call site;
/// the variant classifies the failure so callers can distinguish a tray/menu
/// initialization failure from an icon-rendering failure or a lost background
/// thread without string matching.
#[derive(Debug, Clone, thiserror::Error)]
pub enum UiError {
    /// The platform tray, menu, or settings window failed to initialize.
    #[error("{0}")]
    TrayInit(String),
    /// A status icon could not be rendered.
    #[error("{0}")]
    Icon(String),
    /// The tray background thread stopped or a channel closed unexpectedly.
    #[error("{0}")]
    EventLoop(String),
}

/// Renders the status icon for `state` as a 32x32 RGBA buffer.
///
/// This is the platform-neutral pixel source: a filled circle in the
/// per-state color over a transparent background. Every platform tray converts
/// it into its own icon type (`tray_icon::Icon` on macOS/Windows, `ksni::Icon`
/// on Linux). Keeping the drawing here means the visual is defined once.
pub fn icon_rgba_for_state(state: &DictationState) -> Vec<u8> {
    let color = match state {
        DictationState::Idle => [0x5c, 0x66, 0x70, 0xff],
        DictationState::Recording => [0xdb, 0x36, 0x36, 0xff],
        DictationState::Transcribing => [0x2b, 0x7f, 0xd3, 0xff],
        DictationState::Inserting => [0xf2, 0xb8, 0x4b, 0xff],
        DictationState::Error(_) => [0xcc, 0x24, 0x1d, 0xff],
    };

    let mut rgba = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    let radius = (ICON_SIZE as f32) / 2.0 - 2.0;
    let center = (ICON_SIZE as f32 - 1.0) / 2.0;

    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let alpha = if (dx * dx + dy * dy).sqrt() <= radius {
                color[3]
            } else {
                0
            };
            rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }

    rgba
}

#[cfg(not(target_os = "linux"))]
pub fn build_icon_cache() -> Result<HashMap<DictationState, Icon>, UiError> {
    use codex_voice_core::DictationState::*;
    let mut cache = HashMap::new();
    for state in [
        Idle,
        Recording,
        Transcribing,
        Inserting,
        Error(String::new()),
    ] {
        let icon = build_icon_for_state(&state)?;
        cache.insert(state, icon);
    }
    Ok(cache)
}

#[cfg(not(target_os = "linux"))]
pub fn icon_for_state(cache: &HashMap<DictationState, Icon>, state: &DictationState) -> Icon {
    let lookup = match state {
        DictationState::Error(_) => DictationState::Error(String::new()),
        _ => state.clone(),
    };
    cache
        .get(&lookup)
        .cloned()
        .or_else(|| cache.get(&DictationState::Error(String::new())).cloned())
        .expect("icon cache contains all states")
}

#[cfg(not(target_os = "linux"))]
pub fn build_icon_for_state(state: &DictationState) -> Result<Icon, UiError> {
    Icon::from_rgba(icon_rgba_for_state(state), ICON_SIZE, ICON_SIZE)
        .map_err(|error| UiError::Icon(format!("failed to build tray icon: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time tripwire for the shared `StatusTray` method surface.
    ///
    /// `StatusTray` itself is not hoisted (platform internals differ), but every
    /// platform must expose the same method signatures. Coercing each method to
    /// a function pointer here fails the build if a method is dropped, renamed,
    /// or has its signature changed. The `start` config parameter is inferred
    /// (`_`) because its concrete type is platform-specific.
    #[test]
    fn status_tray_surface_contract() {
        let _start: fn(crate::UiStatus, _) -> Result<crate::StatusTray, UiError> =
            crate::StatusTray::start;
        let _update: fn(&crate::StatusTray, crate::UiStatus) = crate::StatusTray::update;
        let _try_recv: fn(&crate::StatusTray) -> Option<UiCommand> =
            crate::StatusTray::try_recv_command;
        let _status_sender: fn(&crate::StatusTray) -> std::sync::mpsc::Sender<crate::UiStatus> =
            crate::StatusTray::status_sender;
    }

    fn pixel_at(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
        let idx = ((y * ICON_SIZE + x) * 4) as usize;
        [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
    }

    #[test]
    fn icon_rgba_for_state_paints_center_and_leaves_corner_transparent() {
        let cases = [
            (DictationState::Idle, [0x5c, 0x66, 0x70]),
            (DictationState::Recording, [0xdb, 0x36, 0x36]),
            (DictationState::Transcribing, [0x2b, 0x7f, 0xd3]),
            (DictationState::Inserting, [0xf2, 0xb8, 0x4b]),
            (DictationState::Error(String::new()), [0xcc, 0x24, 0x1d]),
        ];

        for (state, rgb) in cases {
            let rgba = icon_rgba_for_state(&state);
            assert_eq!(rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);

            // The center pixel is inside the circle: opaque, in the state color.
            let center = pixel_at(&rgba, ICON_SIZE / 2, ICON_SIZE / 2);
            assert_eq!(
                center,
                [rgb[0], rgb[1], rgb[2], 0xff],
                "center for {state:?}"
            );

            // The corner pixel is outside the circle: fully transparent. The RGB
            // channels still carry the state color; only alpha distinguishes it.
            let corner = pixel_at(&rgba, 0, 0);
            assert_eq!(
                corner,
                [rgb[0], rgb[1], rgb[2], 0x00],
                "corner for {state:?}"
            );
        }
    }

    #[test]
    fn ui_error_display_preserves_message() {
        assert_eq!(
            UiError::TrayInit("failed to initialize GTK: boom".to_string()).to_string(),
            "failed to initialize GTK: boom"
        );
        assert_eq!(
            UiError::Icon("failed to build tray icon: bad rgba".to_string()).to_string(),
            "failed to build tray icon: bad rgba"
        );
        assert_eq!(
            UiError::EventLoop("tray thread stopped during startup".to_string()).to_string(),
            "tray thread stopped during startup"
        );
    }
}
