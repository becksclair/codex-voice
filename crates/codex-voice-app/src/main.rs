mod app;
mod cli;
mod doctor;
mod logging;
mod tts;

use anyhow::Result;
use clap::Parser;
use codex_voice_audio::CpalWavRecorder;
use codex_voice_core::{
    run_engine_loop, AppEvent, DictationEngine, HotkeyEvent, HotkeyService, TextInjector,
};
#[cfg(target_os = "linux")]
use codex_voice_platform::LinuxTextInjector;
#[cfg(target_os = "macos")]
use codex_voice_platform::MacOSTextInjector;
#[cfg(target_os = "windows")]
use codex_voice_platform::WindowsTextInjector;
use codex_voice_transcriber::RuntimeTranscriptionClient;
#[cfg(target_os = "linux")]
use codex_voice_ui::LinuxUiConfig;
#[cfg(target_os = "macos")]
use codex_voice_ui::MacOSUiConfig;
#[cfg(target_os = "windows")]
use codex_voice_ui::WindowsUiConfig;
use codex_voice_ui::{StatusTray, UiStatus};
use std::sync::Arc;
use tokio::sync::mpsc;

use app::{run_app, PlatformParts, TrayHandle};
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

/// Spawns the dictation engine on its own task and returns the channels the
/// shared run loop uses to drive it.
async fn spawn_engine<I>(
    injector: Arc<I>,
    hotkey_service: impl HotkeyService,
) -> Result<(
    mpsc::Receiver<AppEvent>,
    mpsc::Receiver<HotkeyEvent>,
    mpsc::Sender<HotkeyEvent>,
)>
where
    I: TextInjector + 'static,
{
    let DictationApp {
        engine,
        app_rx,
        hotkey_rx,
    } = DictationApp::new(injector, hotkey_service).await?;
    let (engine_tx, engine_rx) = mpsc::channel::<HotkeyEvent>(16);
    tokio::spawn(run_engine_loop(engine, engine_rx));
    Ok((app_rx, hotkey_rx, engine_tx))
}

fn start_tray<C>(config: C) -> Option<Box<dyn TrayHandle>>
where
    StatusTray: TrayStart<C>,
{
    match StatusTray::try_start(UiStatus::idle(), config) {
        Ok(tray) => Some(Box::new(tray)),
        Err(error) => {
            tracing::warn!(%error, "failed to start status tray");
            None
        }
    }
}

/// Bridges the per-platform `StatusTray::start(initial, config)` constructors
/// behind one generic call so `start_tray` stays platform-agnostic.
trait TrayStart<C>: Sized {
    fn try_start(initial: UiStatus, config: C) -> Result<Self, String>;
}

#[cfg(target_os = "linux")]
impl TrayStart<LinuxUiConfig> for StatusTray {
    fn try_start(initial: UiStatus, config: LinuxUiConfig) -> Result<Self, String> {
        StatusTray::start(initial, config)
    }
}

#[cfg(target_os = "windows")]
impl TrayStart<WindowsUiConfig> for StatusTray {
    fn try_start(initial: UiStatus, config: WindowsUiConfig) -> Result<Self, String> {
        StatusTray::start(initial, config)
    }
}

#[cfg(target_os = "macos")]
impl TrayStart<MacOSUiConfig> for StatusTray {
    fn try_start(initial: UiStatus, config: MacOSUiConfig) -> Result<Self, String> {
        StatusTray::start(initial, config)
    }
}

#[cfg(target_os = "linux")]
async fn run() -> Result<()> {
    let log_path = logging::log_file_path();
    let tray = start_tray(LinuxUiConfig { log_path });
    let injector = Arc::new(LinuxTextInjector::new());
    let (app_rx, hotkey_rx, engine_tx) = spawn_engine(
        injector.clone(),
        codex_voice_platform::LinuxHotkeyService::new(),
    )
    .await?;
    run_app(PlatformParts {
        hotkey_rx,
        app_rx,
        engine_tx,
        tray,
        reader: injector,
        banner: "Codex Voice is running. Hold Control-M or the keyboard dictation key to dictate. Press Super-F6 to speak selected text.".into(),
    })
    .await
}

#[cfg(target_os = "windows")]
async fn run() -> Result<()> {
    let log_path = logging::log_file_path();
    let tray = start_tray(WindowsUiConfig { log_path });
    let injector = Arc::new(WindowsTextInjector::new());
    let (app_rx, hotkey_rx, engine_tx) = spawn_engine(
        injector.clone(),
        codex_voice_platform::WindowsHotkeyService::new(),
    )
    .await?;
    run_app(PlatformParts {
        hotkey_rx,
        app_rx,
        engine_tx,
        tray,
        reader: injector,
        banner: "Codex Voice is running. Hold Control-M to dictate. Press Win-F6 to speak selected text.".into(),
    })
    .await
}

#[cfg(target_os = "macos")]
async fn run() -> Result<()> {
    let log_path = logging::log_file_path();
    let tray = start_tray(MacOSUiConfig { log_path });
    let injector = Arc::new(MacOSTextInjector::new());
    let (app_rx, hotkey_rx, engine_tx) = spawn_engine(
        injector.clone(),
        codex_voice_platform::MacOSHotkeyService::new()?,
    )
    .await?;
    run_app(PlatformParts {
        hotkey_rx,
        app_rx,
        engine_tx,
        tray,
        reader: injector,
        banner: "Codex Voice is running. Hold Control-M to dictate. Press Command-F6 to speak selected text.".into(),
    })
    .await
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
async fn run() -> Result<()> {
    anyhow::bail!("this build only implements Linux, Windows, and macOS")
}
