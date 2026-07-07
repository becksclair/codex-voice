mod cli;
mod doctor;
mod logging;
mod tts;

use anyhow::{Context, Result};
use clap::Parser;
use codex_voice_audio::CpalWavRecorder;
use codex_voice_core::DictationState;
use codex_voice_core::{
    run_engine_loop, AppEvent, DictationEngine, HotkeyEvent, HotkeyService, SelectedTextReader,
    TextInjector,
};
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use cli::{Cli, Command, DoctorCommand, TranscriberCommand};

#[tokio::main]
async fn main() -> Result<()> {
    logging::init_tracing()?;

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run().await,
        Command::Server(args) => {
            let config: codex_voice_transcriber::ServeConfig = args.try_into()?;
            let tts_config_path = match tts::default_read_aloud_config_path() {
                Ok(path) => Some(path),
                Err(error) => {
                    tracing::warn!(%error, "TTS config path not available; live reload disabled");
                    None
                }
            };
            let (speech, tts_config) = match tts::load_speech_client(tts_config_path.clone()) {
                Ok(client) => {
                    tracing::info!("TTS client loaded successfully");
                    let config = client.config().clone();
                    (
                        Some(Arc::new(client) as Arc<dyn codex_voice_core::SpeechClient>),
                        Some(config),
                    )
                }
                Err(error) => {
                    tracing::warn!(%error, "TTS client not available; speech endpoint will return 503");
                    (None, None)
                }
            };
            codex_voice_transcriber::serve(config, speech, tts_config, tts_config_path).await
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
    let injector = Arc::new(LinuxTextInjector::new());
    let speech_state = Arc::new(SpeechState::default());
    let DictationApp {
        engine,
        mut app_rx,
        mut hotkey_rx,
    } = DictationApp::new(
        injector.clone(),
        codex_voice_platform::LinuxHotkeyService::new(),
    )
    .await?;
    println!("Codex Voice is running. Hold Control-M or the keyboard dictation key to dictate. Press Super-F6 to speak selected text.");

    let (engine_tx, engine_rx) = tokio::sync::mpsc::channel::<HotkeyEvent>(16);
    tokio::spawn(run_engine_loop(engine, engine_rx));

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => {
                match event {
                    HotkeyEvent::SpeakSelection => {
                        let reader = injector.clone();
                        let speech_state = speech_state.clone();
                        spawn_status_task(status_sender_for_tray(tray.as_ref()), &tray_busy, move |status_tx| {
                            run_speak_selection(status_tx, reader, speech_state)
                        });
                    }
                    other => { let _ = engine_tx.try_send(other); }
                }
            },
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
    let injector = Arc::new(WindowsTextInjector::new());
    let speech_state = Arc::new(SpeechState::default());
    let DictationApp {
        engine,
        mut app_rx,
        mut hotkey_rx,
    } = DictationApp::new(
        injector.clone(),
        codex_voice_platform::WindowsHotkeyService::new(),
    )
    .await?;
    println!(
        "Codex Voice is running. Hold Control-M to dictate. Press Win-F6 to speak selected text."
    );

    let (engine_tx, engine_rx) = tokio::sync::mpsc::channel::<HotkeyEvent>(16);
    tokio::spawn(run_engine_loop(engine, engine_rx));

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => {
                match event {
                    HotkeyEvent::SpeakSelection => {
                        let reader = injector.clone();
                        let speech_state = speech_state.clone();
                        spawn_status_task(status_sender_for_tray(tray.as_ref()), &tray_busy, move |status_tx| {
                            run_speak_selection(status_tx, reader, speech_state)
                        });
                    }
                    other => { let _ = engine_tx.try_send(other); }
                }
            },
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
    let injector = Arc::new(MacOSTextInjector::new());
    let speech_state = Arc::new(SpeechState::default());
    let DictationApp {
        engine,
        mut app_rx,
        mut hotkey_rx,
    } = DictationApp::new(
        injector.clone(),
        codex_voice_platform::MacOSHotkeyService::new()?,
    )
    .await?;
    println!("Codex Voice is running. Hold Control-M to dictate. Press Command-F6 to speak selected text.");

    let (engine_tx, engine_rx) = tokio::sync::mpsc::channel::<HotkeyEvent>(16);
    tokio::spawn(run_engine_loop(engine, engine_rx));

    let tray_busy = Arc::new(AtomicBool::new(false));
    let mut tray_poll = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => {
                match event {
                    HotkeyEvent::SpeakSelection => {
                        let reader = injector.clone();
                        let speech_state = speech_state.clone();
                        spawn_status_task(status_sender_for_tray(tray.as_ref()), &tray_busy, move |status_tx| {
                            run_speak_selection(status_tx, reader, speech_state)
                        });
                    }
                    other => { let _ = engine_tx.try_send(other); }
                }
            },
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

fn status_sender_for_tray(tray: Option<&StatusTray>) -> std::sync::mpsc::Sender<UiStatus> {
    tray.map(StatusTray::status_sender)
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
