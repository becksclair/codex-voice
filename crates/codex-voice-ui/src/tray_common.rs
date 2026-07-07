//! Shared tray contract used by every platform tray implementation.
//!
//! The three platform tray modules (`linux_tray`, `macos_tray`, `windows_tray`)
//! present an identical command enum, menu-item identifiers, and status-icon
//! rendering. Those pieces live here so a menu change is made once rather than
//! copied three times.

use codex_voice_core::DictationState;
use std::collections::HashMap;
use tray_icon::Icon;

pub const MENU_STATUS: &str = "status";
pub const MENU_TEST_RECORDING: &str = "test-recording";
pub const MENU_SPEAK_TEXT: &str = "speak-text";
pub const MENU_SETTINGS: &str = "settings";
pub const MENU_LOGS: &str = "logs";
pub const MENU_DIAGNOSTICS: &str = "diagnostics";
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

pub fn build_icon_cache() -> Result<HashMap<DictationState, Icon>, String> {
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

pub fn build_icon_for_state(state: &DictationState) -> Result<Icon, String> {
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

    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE)
        .map_err(|error| format!("failed to build tray icon: {error}"))
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
        let _start: fn(crate::UiStatus, _) -> Result<crate::StatusTray, String> =
            crate::StatusTray::start;
        let _update: fn(&crate::StatusTray, crate::UiStatus) = crate::StatusTray::update;
        let _try_recv: fn(&crate::StatusTray) -> Option<UiCommand> =
            crate::StatusTray::try_recv_command;
        let _status_sender: fn(&crate::StatusTray) -> std::sync::mpsc::Sender<crate::UiStatus> =
            crate::StatusTray::status_sender;
    }
}
