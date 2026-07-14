mod app;
mod cli;
mod doctor;
mod hud;
mod logging;
mod status;
mod tray;
mod tts;
mod windows;

use anyhow::{Context, Result};
use clap::Parser;
use codex_voice_audio::CpalWavRecorder;
use codex_voice_core::{run_engine_loop, AppEvent, DictationEngine, HotkeyEvent, HotkeyService};
use codex_voice_transcriber::{client::LocalTranscriberClient, EmbeddedServiceHandle};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

use app::{run_app, PlatformParts};
use cli::{Cli, Command, DoctorCommand, TranscriberCommand, TtsCommand};
use status::UiStatus;
use tray::{AppWindows, TauriTray};
use windows::{DesktopWindows, UnavailableWindows};

fn main() -> Result<()> {
    logging::init_tracing()?;

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run);

    // Tauri needs to own the main thread (this matters most on macOS), so
    // `run` is dispatched before a tokio runtime is built on every platform.
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
        Command::Run => unreachable!("`run` is dispatched before the runtime is built"),
        Command::Server(args) => {
            let config: codex_voice_transcriber::ServeConfig = args.try_into()?;
            let (speech, tts_config, tts_config_path) = tts::load_tts();
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

// ---------------------------------------------------------------------------
// Platform text injector / hotkey service construction.
//
// Only the concrete adapter types differ between platforms; the engine spawn
// and run loop wiring is shared.
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
type PlatformTextInjector = codex_voice_platform::LinuxTextInjector;
#[cfg(target_os = "windows")]
type PlatformTextInjector = codex_voice_platform::WindowsTextInjector;
#[cfg(target_os = "macos")]
type PlatformTextInjector = codex_voice_platform::MacOSTextInjector;
#[cfg(target_os = "linux")]
type PlatformHotkeyService = codex_voice_platform::LinuxHotkeyService;
#[cfg(target_os = "windows")]
type PlatformHotkeyService = codex_voice_platform::WindowsHotkeyService;
#[cfg(target_os = "macos")]
type PlatformHotkeyService = codex_voice_platform::MacOSHotkeyService;

#[cfg(target_os = "linux")]
fn platform_hotkey_service() -> Result<codex_voice_platform::LinuxHotkeyService> {
    Ok(codex_voice_platform::LinuxHotkeyService::new())
}
#[cfg(target_os = "windows")]
fn platform_hotkey_service() -> Result<codex_voice_platform::WindowsHotkeyService> {
    Ok(codex_voice_platform::WindowsHotkeyService::new())
}
#[cfg(target_os = "macos")]
fn platform_hotkey_service() -> Result<codex_voice_platform::MacOSHotkeyService> {
    Ok(codex_voice_platform::MacOSHotkeyService::new()?)
}

/// Builds the recorder/transcription/injector engine, starts the platform
/// hotkey service, and spawns the engine loop on its own task. Returns the
/// channels the shared run loop uses to drive it, plus the injector (which
/// doubles as the selected-text reader).
struct EngineControl {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl EngineControl {
    async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.await.context("dictation engine task panicked")?;
        Ok(())
    }
}

struct SpawnedPlatformEngine {
    app_rx: mpsc::Receiver<AppEvent>,
    hotkey_rx: mpsc::Receiver<HotkeyEvent>,
    engine_tx: mpsc::Sender<HotkeyEvent>,
    reader: Arc<PlatformTextInjector>,
    hotkey_service: PlatformHotkeyService,
    control: EngineControl,
}

async fn spawn_platform_engine(
    service_client: Option<LocalTranscriberClient>,
) -> Result<SpawnedPlatformEngine> {
    let injector = Arc::new(PlatformTextInjector::new());
    let audio = Arc::new(CpalWavRecorder::new());
    let resolved = match service_client {
        Some(client) => codex_voice_transcriber::transcription_backend_from_local(client),
        None => codex_voice_transcriber::resolve_transcription_backend().await?,
    };
    tracing::info!(backend = resolved.label, "selected transcription backend");
    let _ = logging::append_log_line(format!("transcription_backend={}", resolved.label));
    println!("transcription backend: {}", resolved.label);
    let transcription = Arc::new(resolved.client);
    let (app_tx, app_rx) = mpsc::channel(64);
    let (hotkey_tx, hotkey_rx) = mpsc::channel(16);
    let hotkey_service = platform_hotkey_service()?;
    hotkey_service.start(hotkey_tx)?;
    let engine = DictationEngine::new(audio, transcription, injector.clone(), app_tx);
    let (engine_tx, engine_rx) = mpsc::channel::<HotkeyEvent>(16);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_engine_loop(engine, engine_rx, shutdown_rx));
    Ok(SpawnedPlatformEngine {
        app_rx,
        hotkey_rx,
        engine_tx,
        reader: injector,
        hotkey_service,
        control: EngineControl {
            shutdown: Some(shutdown_tx),
            task,
        },
    })
}

/// The hotkey hint shown in the startup banner; only the chord differs.
fn speak_selection_hotkey_hint() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "Hold Control-M or the keyboard dictation key to dictate. Press Super-F6 to speak selected text."
    }
    #[cfg(target_os = "windows")]
    {
        "Hold Control-M to dictate. Press Win-F6 to speak selected text."
    }
    #[cfg(target_os = "macos")]
    {
        "Hold Control-M to dictate. Press Command-F6 to speak selected text."
    }
}

fn banner() -> String {
    format!("Codex Voice is running. {}", speak_selection_hotkey_hint())
}

/// Resolves the base URL of the transcriber/TTS HTTP service the desktop UI
/// talks to: reuses a desktop-ready service only at the canonical stable
/// loopback origin, otherwise self-hosts one there.
struct ResolvedServer {
    client: LocalTranscriberClient,
    embedded: Option<EmbeddedServiceHandle>,
}

pub(crate) const DESKTOP_ORIGIN: &str = "http://localhost:3846";

fn is_canonical_desktop_origin(root: &str) -> bool {
    root == DESKTOP_ORIGIN || root == "http://127.0.0.1:3846"
}

fn embedded_serve_config() -> codex_voice_transcriber::ServeConfig {
    codex_voice_transcriber::ServeConfig {
        bind: "127.0.0.1:3846"
            .parse()
            .expect("valid loopback socket addr"),
        codex_upload_limit_bytes: codex_voice_transcriber::DEFAULT_CODEX_UPLOAD_LIMIT_MIB
            * 1024
            * 1024,
        client_upload_limit_bytes: codex_voice_transcriber::DEFAULT_CLIENT_UPLOAD_LIMIT_MIB
            * 1024
            * 1024,
        chunk_seconds: codex_voice_transcriber::DEFAULT_CHUNK_SECONDS,
        token_env: codex_voice_transcriber::DEFAULT_TOKEN_ENV.to_string(),
        ffmpeg_binary: codex_voice_transcriber::DEFAULT_FFMPEG_BINARY.to_string(),
        no_auth: true,
        web_dist_override: None,
    }
}

impl ResolvedServer {
    async fn shutdown(self) -> Result<()> {
        if let Some(embedded) = self.embedded {
            embedded.shutdown().await?;
        }
        Ok(())
    }
}

async fn resolve_server() -> Result<ResolvedServer> {
    const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
    const RUNTIME_TIMEOUT: Duration = Duration::from_secs(60);
    const DESKTOP_READY_WAIT: Duration = Duration::from_secs(10);
    if let Some(client) = LocalTranscriberClient::discover(PROBE_TIMEOUT, RUNTIME_TIMEOUT).await {
        let root = client.web_root_url();
        if is_canonical_desktop_origin(&root) {
            if client
                .wait_for_desktop_ready(PROBE_TIMEOUT, DESKTOP_READY_WAIT)
                .await
            {
                tracing::info!(url = %root, "reusing desktop-ready service at the canonical origin");
                return Ok(ResolvedServer {
                    client,
                    embedded: None,
                });
            }
            anyhow::bail!("canonical desktop service did not become ready within 10 seconds");
        }
        tracing::info!(url = %root, "discovered service is not a desktop-ready canonical origin; self-hosting the desktop service");
    }

    if let Some(client) = LocalTranscriberClient::connect_desktop_origin(
        DESKTOP_ORIGIN,
        PROBE_TIMEOUT,
        RUNTIME_TIMEOUT,
    )
    .await
    {
        if client
            .wait_for_desktop_ready(PROBE_TIMEOUT, DESKTOP_READY_WAIT)
            .await
        {
            tracing::info!(
                url = DESKTOP_ORIGIN,
                "reusing an existing canonical desktop service"
            );
            return Ok(ResolvedServer {
                client,
                embedded: None,
            });
        }
        anyhow::bail!("canonical desktop service did not become ready within 10 seconds");
    }

    tracing::info!("no existing transcriber service found; self-hosting one");

    if codex_voice_transcriber::embedded_web_dist_is_stub() {
        anyhow::bail!(
            "desktop web UI is not built; run `mise run web-build` before `cargo run -p codex-voice-app --bin codex-voice -- run`"
        );
    }

    // Keep the embedded desktop origin stable across launches so browser
    // localStorage and IndexedDB remain attached to the same origin.
    let config = embedded_serve_config();
    let (speech, tts_config, tts_config_path) = tts::load_tts();
    let embedded =
        codex_voice_transcriber::start_embedded(config, speech, tts_config, tts_config_path)
            .await?;
    let client = embedded.client().clone();
    tracing::info!(url = %client.web_root_url(), "self-hosted transcriber service is ready");
    Ok(ResolvedServer {
        client,
        embedded: Some(embedded),
    })
}

/// Resolves when the process receives a termination request (SIGTERM or
/// Ctrl-C). Needed because the Tauri event loop does not exit on SIGTERM by
/// itself, which would leave `systemctl stop` hanging until SIGKILL.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    tracing::warn!(%error, "failed to install SIGTERM handler");
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            };
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Runs the shared select loop with a Tauri tray and window layer: resolves
/// the backing HTTP service, opens the tray (degrading to no tray on
/// failure), spawns the dictation engine, and drives `run_app` until quit.
async fn run_background(app_handle: tauri::AppHandle) -> Result<()> {
    // Server resolution failing must not take dictation down with it: fall
    // back to a window-less run where the tray still works and the speak
    // hotkey reports status instead of opening a window.
    let server = match resolve_server().await {
        Ok(server) => Some(server),
        Err(error) => {
            tracing::error!(%error, "speech service unavailable; running without windows");
            None
        }
    };
    let windows: Option<Arc<dyn AppWindows>> = server.as_ref().map(|server| {
        Arc::new(DesktopWindows::new(
            app_handle.clone(),
            server.client.clone(),
        )) as _
    });
    let tray_windows: Arc<dyn AppWindows> = windows
        .clone()
        .unwrap_or_else(|| Arc::new(UnavailableWindows));

    let tray = match TauriTray::start(UiStatus::idle(), app_handle, tray_windows) {
        Ok(tray) => Some(Box::new(tray) as Box<dyn app::TrayHandle>),
        Err(error) => {
            tracing::warn!(%error, "failed to start system tray; continuing without one");
            None
        }
    };

    let SpawnedPlatformEngine {
        app_rx,
        hotkey_rx,
        engine_tx,
        reader,
        hotkey_service: _hotkey_service,
        control,
    } = spawn_platform_engine(server.as_ref().map(|server| server.client.clone())).await?;

    let app = run_app(PlatformParts {
        hotkey_rx,
        app_rx,
        engine_tx,
        tray,
        reader,
        windows,
        banner: banner(),
    });
    let result = tokio::select! {
        result = app => result,
        _ = shutdown_signal() => {
            tracing::info!("termination signal received; shutting down");
            Ok(())
        }
    };
    control.shutdown().await?;
    if let Some(server) = server {
        server.shutdown().await?;
    }
    result
}

/// Runs the shared select loop with no tray and no windows, for environments
/// where Tauri cannot start (e.g. no display). Dictation and hotkeys still
/// work; the "speak selected text" hotkey degrades to reporting status only.
fn run_headless() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build headless tokio runtime")?;
    runtime.block_on(async move {
        // Reuse or self-host the local speech service just as the windowed path
        // does, so headless dictation uses the local transcriber instead of
        // hard-requiring Codex auth. On failure the engine's own backend
        // resolution falls back to direct Codex.
        let server = match resolve_server().await {
            Ok(server) => Some(server),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "local speech service unavailable; dictation will use the direct backend if available"
                );
                None
            }
        };
        let SpawnedPlatformEngine {
            app_rx,
            hotkey_rx,
            engine_tx,
            reader,
            hotkey_service: _hotkey_service,
            control,
        } = spawn_platform_engine(server.as_ref().map(|server| server.client.clone())).await?;
        let app = run_app(PlatformParts {
            hotkey_rx,
            app_rx,
            engine_tx,
            tray: None,
            reader,
            windows: None,
            banner: banner(),
        });
        let result = tokio::select! {
            result = app => result,
            _ = shutdown_signal() => {
                tracing::info!("termination signal received; shutting down");
                Ok(())
            }
        };
        control.shutdown().await?;
        if let Some(server) = server {
            server.shutdown().await?;
        }
        result
    })
}

/// Builds and runs the Tauri app: the dictation engine, hotkeys, server
/// discovery/self-hosting, and tray all run on a background thread started
/// from `setup`, so the main thread stays free for the Tauri event loop
/// (required on macOS). Falls back to a headless engine-only run if Tauri
/// itself cannot start.
fn run() -> Result<()> {
    // On Linux, GTK initialization inside the Tauri event-loop constructor
    // panics (rather than erroring) without a display, so the build-failure
    // fallback below never fires there. Probe for a display up front.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tracing::warn!("no DISPLAY or WAYLAND_DISPLAY; running headless engine-only");
        return run_headless();
    }

    let context = tauri::generate_context!();
    let app = match tauri::Builder::default()
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("codex-voice-run".into())
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            tracing::error!(%error, "failed to build background tokio runtime");
                            handle.exit(1);
                            return;
                        }
                    };
                    // Catch panics so the process always tears down cleanly:
                    // an unwind through `block_on` would otherwise skip
                    // `handle.exit` and drop the runtime (killing the signal
                    // handler), leaving the Tauri loop running with no engine
                    // and no SIGTERM path — a SIGKILL-only zombie.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        runtime.block_on(run_background(handle.clone()))
                    }));
                    let code = match outcome {
                        Ok(Ok(())) => 0,
                        Ok(Err(error)) => {
                            tracing::error!(%error, "run loop failed");
                            1
                        }
                        Err(_) => {
                            tracing::error!("run loop panicked; shutting down");
                            1
                        }
                    };
                    handle.exit(code);
                })?;
            Ok(())
        })
        .build(context)
    {
        Ok(app) => app,
        Err(error) => {
            tracing::warn!(%error, "Tauri unavailable; running headless engine-only");
            return run_headless();
        }
    };
    app.run(|_, event| {
        // The runtime requests exit when the last window closes; windows here
        // are transient viewers over a persistent tray/daemon, so veto it.
        // Explicit exits (quit menu, signals) carry a code and pass through.
        if let tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } = &event
        {
            api.prevent_exit();
        }
    });
    Ok(())
}

#[cfg(test)]
mod runtime_config_tests {
    use super::*;

    #[test]
    fn canonical_desktop_origin_accepts_localhost_alias_only_on_port_3846() {
        assert!(is_canonical_desktop_origin("http://localhost:3846"));
        assert!(is_canonical_desktop_origin("http://127.0.0.1:3846"));
        assert!(!is_canonical_desktop_origin("http://localhost:3845"));
        assert!(!is_canonical_desktop_origin("http://100.64.0.1:3846"));
    }

    #[test]
    fn embedded_service_uses_shared_server_defaults() {
        let config = embedded_serve_config();
        assert_eq!(
            config.codex_upload_limit_bytes,
            codex_voice_transcriber::DEFAULT_CODEX_UPLOAD_LIMIT_MIB * 1024 * 1024
        );
        assert_eq!(
            config.client_upload_limit_bytes,
            codex_voice_transcriber::DEFAULT_CLIENT_UPLOAD_LIMIT_MIB * 1024 * 1024
        );
        assert_eq!(
            config.chunk_seconds,
            codex_voice_transcriber::DEFAULT_CHUNK_SECONDS
        );
        assert_eq!(config.token_env, codex_voice_transcriber::DEFAULT_TOKEN_ENV);
        assert_eq!(
            config.ffmpeg_binary,
            codex_voice_transcriber::DEFAULT_FFMPEG_BINARY
        );
    }
}
