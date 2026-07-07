//! Cross-platform application orchestration.
//!
//! The platform-specific `run()` entry points in `main.rs` are thin shims that
//! construct their adapters, start the status tray, and hand a [`PlatformParts`]
//! to [`run_app`]. All shared orchestration — the `tokio::select!` loop, the
//! speak/play/test-recording tasks, and the tray helpers — lives here so it can
//! be unit-tested and so a fix reaches every platform at once.

use anyhow::{Context, Result};
use codex_voice_core::DictationState;
use codex_voice_core::{AppEvent, HotkeyEvent, SelectedTextReader};
use codex_voice_ui::{StatusTray, UiCommand, UiStatus};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use crate::doctor;
use crate::logging;

/// Abstraction over the concrete platform [`StatusTray`] so the run loop can be
/// driven by a test double. Production code uses the [`StatusTray`] impl below;
/// tests provide their own command source and status sink.
pub trait TrayHandle: Send {
    fn try_recv_command(&self) -> Option<UiCommand>;
    fn update(&self, status: UiStatus);
    fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus>;
}

impl TrayHandle for StatusTray {
    fn try_recv_command(&self) -> Option<UiCommand> {
        StatusTray::try_recv_command(self)
    }
    fn update(&self, status: UiStatus) {
        StatusTray::update(self, status)
    }
    fn status_sender(&self) -> std::sync::mpsc::Sender<UiStatus> {
        StatusTray::status_sender(self)
    }
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
        banner,
    } = parts;

    let speech_state = Arc::new(SpeechState::default());
    println!("{banner}");

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => {
                match event {
                    HotkeyEvent::SpeakSelection => {
                        let reader = reader.clone();
                        let speech_state = speech_state.clone();
                        spawn_status_task(status_sender_for_tray(tray.as_deref()), &tray_busy, move |status_tx| {
                            run_speak_selection(status_tx, reader, speech_state)
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
                            UiCommand::SpeakText(text) => {
                                let speech_state = speech_state.clone();
                                spawn_tray_task(tray, &tray_busy, move |status_tx| {
                                    run_speak_text(status_tx, text, speech_state)
                                });
                            }
                            UiCommand::PlayLastSpeech => {
                                let speech_state = speech_state.clone();
                                spawn_tray_task(tray, &tray_busy, move |status_tx| {
                                    run_play_last_speech(status_tx, speech_state)
                                });
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

#[derive(Default)]
struct SpeechState {
    last_path: Mutex<Option<PathBuf>>,
}

async fn run_speak_selection<R>(
    status_tx: std::sync::mpsc::Sender<UiStatus>,
    reader: Arc<R>,
    speech_state: Arc<SpeechState>,
) where
    R: SelectedTextReader + Send + Sync + 'static,
{
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Reading selected text..."),
    );
    match reader.selected_text().await {
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
            run_speak_text(status_tx, selection.text, speech_state).await;
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

async fn run_speak_text(
    status_tx: std::sync::mpsc::Sender<UiStatus>,
    text: String,
    speech_state: Arc<SpeechState>,
) {
    if text.trim().is_empty() {
        set_tray_status(
            &status_tx,
            UiStatus::new(
                DictationState::Error("empty speech text".into()),
                "No text to speak",
            ),
        );
        return;
    }

    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Generating speech..."),
    );
    match synthesize_save_and_play(&text, speech_state.clone()).await {
        Ok(report) => {
            let message = format!("Played speech: {} chars", report.chars);
            set_tray_status(&status_tx, UiStatus::new(DictationState::Idle, message));
        }
        Err(error) => {
            tracing::warn!(%error, "speech generation/playback failed");
            let _ =
                logging::append_log_line(format!("speech generation/playback failed: {error:#}"));
            set_tray_error(&status_tx, "Speech failed", &error);
        }
    }
}

async fn run_play_last_speech(
    status_tx: std::sync::mpsc::Sender<UiStatus>,
    speech_state: Arc<SpeechState>,
) {
    let path = {
        let last_path = speech_state.last_path.lock().await;
        last_path.clone()
    }
    .unwrap_or_else(speech_output_path);

    if tokio::fs::metadata(&path).await.is_err() {
        set_tray_status(
            &status_tx,
            UiStatus::new(
                DictationState::Error("no generated speech".into()),
                "No generated speech to play",
            ),
        );
        return;
    }

    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Inserting, "Playing speech..."),
    );
    match play_audio_file(path.clone()).await {
        Ok(()) => {
            let _ = logging::append_log_line(format!("replayed speech audio: {}", path.display()));
            set_tray_status(
                &status_tx,
                UiStatus::new(DictationState::Idle, "Speech replay complete"),
            );
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "speech replay failed");
            let _ = logging::append_log_line(format!("speech replay failed: {error:#}"));
            set_tray_error(&status_tx, "Playback failed", &error);
        }
    }
}

struct SpeechRunReport {
    chars: usize,
}

async fn synthesize_save_and_play(
    text: &str,
    speech_state: Arc<SpeechState>,
) -> Result<SpeechRunReport> {
    let chars = text.chars().count();
    let client = codex_voice_transcriber::client::LocalTranscriberClient::discover(
        Duration::from_millis(500),
        Duration::from_secs(60),
    )
    .await
    .context("local speech service is not healthy or not discoverable")?;
    let speech = client
        .synthesize_speech(text)
        .await
        .map_err(anyhow::Error::from)
        .context("local speech synthesis failed")?;
    let path = speech_output_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tokio::fs::write(&path, &speech.bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    {
        let mut last_path = speech_state.last_path.lock().await;
        *last_path = Some(path.clone());
    }
    tracing::info!(
        chars,
        bytes = speech.bytes.len(),
        path = %path.display(),
        content_type = %speech.mime_type,
        "generated speech audio"
    );
    let _ = logging::append_log_line(format!(
        "generated speech audio: chars={chars} bytes={} path={}",
        speech.bytes.len(),
        path.display()
    ));
    play_audio_file(path).await?;
    Ok(SpeechRunReport { chars })
}

fn speech_output_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("codex-voice")
        .join("last-speech.wav")
}

async fn play_audio_file(path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || play_audio_file_blocking(&path))
        .await
        .context("audio playback task failed")?
}

fn play_audio_file_blocking(path: &Path) -> Result<()> {
    let sink_handle = rodio::DeviceSinkBuilder::open_default_sink()
        .context("failed to open default audio output")?;
    let file =
        std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let player = rodio::play(sink_handle.mixer(), std::io::BufReader::new(file))
        .context("failed to decode or start audio playback")?;
    player.sleep_until_end();
    Ok(())
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
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn open_logs() -> Result<()> {
    let path = logging::ensure_log_file()?;
    tracing::info!(path = %path.display(), "opening log file");
    let _ = logging::append_log_line(format!("opening log file: {}", path.display()));
    let mut child = std::process::Command::new("xdg-open")
        .arg(&path)
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_logs() -> Result<()> {
    let path = logging::ensure_log_file()?;
    tracing::info!(path = %path.display(), "opening log file");
    let _ = logging::append_log_line(format!("opening log file: {}", path.display()));
    let mut child = std::process::Command::new("cmd")
        .args(["/c", "start", "", &path.to_string_lossy()])
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
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

#[cfg(target_os = "windows")]
async fn run_tray_diagnostics(status_tx: std::sync::mpsc::Sender<UiStatus>) {
    let _ = logging::append_log_line("running windows diagnostics");
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Running diagnostics..."),
    );
    // Windows tray diagnostics are non-interactive in v1 because the main hotkey
    // service is already running and an interactive test would conflict with it.
    // Full diagnostics are available via CLI: doctor hotkey, doctor paste, doctor audio, etc.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = logging::append_log_line("windows diagnostics complete");
    set_tray_status(
        &status_tx,
        UiStatus::new(
            DictationState::Idle,
            "Diagnostics complete — use CLI for full tests",
        ),
    );
}

#[cfg(target_os = "macos")]
fn open_logs() -> Result<()> {
    let path = logging::ensure_log_file()?;
    tracing::info!(path = %path.display(), "opening log file");
    let _ = logging::append_log_line(format!("opening log file: {}", path.display()));
    let mut child = std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
async fn run_tray_diagnostics(status_tx: std::sync::mpsc::Sender<UiStatus>) {
    let _ = logging::append_log_line("running macos diagnostics");
    set_tray_status(
        &status_tx,
        UiStatus::new(DictationState::Transcribing, "Running diagnostics..."),
    );
    // macOS tray diagnostics are non-interactive in v1 because portal-based
    // diagnostics are Linux-only. Full diagnostics are available via CLI.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = logging::append_log_line("macos diagnostics complete");
    set_tray_status(
        &status_tx,
        UiStatus::new(
            DictationState::Idle,
            "Diagnostics complete — use CLI for full tests",
        ),
    );
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn open_logs() -> Result<()> {
    anyhow::bail!("open_logs is not implemented for this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
async fn run_tray_diagnostics(_status_tx: std::sync::mpsc::Sender<UiStatus>) {
    // unreachable on unsupported platforms because run() bails early
}
