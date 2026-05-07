use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use codex_voice_audio::CpalWavRecorder;
use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
#[cfg(target_os = "linux")]
use codex_voice_core::DictationState;
use codex_voice_core::{
    AppEvent, AudioRecorder, DictationEngine, HotkeyService, PermissionService, RecordedAudio,
    TextInjector, TranscriptionClient,
};
#[cfg(target_os = "linux")]
use codex_voice_platform::{LinuxHotkeyService, LinuxPermissionService, LinuxTextInjector};
#[cfg(target_os = "windows")]
use codex_voice_platform::{WindowsHotkeyService, WindowsPermissionService, WindowsTextInjector};
#[cfg(target_os = "linux")]
use codex_voice_ui::{LinuxUiConfig, StatusTray, UiCommand, UiStatus};
#[cfg(target_os = "linux")]
use std::process::Command as ProcessCommand;
use std::{
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

#[derive(Debug, Parser)]
#[command(
    name = "codex-voice",
    version,
    about = "Hold-to-dictate desktop utility backed by local Codex auth"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run,
    Doctor {
        #[command(subcommand)]
        command: Option<DoctorCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum DoctorCommand {
    Audio(AudioDoctor),
    CodexAuth,
    Transcribe(TranscribeDoctor),
    Hotkey,
    Paste(PasteDoctor),
    LinuxPortals,
}

#[derive(Debug, Args)]
struct AudioDoctor {
    #[arg(long, default_value_t = 2)]
    seconds: u64,
    #[arg(long)]
    keep: bool,
}

#[derive(Debug, Args)]
struct TranscribeDoctor {
    #[arg(long)]
    file: PathBuf,
}

#[derive(Debug, Args)]
struct PasteDoctor {
    #[arg(long)]
    text: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run().await,
        Command::Doctor { command } => match command.unwrap_or(DoctorCommand::LinuxPortals) {
            DoctorCommand::Audio(args) => doctor_audio(args).await,
            DoctorCommand::CodexAuth => doctor_codex_auth(),
            DoctorCommand::Transcribe(args) => doctor_transcribe(args.file).await,
            DoctorCommand::Hotkey => doctor_hotkey().await,
            DoctorCommand::Paste(args) => doctor_paste(args.text).await,
            DoctorCommand::LinuxPortals => doctor_linux_portals().await,
        },
    }
}

#[cfg(target_os = "linux")]
async fn run() -> Result<()> {
    let audio = Arc::new(CpalWavRecorder::new());
    let auth = CodexAuthService::new()?;
    let transcription = Arc::new(CodexTranscriptionClient::new(auth)?);
    let injector = Arc::new(LinuxTextInjector::new());
    let (app_tx, mut app_rx) = mpsc::channel(64);
    let (hotkey_tx, mut hotkey_rx) = mpsc::channel(16);
    LinuxHotkeyService::new().start(hotkey_tx)?;
    let log_path = log_file_path();
    let tray = match StatusTray::start(UiStatus::idle(), LinuxUiConfig { log_path }) {
        Ok(tray) => Some(tray),
        Err(error) => {
            tracing::warn!(%error, "failed to start status tray");
            None
        }
    };
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));

    let mut engine = DictationEngine::new(audio, transcription, injector, app_tx);
    println!("Codex Voice is running. Hold Control-M or the keyboard dictation key to dictate through the KDE/Wayland GlobalShortcuts portal.");

    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => engine.handle_hotkey(event).await,
            Some(event) = app_rx.recv() => {
                if let Some(status) = UiStatus::from_app_event(&event) {
                    if let Some(tray) = &tray {
                        tray.update(status);
                    }
                }
                print_app_event(event);
            }
            _ = tray_poll.tick() => {
                if let Some(status_tray) = &tray {
                    while let Some(command) = status_tray.try_recv_command() {
                        match command {
                            UiCommand::StartTestRecording => run_tray_test_recording(&tray).await,
                            UiCommand::OpenLogs => open_tray_logs(&tray),
                            UiCommand::RunDiagnostics => run_tray_diagnostics(&tray).await,
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

fn init_tracing() -> Result<()> {
    let log_path = ensure_log_file()?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        "codex_voice_app=info,codex_voice_audio=info,codex_voice_platform=info,info".into()
    });

    tracing_subscriber::fmt().with_env_filter(filter).init();

    append_log_line(format!("logging initialized: {}", log_path.display()))?;
    Ok(())
}

fn log_file_path() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("codex-voice")
        .join("codex-voice.log")
}

fn append_log_line(message: impl AsRef<str>) -> Result<()> {
    let path = ensure_log_file()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut file = open_log_append(&path)?;
    writeln!(file, "{timestamp} {}", message.as_ref())
        .with_context(|| format!("failed to write log file {}", path.display()))?;
    Ok(())
}

fn ensure_log_file() -> Result<PathBuf> {
    let path = log_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }
    open_log_append(&path)?;
    Ok(path)
}

fn open_log_append(path: &PathBuf) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open log file {}", path.display()))
}

#[cfg(target_os = "windows")]
async fn run() -> Result<()> {
    let audio = Arc::new(CpalWavRecorder::new());
    let auth = CodexAuthService::new()?;
    let transcription = Arc::new(CodexTranscriptionClient::new(auth)?);
    let injector = Arc::new(WindowsTextInjector::new());
    let (app_tx, mut app_rx) = mpsc::channel(64);
    let (hotkey_tx, mut hotkey_rx) = mpsc::channel(16);
    WindowsHotkeyService::new().start(hotkey_tx)?;
    let mut engine = DictationEngine::new(audio, transcription, injector, app_tx);
    println!("Codex Voice is running. Hold Control-M to dictate. Paste insertion uses clipboard plus SendInput and may be blocked for elevated target apps.");

    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => engine.handle_hotkey(event).await,
            Some(event) = app_rx.recv() => print_app_event(event),
            else => break,
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn run() -> Result<()> {
    anyhow::bail!("this milestone implements Linux and Windows only")
}

async fn doctor_audio(args: AudioDoctor) -> Result<()> {
    println!("starting audio capture for {} second(s)...", args.seconds);
    println!("recording...");
    let (recording, size) = capture_audio_sample(Duration::from_secs(args.seconds)).await?;
    println!("stopping audio capture...");
    println!("path: {}", recording.path.display());
    println!("duration_ms: {}", recording.duration.as_millis());
    println!("content_type: {}", recording.content_type);
    println!("bytes: {size}");
    if !args.keep {
        std::fs::remove_file(&recording.path)
            .with_context(|| format!("failed to delete {}", recording.path.display()))?;
        println!("deleted: true");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn run_tray_test_recording(tray: &Option<StatusTray>) {
    if let Err(error) = run_test_recording(tray).await {
        tracing::warn!(%error, "tray test recording failed");
    }
}

#[cfg(target_os = "linux")]
async fn run_test_recording(tray: &Option<StatusTray>) -> Result<()> {
    set_tray_status(
        tray,
        UiStatus::new(DictationState::Recording, "Running test recording..."),
    );
    tracing::info!("starting tray test recording");
    append_log_line("starting test recording")?;
    let result = capture_audio_sample(Duration::from_secs(2)).await;

    match result {
        Ok((recording, size)) => {
            let duration_ms = recording.duration.as_millis();
            let cleanup = std::fs::remove_file(&recording.path)
                .with_context(|| format!("failed to delete {}", recording.path.display()));
            if let Err(error) = cleanup {
                set_test_recording_error(tray, &error);
                return Err(error);
            }
            let message = format!("Test recording ok: {} ms, {size} bytes", duration_ms);
            tracing::info!(duration_ms, bytes = size, "test recording ok");
            if let Err(error) =
                append_log_line(format!("test recording ok: {duration_ms} ms, {size} bytes"))
            {
                set_test_recording_error(tray, &error);
                return Err(error);
            }
            set_tray_status(tray, UiStatus::new(DictationState::Idle, message));
            Ok(())
        }
        Err(error) => {
            set_test_recording_error(tray, &error);
            Err(error)
        }
    }
}

#[cfg(target_os = "linux")]
fn set_test_recording_error(tray: &Option<StatusTray>, error: &anyhow::Error) {
    let message = format!("Test recording failed: {error:#}");
    tracing::warn!(error = %error, "test recording failed");
    let _ = append_log_line(format!("test recording failed: {error:#}"));
    set_tray_status(
        tray,
        UiStatus::new(DictationState::Error(error.to_string()), message),
    );
}

async fn capture_audio_sample(duration: Duration) -> Result<(RecordedAudio, u64)> {
    let recorder = CpalWavRecorder::new();
    recorder
        .start()
        .await
        .context("failed to start audio capture")?;
    tokio::time::sleep(duration).await;
    let recording = recorder
        .stop()
        .await
        .context("failed to stop audio capture")?
        .context("audio recorder returned no recording")?;
    let size = match std::fs::metadata(&recording.path) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            let _ = std::fs::remove_file(&recording.path);
            return Err(error)
                .with_context(|| format!("failed to stat {}", recording.path.display()));
        }
    };
    Ok((recording, size))
}

#[cfg(target_os = "linux")]
fn set_tray_status(tray: &Option<StatusTray>, status: UiStatus) {
    if let Some(tray) = tray {
        tray.update(status);
    }
}

#[cfg(target_os = "linux")]
fn open_logs() -> Result<()> {
    let path = ensure_log_file()?;
    tracing::info!(path = %path.display(), "opening log file");
    append_log_line(format!("opening log file: {}", path.display()))?;
    ProcessCommand::new("xdg-open")
        .arg(&path)
        .spawn()
        .with_context(|| format!("failed to open {}", path.display()))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_tray_logs(tray: &Option<StatusTray>) {
    if let Err(error) = open_logs() {
        tracing::warn!(%error, "failed to open logs");
        set_tray_error(tray, format!("Open logs failed: {error:#}"), error);
    }
}

#[cfg(target_os = "linux")]
async fn run_tray_diagnostics(tray: &Option<StatusTray>) {
    let _ = append_log_line("running linux portal diagnostics");
    set_tray_status(
        tray,
        UiStatus::new(DictationState::Transcribing, "Running diagnostics..."),
    );
    match doctor_linux_portals().await {
        Ok(()) => {
            let _ = append_log_line("linux portal diagnostics complete");
            set_tray_status(
                tray,
                UiStatus::new(DictationState::Idle, "Diagnostics complete"),
            );
        }
        Err(error) => {
            tracing::warn!(%error, "tray diagnostics failed");
            let _ = append_log_line(format!("linux portal diagnostics failed: {error:#}"));
            set_tray_error(tray, format!("Diagnostics failed: {error:#}"), error);
        }
    }
}

#[cfg(target_os = "linux")]
fn set_tray_error(tray: &Option<StatusTray>, message: String, error: anyhow::Error) {
    set_tray_status(
        tray,
        UiStatus::new(DictationState::Error(error.to_string()), message),
    );
}

fn doctor_codex_auth() -> Result<()> {
    let auth_service = CodexAuthService::new()?;
    let auth = auth_service.read_or_refresh()?;
    println!("auth_path: {}", auth_service.auth_path().display());
    println!("access_token: present_redacted");
    println!(
        "account_id: {}",
        auth.account_id
            .as_deref()
            .map(redact_account)
            .unwrap_or_else(|| "absent".into())
    );
    Ok(())
}

async fn doctor_transcribe(file: PathBuf) -> Result<()> {
    let duration = wav_duration(&file).unwrap_or_default();
    let recording = codex_voice_core::RecordedAudio {
        filename: file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("input.wav")
            .to_string(),
        path: file,
        content_type: "audio/wav".into(),
        duration,
    };
    let client = CodexTranscriptionClient::new(CodexAuthService::new()?)?;
    let transcript = client.transcribe(&recording).await?;
    let preview: String = transcript.chars().take(80).collect();
    println!("transcript_chars: {}", transcript.chars().count());
    println!("preview: {}", preview.replace('\n', " "));
    Ok(())
}

#[cfg(target_os = "linux")]
async fn doctor_hotkey() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(8);
    LinuxHotkeyService::new().start(tx)?;
    println!("Waiting for hotkey events. Hold and release Control-M or the keyboard dictation key after approving the KDE/Wayland GlobalShortcuts portal prompt.");
    for _ in 0..2 {
        let event = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .context("timed out waiting for hotkey event")?
            .context("hotkey listener stopped before emitting the expected events")?;
        println!("{event:?}");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
async fn doctor_hotkey() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(8);
    WindowsHotkeyService::new().start(tx)?;
    println!("Waiting for hotkey events. Hold and release Control-M within 30 seconds.");
    for _ in 0..2 {
        let event = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .context("timed out waiting for hotkey event")?
            .context("hotkey listener stopped before emitting the expected events")?;
        println!("{event:?}");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn doctor_hotkey() -> Result<()> {
    anyhow::bail!("hotkey diagnostics are implemented for Linux and Windows only")
}

#[cfg(target_os = "linux")]
async fn doctor_paste(text: String) -> Result<()> {
    let report = LinuxTextInjector::new().insert_text(&text).await?;
    println!("method: {:?}", report.method);
    println!("restored_clipboard: {}", report.restored_clipboard);
    Ok(())
}

#[cfg(target_os = "windows")]
async fn doctor_paste(text: String) -> Result<()> {
    let report = WindowsTextInjector::new().insert_text(&text).await?;
    println!("method: {:?}", report.method);
    println!("restored_clipboard: {}", report.restored_clipboard);
    println!(
        "note: SendInput may be blocked when targeting elevated apps from a non-elevated process"
    );
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn doctor_paste(_text: String) -> Result<()> {
    anyhow::bail!("paste diagnostics are implemented for Linux and Windows only")
}

#[cfg(target_os = "linux")]
async fn doctor_linux_portals() -> Result<()> {
    println!(
        "XDG_SESSION_TYPE: {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unset".into())
    );
    println!(
        "XDG_CURRENT_DESKTOP: {}",
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unset".into())
    );
    for status in LinuxPermissionService::new().check().await? {
        println!(
            "{:?}: available={} granted={:?} detail={}",
            status.kind, status.available, status.granted, status.detail
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn doctor_linux_portals() -> Result<()> {
    #[cfg(not(target_os = "windows"))]
    anyhow::bail!("linux portal diagnostics are implemented for Linux only");

    #[cfg(target_os = "windows")]
    println!("linux portal diagnostics are not applicable on Windows");
    #[cfg(target_os = "windows")]
    for status in WindowsPermissionService::new().check().await? {
        println!(
            "{:?}: available={} granted={:?} detail={}",
            status.kind, status.available, status.granted, status.detail
        );
    }
    #[cfg(target_os = "windows")]
    Ok(())
}

fn print_app_event(event: AppEvent) {
    match event {
        AppEvent::TranscriptReady { chars } => {
            tracing::info!(target: "codex_voice_app", chars, "transcript ready");
            let _ = append_log_line(format!("transcript ready: {chars} chars"));
            println!("transcript ready: {chars} chars");
        }
        AppEvent::Inserted(report) => {
            tracing::info!(
                target: "codex_voice_app",
                method = ?report.method,
                restored_clipboard = report.restored_clipboard,
                "inserted transcript"
            );
            let _ = append_log_line(format!(
                "inserted transcript: method={:?} restored_clipboard={}",
                report.method, report.restored_clipboard
            ));
            println!("inserted via {:?}", report.method);
        }
        other => {
            tracing::debug!(target: "codex_voice_app", event = ?other, "app event");
            println!("{other:?}");
        }
    }
}

fn redact_account(account_id: &str) -> String {
    let mut chars = account_id.chars();
    let prefix: String = chars.by_ref().take(6).collect();
    if account_id.chars().count() <= 6 {
        "present_redacted".into()
    } else {
        format!("{prefix}...")
    }
}

fn wav_duration(path: &PathBuf) -> Result<Duration> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples = reader.duration();
    Ok(Duration::from_secs_f64(
        samples as f64 / spec.sample_rate.max(1) as f64,
    ))
}
