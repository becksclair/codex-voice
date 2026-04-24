use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use codex_voice_audio::CpalWavRecorder;
use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
use codex_voice_core::{
    AppEvent, AudioRecorder, DictationEngine, HotkeyService, PermissionService, TextInjector,
    TranscriptionClient,
};
#[cfg(target_os = "linux")]
use codex_voice_platform::{LinuxHotkeyService, LinuxPermissionService, LinuxTextInjector};
use std::{path::PathBuf, sync::Arc, time::Duration};
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codex_voice_app=info,codex_voice_audio=info".into()),
        )
        .init();

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

    let mut engine = DictationEngine::new(audio, transcription, injector, app_tx);
    println!("Codex Voice is running. Press Enter in this terminal to simulate Control-M until the portal hotkey adapter is wired.");

    loop {
        tokio::select! {
            Some(event) = hotkey_rx.recv() => engine.handle_hotkey(event).await,
            Some(event) = app_rx.recv() => print_app_event(event),
            else => break,
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn run() -> Result<()> {
    anyhow::bail!("this milestone implements Linux only")
}

async fn doctor_audio(args: AudioDoctor) -> Result<()> {
    let recorder = CpalWavRecorder::new();
    println!("starting audio capture for {} second(s)...", args.seconds);
    recorder
        .start()
        .await
        .context("failed to start audio capture")?;
    println!("recording...");
    tokio::time::sleep(Duration::from_secs(args.seconds)).await;
    println!("stopping audio capture...");
    let recording = recorder
        .stop()
        .await
        .context("failed to stop audio capture")?
        .context("audio recorder returned no recording")?;
    let size = std::fs::metadata(&recording.path)
        .with_context(|| format!("failed to stat {}", recording.path.display()))?
        .len();
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
    println!("Waiting for hotkey events. In this Linux milestone build, press Enter to simulate one press/release cycle.");
    for _ in 0..2 {
        if let Some(event) = rx.recv().await {
            println!("{event:?}");
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn doctor_hotkey() -> Result<()> {
    anyhow::bail!("hotkey diagnostics are implemented for Linux only")
}

#[cfg(target_os = "linux")]
async fn doctor_paste(text: String) -> Result<()> {
    let report = LinuxTextInjector::new().insert_text(&text).await?;
    println!("method: {:?}", report.method);
    println!("restored_clipboard: {}", report.restored_clipboard);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn doctor_paste(_text: String) -> Result<()> {
    anyhow::bail!("paste diagnostics are implemented for Linux only")
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
    anyhow::bail!("linux portal diagnostics are implemented for Linux only")
}

fn print_app_event(event: AppEvent) {
    match event {
        AppEvent::TranscriptReady { chars } => println!("transcript ready: {chars} chars"),
        AppEvent::Inserted(report) => println!("inserted via {:?}", report.method),
        other => println!("{other:?}"),
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
