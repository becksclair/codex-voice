//! UI status model and status-icon rendering shared across tray/window code.

use codex_voice_core::{AppEvent, DictationState, InsertMethod};

pub const ICON_SIZE: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiStatus {
    pub state: DictationState,
    pub message: String,
}

impl UiStatus {
    pub fn idle() -> Self {
        Self::new(DictationState::Idle, "Ready")
    }

    pub fn new(state: DictationState, message: impl Into<String>) -> Self {
        Self {
            state,
            message: message.into(),
        }
    }

    pub fn from_app_event(event: &AppEvent) -> Option<Self> {
        match event {
            AppEvent::StateChanged(state) => Some(Self::for_state(state.clone())),
            AppEvent::RecordingDiscarded { .. } => {
                Some(Self::new(DictationState::Idle, "Recording too short"))
            }
            AppEvent::TranscriptReady { chars } => Some(Self::new(
                DictationState::Transcribing,
                format!("Transcript ready: {chars} chars"),
            )),
            AppEvent::Inserted(report) => Some(Self::new(
                DictationState::Inserting,
                format!("Inserted via {}", insert_method_label(report.method)),
            )),
            AppEvent::Error { message, .. } => Some(Self::new(
                DictationState::Error(message.clone()),
                format!("Error: {message}"),
            )),
            AppEvent::RecordingDeleted { .. } => None,
        }
    }

    pub fn tray_label(&self) -> String {
        format!("Codex Voice: {}", self.message)
    }

    /// Used by the macOS HUD notification title; other platforms build their
    /// notifications from `message` alone.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn title(&self) -> &'static str {
        match self.state {
            DictationState::Idle => "Ready",
            DictationState::Recording => "Listening",
            DictationState::Transcribing => "Transcribing",
            DictationState::Inserting => "Inserting",
            DictationState::Error(_) => "Error",
        }
    }

    fn for_state(state: DictationState) -> Self {
        let message = match &state {
            DictationState::Idle => "Ready".to_string(),
            DictationState::Recording => "Listening...".to_string(),
            DictationState::Transcribing => "Transcribing...".to_string(),
            DictationState::Inserting => "Inserting...".to_string(),
            DictationState::Error(message) => format!("Error: {message}"),
        };
        Self::new(state, message)
    }
}

fn insert_method_label(method: InsertMethod) -> &'static str {
    match method {
        InsertMethod::Accessibility => "accessibility",
        InsertMethod::ClipboardPaste => "clipboard paste",
        InsertMethod::PortalPaste => "portal paste",
        InsertMethod::SendInputPaste => "SendInput paste",
        InsertMethod::UiAutomationValuePattern => "UI Automation value pattern",
    }
}

/// Renders the status icon for `state` as a 32x32 RGBA buffer.
///
/// This is the platform-neutral pixel source: a filled circle in the
/// per-state color over a transparent background. Tauri's tray icon consumes
/// this buffer directly, so the visual is defined once here.
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_voice_core::{InsertMethod, InsertReport};

    #[test]
    fn maps_core_state_to_status_label() {
        let status = UiStatus::from_app_event(&AppEvent::StateChanged(DictationState::Recording))
            .expect("state changes should produce UI status");

        assert_eq!(status.title(), "Listening");
        assert_eq!(status.tray_label(), "Codex Voice: Listening...");
    }

    #[test]
    fn maps_insert_report_to_human_status() {
        let status = UiStatus::from_app_event(&AppEvent::Inserted(InsertReport {
            method: InsertMethod::PortalPaste,
            restored_clipboard: true,
        }))
        .expect("insert reports should produce UI status");

        assert_eq!(status.message, "Inserted via portal paste");
    }

    #[test]
    fn skips_internal_file_cleanup_events() {
        let status = UiStatus::from_app_event(&AppEvent::RecordingDeleted {
            path: "recording.wav".into(),
        });

        assert_eq!(status, None);
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
}
