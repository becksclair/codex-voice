//! Cross-platform application orchestration.
//!
//! The shared select loop, tray helpers, and status plumbing live here so they
//! can be unit-tested independent of the platform tray/window layer. `run()`
//! in `main.rs` builds the Tauri app, wires [`crate::tray::TauriTray`] into
//! [`TrayHandle`] and [`crate::windows::DesktopWindows`] into
//! [`crate::tray::AppWindows`], and calls into [`run_app`] the way the old
//! per-platform shims did.

use anyhow::{Context, Result};
use codex_voice_core::DictationState;
use codex_voice_core::{AppEvent, HotkeyEvent, SelectedTextReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

use crate::doctor;
use crate::logging;
use crate::status::UiStatus;
use crate::tray::{AppWindows, UiCommand};

/// Abstraction over the platform tray so the run loop can be driven by a test
/// double. [`crate::tray::TauriTray`] is the concrete Tauri-backed
/// implementation.
pub trait TrayHandle: Send {
    fn try_recv_command(&self) -> Option<UiCommand>;
    fn update(&self, status: UiStatus);
    fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus>;
}

/// Everything a platform shim must supply to run the shared application loop.
///
/// The dictation engine is spawned by the shim (via `run_engine_loop`) before
/// building this struct; the loop only needs the channels to forward hotkeys to
/// it and to receive app events back.
pub struct PlatformParts<R>
where
    R: SelectedTextReader + Send + Sync + 'static,
{
    pub hotkey_rx: mpsc::Receiver<HotkeyEvent>,
    pub app_rx: mpsc::Receiver<AppEvent>,
    pub engine_tx: mpsc::Sender<HotkeyEvent>,
    pub tray: Option<Box<dyn TrayHandle>>,
    pub reader: Arc<R>,
    pub windows: Option<Arc<dyn AppWindows>>,
    pub banner: String,
}

/// Drives the shared select-loop: forwards hotkeys to the engine, mirrors app
/// events onto the tray, and services tray commands. Returns when the tray
/// requests quit or all event sources close.
pub async fn run_app<R>(parts: PlatformParts<R>) -> Result<()>
where
    R: SelectedTextReader + Send + Sync + 'static,
{
    let PlatformParts {
        mut hotkey_rx,
        mut app_rx,
        engine_tx,
        tray,
        reader,
        windows,
        banner,
    } = parts;

    println!("{banner}");

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => {
                match event {
                    HotkeyEvent::SpeakSelection => {
                        let reader = reader.clone();
                        let windows = windows.clone();
                        spawn_status_task(status_sender_for_tray(tray.as_deref()), &tray_busy, move |status_tx| {
                            run_speak_selection(status_tx, reader, windows)
                        });
                    }
                    other => { let _ = engine_tx.try_send(other); }
                }
            },
            Some(event) = app_rx.recv() => {
                if let Some(tray) = tray.as_deref() {
                    if let Some(status) = UiStatus::from_app_event(&event) {
                        tray.update(status);
                    }
                }
                print_app_event(event);
            }
            _ = tray_poll.tick() => {
                if let Some(tray) = tray.as_deref() {
                    while let Some(command) = tray.try_recv_command() {
                        match command {
                            UiCommand::StartTestRecording => {
                                spawn_tray_task(tray, &tray_busy, run_tray_test_recording);
                            }
                            UiCommand::OpenLogs => open_tray_logs(tray),
                            UiCommand::RunDiagnostics => {
                                spawn_tray_task(tray, &tray_busy, run_tray_diagnostics);
                            }
                            UiCommand::Quit => return Ok(()),
                        }
                    }
                }
            }
            else => break,
        }
    }
    Ok(())
}

fn print_app_event(event: AppEvent) {
    match event {
        AppEvent::TranscriptReady { chars } => {
            tracing::info!(target: "codex_voice_app", chars, "transcript ready");
            let _ = logging::append_log_line("transcript ready");
            println!("transcript ready: {chars} chars");
        }
        AppEvent::Inserted(report) => {
            tracing::info!(
                target: "codex_voice_app",
                method = ?report.method,
                restored_clipboard = report.restored_clipboard,
                "inserted transcript"
            );
            let _ = logging::append_log_line("inserted transcript");
            println!("inserted via {:?}", report.method);
        }
        AppEvent::Error { stage, message: _ } => {
            tracing::error!(target: "codex_voice_app", stage = %stage.label(), "app event error");
            let _ = logging::append_log_line(format!("dictation error: {}", stage.label()));
            println!("dictation error occurred; see logs for details");
        }
        other => {
            tracing::debug!(target: "codex_voice_app", event = ?other, "app event");
            println!("{other:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tray helpers (cross-platform)
// ---------------------------------------------------------------------------

fn spawn_tray_task<F, Fut>(tray: &dyn TrayHandle, busy: &Arc<AtomicBool>, task: F)
where
    F: FnOnce(std::sync::mpsc::Sender<UiStatus>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    spawn_status_task(tray.status_sender(), busy, task);
}

fn spawn_status_task<F, Fut>(
    status_tx: std::sync::mpsc::Sender<UiStatus>,
    busy: &Arc<AtomicBool>,
    task: F,
) where
    F: FnOnce(std::sync::mpsc::Sender<UiStatus>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    if busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        let busy = busy.clone();
        tokio::spawn(async move {
            let _guard = TrayBusyGuard(busy);
            task(status_tx).await;
        });
    }
}

fn status_sender_for_tray(tray: Option<&dyn TrayHandle>) -> std::sync::mpsc::Sender<UiStatus> {
    tray.map(TrayHandle::status_sender)
        .unwrap_or_else(|| std::sync::mpsc::channel().0)
}

struct TrayBusyGuard(Arc<AtomicBool>);

impl Drop for TrayBusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Reads the current selection and reports it back via the tray status
/// channel. On success, stores the selection as a one-shot server intent and
/// opens the main window with its `#intent` identifier; speech synthesis and
/// playback happen behind that window.
async fn run_speak_selection<R>(
    status_tx: std::sync::mpsc::Sender<UiStatus>,
    reader: Arc<R>,
    windows: Option<Arc<dyn AppWindows>>,
) where
    R: SelectedTextReader + Send + Sync + 'static,
{
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Reading selected text..."),
    );
    match reader.selected_text().await {
        Ok(selection) if selection.text.trim().is_empty() => {
            tracing::info!("selected text is empty or whitespace-only; nothing to speak");
            let _ = logging::append_log_line("selected text was whitespace-only; ignored");
            set_tray_status(
                &status_tx,
                UiStatus::new(
                    DictationState::Error("selection is empty".into()),
                    "No selected text",
                ),
            );
        }
        Ok(selection) => {
            tracing::info!(
                chars = selection.chars,
                restored_clipboard = selection.restored_clipboard,
                "selected text captured for speech"
            );
            let _ = logging::append_log_line(format!(
                "selected text captured for speech: {} chars restored_clipboard={}",
                selection.chars, selection.restored_clipboard
            ));
            match windows.as_deref() {
                Some(windows) => match windows.open_main_with_speak(selection.text).await {
                    Ok(()) => {
                        let message = format!("Selected text captured: {} chars", selection.chars);
                        set_tray_status(&status_tx, UiStatus::new(DictationState::Idle, message));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to hand selected text to the speak window");
                        set_tray_status(
                            &status_tx,
                            UiStatus::new(
                                DictationState::Error("speak window unavailable".into()),
                                "Could not open selected text",
                            ),
                        );
                    }
                },
                None => {
                    tracing::warn!(
                        "no window layer available; cannot open Speak Text with the captured selection"
                    );
                    let _ = logging::append_log_line(
                        "selected text captured but no window layer is available",
                    );
                    set_tray_status(
                        &status_tx,
                        UiStatus::new(
                            DictationState::Idle,
                            format!(
                                "Selected text captured: {} chars (no window available)",
                                selection.chars
                            ),
                        ),
                    );
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "selected text capture failed");
            let _ = logging::append_log_line(format!("selected text capture failed: {error}"));
            set_tray_status(
                &status_tx,
                UiStatus::new(DictationState::Error(error.to_string()), "No selected text"),
            );
        }
    }
}

async fn run_tray_test_recording(status_tx: std::sync::mpsc::Sender<UiStatus>) {
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Recording, "Running test recording..."),
    );
    match run_test_recording().await {
        Ok(message) => {
            set_tray_status(&status_tx, UiStatus::new(DictationState::Idle, message));
        }
        Err(error) => {
            tracing::warn!(%error, "tray test recording failed");
            let _ = logging::append_log_line(format!("test recording failed: {error:#}"));
            set_tray_error(&status_tx, "Test recording failed", &error);
        }
    }
}

async fn run_test_recording() -> Result<String> {
    tracing::info!("starting tray test recording");
    let _ = logging::append_log_line("starting test recording");
    let (recording, size) = doctor::capture_audio_sample(Duration::from_secs(2)).await?;
    let duration_ms = recording.duration.as_millis();
    if let Err(error) = tokio::fs::remove_file(&recording.path).await {
        tracing::warn!(%error, path = %recording.path.display(), "failed to delete temp recording");
    }
    let message = format!("Test recording ok: {} ms, {size} bytes", duration_ms);
    tracing::info!(duration_ms, bytes = size, "test recording ok");
    let _ = logging::append_log_line(format!("test recording ok: {duration_ms} ms, {size} bytes"));
    Ok(message)
}

fn set_tray_status(status_tx: &std::sync::mpsc::Sender<UiStatus>, status: UiStatus) {
    let _ = status_tx.send(status);
}

fn open_tray_logs(tray: &dyn TrayHandle) {
    if let Err(error) = open_logs() {
        tracing::warn!(%error, "failed to open logs");
        let status_tx = tray.status_sender();
        set_tray_error(&status_tx, "Open logs failed", &error);
    }
}

fn set_tray_error(
    status_tx: &std::sync::mpsc::Sender<UiStatus>,
    prefix: &str,
    error: &anyhow::Error,
) {
    let message = format!("{prefix}: {error:#}");
    set_tray_status(
        status_tx,
        UiStatus::new(DictationState::Error(error.to_string()), message),
    );
}

// ---------------------------------------------------------------------------
// Platform-specific tray actions
//
// The bodies below are identical except for the one platform detail that
// genuinely differs, so only that detail is cfg-gated.
// ---------------------------------------------------------------------------

/// Opens the log file with the platform's default file/URL opener. Only the
/// launcher command differs between platforms.
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn open_logs() -> Result<()> {
    let path = logging::ensure_log_file()?;
    tracing::info!(path = %path.display(), "opening log file");
    let _ = logging::append_log_line(format!("opening log file: {}", path.display()));

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(&path);
        cmd
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut cmd = std::process::Command::new("open");
        cmd.arg(&path);
        cmd
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/c", "start", "", &path.to_string_lossy()]);
        cmd
    };

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn open_logs() -> Result<()> {
    anyhow::bail!("open_logs is not implemented for this platform")
}

#[cfg(target_os = "linux")]
async fn run_tray_diagnostics(status_tx: std::sync::mpsc::Sender<UiStatus>) {
    let _ = logging::append_log_line("running portal diagnostics");
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Running diagnostics..."),
    );
    match doctor::doctor_portals().await {
        Ok(()) => {
            let _ = logging::append_log_line("portal diagnostics complete");
            set_tray_status(
                &status_tx,
                UiStatus::new(DictationState::Idle, "Diagnostics complete"),
            );
        }
        Err(error) => {
            tracing::warn!(%error, "tray diagnostics failed");
            let _ = logging::append_log_line(format!("portal diagnostics failed: {error:#}"));
            set_tray_error(&status_tx, "Diagnostics failed", &error);
        }
    }
}

/// Windows and macOS tray diagnostics are non-interactive in v1: the main
/// hotkey service is already running and interactive tests are Linux-only
/// (portal-based). Full diagnostics remain available via the CLI. The only
/// per-platform difference is the log label.
#[cfg(any(target_os = "windows", target_os = "macos"))]
async fn run_tray_diagnostics(status_tx: std::sync::mpsc::Sender<UiStatus>) {
    #[cfg(target_os = "windows")]
    const PLATFORM: &str = "windows";
    #[cfg(target_os = "macos")]
    const PLATFORM: &str = "macos";

    let _ = logging::append_log_line(format!("running {PLATFORM} diagnostics"));
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Running diagnostics..."),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = logging::append_log_line(format!("{PLATFORM} diagnostics complete"));
    set_tray_status(
        &status_tx,
        UiStatus::new(
            DictationState::Idle,
            "Diagnostics complete — use CLI for full tests",
        ),
    );
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
async fn run_tray_diagnostics(_status_tx: std::sync::mpsc::Sender<UiStatus>) {
    // unreachable on unsupported platforms because run() bails early
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_voice_core::{PlatformError, PlatformResult, SelectedText};
    use std::sync::mpsc as std_mpsc;
    use std::time::Duration;

    /// Test double for [`TrayHandle`]: commands are fed from a std channel and
    /// status updates are captured for assertions.
    struct FakeTray {
        commands: std_mpsc::Receiver<UiCommand>,
        status_tx: std_mpsc::Sender<UiStatus>,
    }

    impl TrayHandle for FakeTray {
        fn try_recv_command(&self) -> Option<UiCommand> {
            self.commands.try_recv().ok()
        }
        fn update(&self, status: UiStatus) {
            let _ = self.status_tx.send(status);
        }
        fn status_sender(&self) -> std_mpsc::Sender<UiStatus> {
            self.status_tx.clone()
        }
    }

    /// Test double for [`AppWindows`] that records the last `open_main_with_speak`
    /// call instead of touching a real window.
    #[derive(Default)]
    struct FakeWindows {
        speak_text: std::sync::Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl AppWindows for FakeWindows {
        fn open_main(&self) {}
        async fn open_main_with_speak(&self, text: String) -> Result<(), String> {
            *self.speak_text.lock().unwrap() = Some(text);
            Ok(())
        }
        fn open_settings(&self) {}
    }

    /// Test double for [`SelectedTextReader`] returning a fixed result.
    struct FakeReader {
        result: PlatformResult<SelectedText>,
    }

    #[async_trait::async_trait]
    impl SelectedTextReader for FakeReader {
        async fn selected_text(&self) -> PlatformResult<SelectedText> {
            match &self.result {
                Ok(selection) => Ok(selection.clone()),
                Err(error) => Err(match error {
                    PlatformError::Message(m) => PlatformError::Message(m.clone()),
                    PlatformError::PermissionDenied(m) => {
                        PlatformError::PermissionDenied(m.clone())
                    }
                    PlatformError::Unavailable(m) => PlatformError::Unavailable(m.clone()),
                }),
            }
        }
    }

    #[tokio::test]
    async fn run_app_quits_on_tray_quit_command() {
        let (_hotkey_tx, hotkey_rx) = mpsc::channel::<HotkeyEvent>(4);
        let (_app_tx, app_rx) = mpsc::channel::<AppEvent>(4);
        let (engine_tx, _engine_rx) = mpsc::channel::<HotkeyEvent>(4);

        let (command_tx, command_rx) = std_mpsc::channel::<UiCommand>();
        let (status_tx, _status_rx) = std_mpsc::channel::<UiStatus>();
        command_tx
            .send(UiCommand::Quit)
            .expect("queue quit command");

        let reader = Arc::new(FakeReader {
            result: Ok(SelectedText {
                text: String::new(),
                chars: 0,
                restored_clipboard: false,
            }),
        });

        let parts = PlatformParts {
            hotkey_rx,
            app_rx,
            engine_tx,
            tray: Some(Box::new(FakeTray {
                commands: command_rx,
                status_tx,
            })),
            reader,
            windows: None,
            banner: "test".into(),
        };

        // The first interval tick fires immediately, draining the queued Quit
        // command; run_app must then return promptly.
        let result = tokio::time::timeout(Duration::from_secs(2), run_app(parts)).await;
        assert!(result.is_ok(), "run_app did not return after Quit command");
        assert!(result.unwrap().is_ok(), "run_app returned an error");
    }

    #[tokio::test]
    async fn speak_selection_reports_status_sequence_on_reader_failure() {
        let (status_tx, status_rx) = std_mpsc::channel::<UiStatus>();
        let reader = Arc::new(FakeReader {
            result: Err(PlatformError::Unavailable("no selection".into())),
        });

        run_speak_selection(status_tx, reader, None).await;

        let statuses: Vec<UiStatus> = status_rx.try_iter().collect();
        assert_eq!(statuses.len(), 2, "expected two status updates");
        assert_eq!(statuses[0].state, DictationState::Transcribing);
        assert_eq!(statuses[0].message, "Reading selected text...");
        assert!(
            matches!(statuses[1].state, DictationState::Error(_)),
            "expected an error state, got {:?}",
            statuses[1].state
        );
        assert_eq!(statuses[1].message, "No selected text");
    }

    #[tokio::test]
    async fn speak_selection_reports_status_sequence_on_success() {
        let (status_tx, status_rx) = std_mpsc::channel::<UiStatus>();
        let reader = Arc::new(FakeReader {
            result: Ok(SelectedText {
                text: "hello".into(),
                chars: 5,
                restored_clipboard: true,
            }),
        });
        let windows = Arc::new(FakeWindows::default());

        run_speak_selection(status_tx, reader, Some(windows.clone())).await;

        let statuses: Vec<UiStatus> = status_rx.try_iter().collect();
        assert_eq!(statuses.len(), 2, "expected two status updates");
        assert_eq!(statuses[0].state, DictationState::Transcribing);
        assert_eq!(statuses[0].message, "Reading selected text...");
        assert_eq!(statuses[1].state, DictationState::Idle);
        assert_eq!(statuses[1].message, "Selected text captured: 5 chars");
        assert_eq!(windows.speak_text.lock().unwrap().as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn speak_selection_ignores_whitespace_only_selection() {
        let (status_tx, status_rx) = std_mpsc::channel::<UiStatus>();
        let reader = Arc::new(FakeReader {
            result: Ok(SelectedText {
                text: "  \n\t".into(),
                chars: 4,
                restored_clipboard: false,
            }),
        });
        let windows = Arc::new(FakeWindows::default());

        run_speak_selection(status_tx, reader, Some(windows.clone())).await;

        let statuses: Vec<UiStatus> = status_rx.try_iter().collect();
        assert_eq!(statuses.len(), 2, "expected two status updates");
        assert_eq!(statuses[0].state, DictationState::Transcribing);
        assert!(
            matches!(statuses[1].state, DictationState::Error(_)),
            "expected an error state, got {:?}",
            statuses[1].state
        );
        assert_eq!(statuses[1].message, "No selected text");
        assert!(
            windows.speak_text.lock().unwrap().is_none(),
            "whitespace-only selection must not open the speak window"
        );
    }

    #[tokio::test]
    async fn speak_selection_degrades_gracefully_without_a_window_layer() {
        let (status_tx, status_rx) = std_mpsc::channel::<UiStatus>();
        let reader = Arc::new(FakeReader {
            result: Ok(SelectedText {
                text: "hello".into(),
                chars: 5,
                restored_clipboard: true,
            }),
        });

        run_speak_selection(status_tx, reader, None).await;

        let statuses: Vec<UiStatus> = status_rx.try_iter().collect();
        assert_eq!(statuses.len(), 2, "expected two status updates");
        assert_eq!(statuses[1].state, DictationState::Idle);
        assert!(statuses[1].message.contains("no window available"));
    }
}
