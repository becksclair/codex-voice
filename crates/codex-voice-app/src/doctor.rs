use anyhow::{Context, Result};
use codex_voice_audio::{wav_duration, CpalWavRecorder};
use codex_voice_codex::CodexAuthService;
use codex_voice_core::{
    AudioRecorder, HotkeyService, PermissionService, RecordedAudio, TextInjector,
    TranscriptionClient,
};
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc;

#[cfg(target_os = "linux")]
use codex_voice_platform::{LinuxHotkeyService, LinuxPermissionService, LinuxTextInjector};
#[cfg(target_os = "windows")]
use codex_voice_platform::{WindowsHotkeyService, WindowsTextInjector};

use super::cli::AudioDoctor;

/// Generates linux/windows/other `#[cfg]` variants for a single async diagnostic fn.
macro_rules! platform_impl {
    (
        $vis:vis async fn $name:ident($($arg:ident: $ty:ty),*) -> Result<()>
        linux($linux_body:expr)
        windows($windows_body:expr)
        other($other_body:expr)
    ) => {
        #[cfg(target_os = "linux")]
        $vis async fn $name($($arg: $ty),*) -> Result<()> {
            $linux_body
        }

        #[cfg(target_os = "windows")]
        $vis async fn $name($($arg: $ty),*) -> Result<()> {
            $windows_body
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        $vis async fn $name($($arg: $ty),*) -> Result<()> {
            $other_body
        }
    };
}

pub async fn doctor_audio(args: AudioDoctor) -> Result<()> {
    println!("starting audio capture for {} second(s)...", args.seconds);
    println!("recording...");
    let (recording, size) = capture_audio_sample(Duration::from_secs(args.seconds)).await?;
    println!("stopping audio capture...");
    println!("path: {}", recording.path.display());
    println!("duration_ms: {}", recording.duration.as_millis());
    println!("content_type: {}", recording.content_type);
    println!("bytes: {size}");
    if !args.keep {
        tokio::fs::remove_file(&recording.path)
            .await
            .with_context(|| format!("failed to delete {}", recording.path.display()))?;
        println!("deleted: true");
    }
    Ok(())
}

pub async fn doctor_codex_auth() -> Result<()> {
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

pub async fn doctor_transcribe(file: PathBuf) -> Result<()> {
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
    let resolved = codex_voice_transcriber::resolve_transcription_backend().await?;
    println!("backend: {}", resolved.label);
    let transcript = resolved.client.transcribe(&recording).await?;
    let preview: String = transcript.chars().take(80).collect();
    println!("transcript_chars: {}", transcript.chars().count());
    println!("preview: {}", preview.replace('\n', " "));
    Ok(())
}

pub async fn doctor_hotkey() -> Result<()> {
    doctor_hotkey_platform().await
}

platform_impl! {
    async fn doctor_hotkey_platform() -> Result<()>
    linux({
        doctor_hotkey_generic(
            LinuxHotkeyService::new(),
            "Waiting for hotkey events. Hold and release Control-M or the keyboard dictation key after approving the KDE/Wayland GlobalShortcuts portal prompt.",
        )
        .await
    })
    windows({
        doctor_hotkey_generic(
            WindowsHotkeyService::new(),
            "Waiting for hotkey events. Hold and release Control-M within 30 seconds.",
        )
        .await
    })
    other(anyhow::bail!("hotkey diagnostics are implemented for Linux and Windows only"))
}

async fn doctor_hotkey_generic(service: impl HotkeyService, help_text: &str) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(8);
    service.start(tx)?;
    println!("{help_text}");
    for _ in 0..2 {
        let event = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .context("timed out waiting for hotkey event")?
            .context("hotkey listener stopped before emitting the expected events")?;
        println!("{event:?}");
    }
    Ok(())
}

pub async fn doctor_paste(text: String) -> Result<()> {
    doctor_paste_platform(text).await
}

platform_impl! {
    async fn doctor_paste_platform(text: String) -> Result<()>
    linux(doctor_paste_generic(LinuxTextInjector::new(), &text, None).await)
    windows({
        doctor_paste_generic(
            WindowsTextInjector::new(),
            &text,
            Some("SendInput may be blocked when targeting elevated apps from a non-elevated process"),
        )
        .await
    })
    other(anyhow::bail!("paste diagnostics are implemented for Linux and Windows only"))
}

async fn doctor_paste_generic(
    injector: impl TextInjector,
    text: &str,
    extra_note: Option<&str>,
) -> Result<()> {
    let report = injector.insert_text(text).await?;
    println!("method: {:?}", report.method);
    println!("restored_clipboard: {}", report.restored_clipboard);
    if let Some(note) = extra_note {
        println!("note: {note}");
    }
    Ok(())
}

pub async fn doctor_portals() -> Result<()> {
    doctor_portals_platform().await
}

platform_impl! {
    async fn doctor_portals_platform() -> Result<()>
    linux({
        println!(
            "XDG_SESSION_TYPE: {}",
            std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unset".into())
        );
        println!(
            "XDG_CURRENT_DESKTOP: {}",
            std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unset".into())
        );
        check_portals(LinuxPermissionService::new()).await
    })
    windows({
        println!("portal diagnostics are only available on Linux (Wayland portals)");
        Ok(())
    })
    other(anyhow::bail!("portal diagnostics are implemented for Linux only"))
}

async fn check_portals(service: impl PermissionService) -> Result<()> {
    for status in service.check().await? {
        println!(
            "{:?}: available={} granted={:?} detail={}",
            status.kind, status.available, status.granted, status.detail
        );
    }
    Ok(())
}

pub async fn capture_audio_sample(duration: Duration) -> Result<(RecordedAudio, u64)> {
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
    let size = match tokio::fs::metadata(&recording.path).await {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            let _ = tokio::fs::remove_file(&recording.path).await;
            return Err(error)
                .with_context(|| format!("failed to stat {}", recording.path.display()));
        }
    };
    Ok((recording, size))
}

pub fn redact_account(account_id: &str) -> String {
    let mut chars = account_id.chars();
    let prefix: String = chars.by_ref().take(6).collect();
    let total = prefix.chars().count() + chars.count();
    if total <= 6 {
        "present_redacted".into()
    } else {
        format!("{prefix}...")
    }
}
