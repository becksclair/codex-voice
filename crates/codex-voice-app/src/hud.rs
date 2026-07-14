//! Transient desktop notifications ("HUD") mirroring the old per-platform
//! tray shims: `notify-send` on Linux, `osascript` on macOS, no-op on
//! Windows. Ported verbatim from `crates/codex-voice-ui/src/linux_tray.rs`
//! and `macos_tray.rs` (deleted when the crate was retired).

#[cfg(any(target_os = "linux", target_os = "macos", test))]
use codex_voice_core::DictationState;

use crate::status::UiStatus;

/// Shows (or updates/clears) the transient notification for `status`. Skips
/// `Idle` entirely. Enqueues to a single worker thread and returns
/// immediately, so callers never block on the notification helper and the
/// replace-id/dedupe state has exactly one owner (no cross-thread races).
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn show(status: &UiStatus) {
    use std::sync::mpsc::{self, Sender};
    use std::sync::OnceLock;

    static SENDER: OnceLock<Sender<UiStatus>> = OnceLock::new();
    let sender = SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<UiStatus>();
        std::thread::spawn(move || {
            let mut worker = HudWorker::default();
            while let Ok(status) = rx.recv() {
                worker.handle(status);
            }
        });
        tx
    });
    let _ = sender.send(status.clone());
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn show(_status: &UiStatus) {
    // No HUD backend on this platform.
}

/// Single-threaded owner of the notification state; only reachable from the
/// worker thread, so its fields need no synchronization.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Default)]
struct HudWorker {
    #[cfg(target_os = "linux")]
    replace_id: Option<String>,
    last_message: Option<String>,
    unavailable: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl HudWorker {
    fn handle(&mut self, status: UiStatus) {
        if status.state == DictationState::Idle {
            self.last_message = None;
            return;
        }
        if self.unavailable {
            return;
        }
        if self.last_message.as_deref() == Some(status.message.as_str()) {
            return;
        }
        self.dispatch(status);
    }

    #[cfg(target_os = "linux")]
    fn dispatch(&mut self, status: UiStatus) {
        let args = notify_send_args(&status, self.replace_id.as_deref());
        match std::process::Command::new("notify-send")
            .args(&args)
            .output()
        {
            Ok(output) if output.status.success() => {
                let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !id.is_empty() {
                    self.replace_id = Some(id);
                }
                self.last_message = Some(status.message);
            }
            _ => {
                self.unavailable = true;
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn dispatch(&mut self, status: UiStatus) {
        let script = osascript_script(&status);
        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            Ok(output) if output.status.success() => {
                self.last_message = Some(status.message);
            }
            _ => {
                self.unavailable = true;
            }
        }
    }
}

/// Builds the `notify-send` argument list for `status`. Ported from
/// `HudWindow::update` in the deleted `linux_tray.rs`: `--transient`,
/// per-state urgency/expire-time, and an optional `--replace-id` to update
/// the previous notification in place.
#[cfg(any(target_os = "linux", test))]
fn notify_send_args(status: &UiStatus, replace_id: Option<&str>) -> Vec<String> {
    let timeout_ms = match status.state {
        DictationState::Recording => "60000",
        DictationState::Error(_) => "8000",
        _ => "2500",
    };
    let urgency = match status.state {
        DictationState::Error(_) => "critical",
        _ => "low",
    };

    let mut args = vec![
        "--print-id".to_string(),
        "--transient".to_string(),
        "--app-name=Codex Voice".to_string(),
        "--category=status".to_string(),
        "--urgency".to_string(),
        urgency.to_string(),
        "--expire-time".to_string(),
        timeout_ms.to_string(),
    ];
    if let Some(id) = replace_id {
        args.push("--replace-id".to_string());
        args.push(id.to_string());
    }
    args.push("Codex Voice".to_string());
    args.push(status.message.clone());
    args
}

/// Builds the `osascript` notification script for `status`. Ported from
/// `HudNotifier::update` in the deleted `macos_tray.rs`.
#[cfg(any(target_os = "macos", test))]
fn osascript_script(status: &UiStatus) -> String {
    format!(
        "display notification \"{}\" with title \"Codex Voice – {}\" sound name \"Funk\"",
        status.message.replace('"', "\\\""),
        status.title()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_send_args_uses_recording_timeout_and_low_urgency() {
        let status = UiStatus::new(DictationState::Recording, "Listening...");
        let args = notify_send_args(&status, None);
        assert!(args.contains(&"--expire-time".to_string()));
        assert!(args.contains(&"60000".to_string()));
        assert!(args.contains(&"low".to_string()));
        assert!(args.contains(&"Codex Voice".to_string()));
        assert!(args.contains(&"Listening...".to_string()));
        assert!(!args.iter().any(|a| a == "--replace-id"));
    }

    #[test]
    fn notify_send_args_uses_error_timeout_and_critical_urgency() {
        let status = UiStatus::new(DictationState::Error("boom".into()), "Error: boom");
        let args = notify_send_args(&status, None);
        assert!(args.contains(&"8000".to_string()));
        assert!(args.contains(&"critical".to_string()));
    }

    #[test]
    fn notify_send_args_uses_default_timeout_for_other_states() {
        let status = UiStatus::new(DictationState::Transcribing, "Transcribing...");
        let args = notify_send_args(&status, None);
        assert!(args.contains(&"2500".to_string()));
        assert!(args.contains(&"low".to_string()));
    }

    #[test]
    fn notify_send_args_includes_replace_id_when_present() {
        let status = UiStatus::new(DictationState::Recording, "Listening...");
        let args = notify_send_args(&status, Some("42"));
        let idx = args
            .iter()
            .position(|a| a == "--replace-id")
            .expect("replace-id flag present");
        assert_eq!(args[idx + 1], "42");
    }

    #[test]
    fn osascript_script_escapes_quotes_and_includes_title() {
        let status = UiStatus::new(DictationState::Inserting, "Inserted \"hello\"");
        let script = osascript_script(&status);
        assert!(script.contains("Inserted \\\"hello\\\""));
        assert!(script.contains("Codex Voice – Inserting"));
        assert!(script.contains("sound name \"Funk\""));
    }
}
