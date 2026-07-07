use codex_voice_core::{AppEvent, DictationState, InsertMethod};

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

mod tray_common;

pub use tray_common::UiCommand;

#[cfg(target_os = "linux")]
mod linux_tray;

#[cfg(target_os = "linux")]
pub use linux_tray::{LinuxUiConfig, StatusTray};

#[cfg(target_os = "windows")]
mod windows_tray;

#[cfg(target_os = "windows")]
pub use windows_tray::{StatusTray, WindowsUiConfig};

#[cfg(target_os = "macos")]
mod macos_tray;

#[cfg(target_os = "macos")]
pub use macos_tray::{MacOSUiConfig, StatusTray};

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
}
