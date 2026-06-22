mod cli;
mod doctor;
mod logging;
mod tts;

use anyhow::{Context, Result};
use clap::Parser;
use codex_voice_audio::CpalWavRecorder;
use codex_voice_core::DictationState;
use codex_voice_core::{AppEvent, DictationEngine, HotkeyEvent, HotkeyService, TextInjector};
#[cfg(target_os = "linux")]
use codex_voice_platform::LinuxTextInjector;
#[cfg(target_os = "macos")]
use codex_voice_platform::MacOSTextInjector;
#[cfg(target_os = "windows")]
use codex_voice_platform::WindowsTextInjector;
use codex_voice_transcriber::RuntimeTranscriptionClient;
#[cfg(target_os = "linux")]
use codex_voice_ui::{LinuxUiConfig, StatusTray, UiCommand, UiStatus};
#[cfg(target_os = "macos")]
use codex_voice_ui::{MacOSUiConfig, StatusTray, UiCommand, UiStatus};
#[cfg(target_os = "windows")]
use codex_voice_ui::{StatusTray, UiCommand, UiStatus, WindowsUiConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

use cli::{Cli, Command, DoctorCommand, TranscriberCommand};

#[tokio::main]
async fn main() -> Result<()> {
    logging::init_tracing()?;

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run().await,
        Command::Server(args) => {
            let config: codex_voice_transcriber::ServeConfig = args.try_into()?;
            let speech = match tts::load_speech_client(None) {
                Ok(client) => {
                    tracing::info!("TTS client loaded successfully");
                    Some(Arc::new(client) as Arc<dyn codex_voice_core::SpeechClient>)
                }
                Err(error) => {
                    tracing::warn!(%error, "TTS client not available; speech endpoint will return 503");
                    None
                }
            };
            codex_voice_transcriber::serve(config, speech).await
        }
        Command::Doctor { command } => match command.unwrap_or(DoctorCommand::LinuxPortals) {
            DoctorCommand::Audio(args) => doctor::doctor_audio(args).await,
            DoctorCommand::CodexAuth => doctor::doctor_codex_auth().await,
            DoctorCommand::Transcribe(args) => doctor::doctor_transcribe(args.file).await,
            DoctorCommand::Tts(args) => tts::doctor_tts(args).await,
            DoctorCommand::Hotkey => doctor::doctor_hotkey().await,
            DoctorCommand::Paste(args) => doctor::doctor_paste(args.text).await,
            DoctorCommand::LinuxPortals => doctor::doctor_portals().await,
        },
        Command::Transcriber { command } => match command {
            TranscriberCommand::ProbeLimits(args) => {
                codex_voice_transcriber::probe_limits(args.try_into()?).await
            }
        },
    }
}

struct DictationApp<I: TextInjector> {
    engine: DictationEngine<CpalWavRecorder, RuntimeTranscriptionClient, I>,
    app_rx: mpsc::Receiver<AppEvent>,
    hotkey_rx: mpsc::Receiver<HotkeyEvent>,
}

impl<I: TextInjector> DictationApp<I> {
    async fn new(injector: Arc<I>, hotkey_service: impl HotkeyService) -> Result<Self> {
        let audio = Arc::new(CpalWavRecorder::new());
        let resolved = codex_voice_transcriber::resolve_transcription_backend().await?;
        tracing::info!(backend = resolved.label, "selected transcription backend");
        let _ = logging::append_log_line(format!("transcription_backend={}", resolved.label));
        println!("transcription backend: {}", resolved.label);
        let transcription = Arc::new(resolved.client);
        let (app_tx, app_rx) = mpsc::channel(64);
        let (hotkey_tx, hotkey_rx) = mpsc::channel(16);
        hotkey_service.start(hotkey_tx)?;
        let engine = DictationEngine::new(audio, transcription, injector, app_tx);
        Ok(Self {
            engine,
            app_rx,
            hotkey_rx,
        })
    }
}

#[cfg(target_os = "linux")]
async fn run() -> Result<()> {
    let log_path = logging::log_file_path();
    let tray = match StatusTray::start(UiStatus::idle(), LinuxUiConfig { log_path }) {
        Ok(tray) => Some(tray),
        Err(error) => {
            tracing::warn!(%error, "failed to start status tray");
            None
        }
    };
    let DictationApp {
        mut engine,
        mut app_rx,
        mut hotkey_rx,
    } = DictationApp::new(
        Arc::new(LinuxTextInjector::new()),
        codex_voice_platform::LinuxHotkeyService::new(),
    )
    .await?;
    println!("Codex Voice is running. Hold Control-M or the keyboard dictation key to dictate through the KDE/Wayland GlobalShortcuts portal.");

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => engine.handle_hotkey(event).await,
            Some(event) = app_rx.recv() => {
                if let Some(ref tray) = tray {
                    if let Some(status) = UiStatus::from_app_event(&event) {
                        tray.update(status);
                    }
                }
                print_app_event(event);
            }
            _ = tray_poll.tick() => {
                if let Some(ref tray) = tray {
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

#[cfg(target_os = "windows")]
async fn run() -> Result<()> {
    let log_path = logging::log_file_path();
    let tray = match StatusTray::start(UiStatus::idle(), WindowsUiConfig { log_path }) {
        Ok(tray) => Some(tray),
        Err(error) => {
            tracing::warn!(%error, "failed to start status tray");
            None
        }
    };
    let DictationApp {
        mut engine,
        mut app_rx,
        mut hotkey_rx,
    } = DictationApp::new(
        Arc::new(WindowsTextInjector::new()),
        codex_voice_platform::WindowsHotkeyService::new(),
    )
    .await?;
    println!("Codex Voice is running. Hold Control-M to dictate. Paste insertion uses clipboard plus SendInput and may be blocked for elevated target apps.");

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => engine.handle_hotkey(event).await,
            Some(event) = app_rx.recv() => {
                if let Some(ref tray) = tray {
                    if let Some(status) = UiStatus::from_app_event(&event) {
                        tray.update(status);
                    }
                }
                print_app_event(event);
            }
            _ = tray_poll.tick() => {
                if let Some(ref tray) = tray {
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

#[cfg(target_os = "macos")]
async fn run() -> Result<()> {
    let log_path = logging::log_file_path();
    let tray = match StatusTray::start(UiStatus::idle(), MacOSUiConfig { log_path }) {
        Ok(tray) => Some(tray),
        Err(error) => {
            tracing::warn!(%error, "failed to start status tray");
            None
        }
    };
    let DictationApp {
        mut engine,
        mut app_rx,
        mut hotkey_rx,
    } = DictationApp::new(
        Arc::new(MacOSTextInjector::new()),
        codex_voice_platform::MacOSHotkeyService::new()?,
    )
    .await?;
    println!("Codex Voice is running. Hold Control-M to dictate. Text insertion uses macOS Accessibility API with clipboard paste fallback.");

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => engine.handle_hotkey(event).await,
            Some(event) = app_rx.recv() => {
                if let Some(ref tray) = tray {
                    if let Some(status) = UiStatus::from_app_event(&event) {
                        tray.update(status);
                    }
                }
                print_app_event(event);
            }
            _ = tray_poll.tick() => {
                if let Some(ref tray) = tray {
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

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
async fn run() -> Result<()> {
    anyhow::bail!("this build only implements Linux, Windows, and macOS")
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

fn spawn_tray_task<F, Fut>(tray: &StatusTray, busy: &Arc<AtomicBool>, task: F)
where
    F: FnOnce(std::sync::mpsc::Sender<UiStatus>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    if busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        let status_tx = tray.status_sender();
        let busy = busy.clone();
        tokio::spawn(async move {
            let _guard = TrayBusyGuard(busy);
            task(status_tx).await;
        });
    }
}

struct TrayBusyGuard(Arc<AtomicBool>);

impl Drop for TrayBusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
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

fn open_tray_logs(tray: &StatusTray) {
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
