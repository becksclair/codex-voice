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
#[cfg(target_os = "macos")]
use codex_voice_ui::MacOSUiConfig;
#[cfg(target_os = "windows")]
use codex_voice_ui::WindowsUiConfig;
#[cfg(target_os = "linux")]
use codex_voice_ui::{run_window_daemon, LinuxUiConfig, SettingsInfo, UiCommand, WindowEvent};
use codex_voice_ui::{StatusTray, UiStatus};
use std::sync::Arc;
use tokio::sync::mpsc;

use app::{run_app, PlatformParts, TrayHandle};
use cli::{Cli, Command, DoctorCommand, TranscriberCommand, TtsCommand};

fn main() -> Result<()> {
    logging::init_tracing()?;

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run);

    // The Linux `run` path must keep the main thread free of any ambient tokio
    // runtime: iced's tokio-backed executor calls block_on during window
    // creation, which panics ("Cannot start a runtime from within a runtime")
    // if the main thread already sits inside #[tokio::main]. Linux run() is
    // synchronous — it spawns its own background runtime and blocks the main
    // thread in the iced daemon — so dispatch it before building a runtime.
    #[cfg(target_os = "linux")]
    if matches!(command, Command::Run) {
        return run();
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(dispatch(command))
}

async fn dispatch(command: Command) -> Result<()> {
    match command {
        #[cfg(not(target_os = "linux"))]
        Command::Run => run().await,
        #[cfg(target_os = "linux")]
        Command::Run => unreachable!("Linux `run` is dispatched before the runtime is built"),
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
                codex_voice_transcriber::probe_limits(args.try_into()?)
                    .await
                    .map_err(Into::into)
            }
        },
        Command::Tts { command } => match command {
            TtsCommand::Bench(args) => tts::run_tts_bench(args).await,
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
    fn try_start(initial: UiStatus, config: C) -> Result<Self, codex_voice_ui::UiError>;
}

#[cfg(target_os = "linux")]
impl TrayStart<LinuxUiConfig> for StatusTray {
    fn try_start(
        initial: UiStatus,
        config: LinuxUiConfig,
    ) -> Result<Self, codex_voice_ui::UiError> {
        StatusTray::start(initial, config)
    }
}

#[cfg(target_os = "windows")]
impl TrayStart<WindowsUiConfig> for StatusTray {
    fn try_start(
        initial: UiStatus,
        config: WindowsUiConfig,
    ) -> Result<Self, codex_voice_ui::UiError> {
        StatusTray::start(initial, config)
    }
}

#[cfg(target_os = "macos")]
impl TrayStart<MacOSUiConfig> for StatusTray {
    fn try_start(
        initial: UiStatus,
        config: MacOSUiConfig,
    ) -> Result<Self, codex_voice_ui::UiError> {
        StatusTray::start(initial, config)
    }
}

/// Linux `run` inverts the usual thread ownership: the iced window daemon must
/// own the process main thread (winit 0.30 refuses off-main-thread event loops
/// on Linux), so the tokio engine + run-loop move to a background thread with
/// their own runtime, and this function blocks the main thread in the daemon.
///
/// The `server` subcommand and every non-Linux `run` path are unaffected.
#[cfg(target_os = "linux")]
fn run() -> Result<()> {
    let log_path = logging::log_file_path();

    // WindowEvent channel: tray/app -> iced daemon (open/focus windows, status).
    let (window_tx, window_rx) = mpsc::unbounded_channel::<WindowEvent>();
    let config_window_tx = window_tx.clone();
    let exit_window_tx = window_tx;

    // UiCommand channel: ksni menu closures and the iced Speak Text window both
    // send here; the run-loop drains it via `try_recv_command`.
    let (command_tx, command_rx) = std::sync::mpsc::channel::<UiCommand>();
    let daemon_command_tx = command_tx.clone();

    let settings_info = SettingsInfo::new(log_path.clone());

    // Releases the daemon even if the driven future panics: a panic unwinds
    // through block_on and would skip a plain send, leaving the daemon (and
    // the process) alive forever instead of surfacing the panic via join().
    struct ExitGuard(tokio::sync::mpsc::UnboundedSender<WindowEvent>);
    impl Drop for ExitGuard {
        fn drop(&mut self) {
            let _ = self.0.send(WindowEvent::Exit);
        }
    }

    // Background thread: owns a multi-threaded tokio runtime that runs the
    // engine and the shared select loop, exactly as the other platforms do on
    // their main thread.
    let background = std::thread::Builder::new()
        .name("codex-voice-run".to_string())
        .spawn(move || -> Result<()> {
            // First thing in the thread, so even a runtime-build failure or a
            // panic anywhere below still releases the daemon on unwind.
            let _exit_guard = ExitGuard(exit_window_tx);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let outcome = runtime.block_on(async move {
                let tray = start_tray(LinuxUiConfig {
                    window_tx: config_window_tx,
                    command_tx,
                    command_rx,
                });
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
            });
            // The ExitGuard drop releases the daemon however the run-loop
            // finished (Quit, closed channels, error, or panic).
            outcome
        })?;

    // Main thread: block in the iced daemon. If it cannot start (e.g. no
    // display), degrade to tray-only/headless by simply joining the background
    // thread, matching the optional-tray philosophy.
    if let Err(error) = run_window_daemon(window_rx, daemon_command_tx, settings_info) {
        tracing::warn!(%error, "window daemon unavailable; running without on-demand windows");
    }

    match background.join() {
        Ok(result) => result,
        Err(_) => anyhow::bail!("background application thread panicked"),
    }
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
