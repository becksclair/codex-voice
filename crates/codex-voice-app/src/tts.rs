use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest};
use codex_voice_tts::{ConfiguredSpeechClient, ReadAloudConfigLoader};

#[derive(Debug, Args)]
pub struct TtsDoctor {
    #[arg(long)]
    pub text: Option<String>,
    #[arg(long)]
    pub voice: Option<String>,
    #[arg(long)]
    pub response_format: Option<String>,
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long)]
    pub keep: bool,
    #[arg(long)]
    pub read_aloud_config: Option<PathBuf>,
}

/// Load TTS configuration and build a configured speech client.
///
/// Resolves the config path (explicit or default), loads the config,
/// creates the client, and verifies that at least one provider is usable.
pub fn load_speech_client(path: Option<PathBuf>) -> Result<ConfiguredSpeechClient> {
    let config_path = match path {
        Some(p) => p,
        None => ReadAloudConfigLoader::default_path()
            .context("failed to resolve default read-aloud config path")?,
    };

    let loader = ReadAloudConfigLoader::new(config_path);
    let config = loader.load().context("failed to load read-aloud config")?;
    let client = ConfiguredSpeechClient::try_new(config)
        .context("failed to create TTS client from config")?;
    if !client.has_any_provider() {
        return Err(anyhow::anyhow!(
            "TTS config parsed but no usable provider is configured"
        ));
    }
    Ok(client)
}

pub async fn doctor_tts(args: TtsDoctor) -> Result<()> {
    let client = load_speech_client(args.read_aloud_config.clone())?;
    let config = client.config();

    if let Some(ref path) = args.read_aloud_config {
        println!("config_path: {}", path.display());
    } else if let Ok(default) = ReadAloudConfigLoader::default_path() {
        println!("config_path: {}", default.display());
    }

    println!("config_load: ok");
    println!("default_provider: {:?}", config.default_provider);
    if let Some(ref persona) = config.default_persona {
        println!("default_persona: {persona}");
    }
    println!("max_text_length: {}", config.max_text_length);
    println!("google_configured: {}", config.google.is_some());
    println!("elevenlabs_configured: {}", config.elevenlabs.is_some());

    // If no text provided, stop after config diagnostics.
    let Some(text) = args.text else {
        println!("synthesis: skipped (no --text)");
        return Ok(());
    };

    let format = match args.response_format.as_deref() {
        None | Some("") => SpeechFormat::Mp3,
        Some(format) => SpeechFormat::from_openai(format).ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported response_format: {format:?}; supported values are mp3, opus, aac, flac, wav, pcm"
            )
        })?,
    };

    let request = SpeechRequest {
        input: text,
        model_hint: "gpt-4o-mini-tts".to_string(),
        voice_hint: args.voice,
        instructions: None,
        format,
        speed: None,
    };

    println!("synthesizing...");
    let start = std::time::Instant::now();
    let speech = client
        .synthesize(&request)
        .await
        .context("TTS synthesis failed")?;
    let elapsed = start.elapsed();

    println!("synthesis: ok");
    println!("elapsed_ms: {}", elapsed.as_millis());
    println!("content_type: {}", speech.mime_type);
    println!("bytes: {}", speech.bytes.len());

    let out_path = args.out.unwrap_or_else(|| {
        let ext = match format {
            SpeechFormat::Mp3 => "mp3",
            SpeechFormat::Opus => "opus",
            SpeechFormat::Aac => "aac",
            SpeechFormat::Flac => "flac",
            SpeechFormat::Wav => "wav",
            SpeechFormat::Pcm => "pcm",
        };
        std::env::temp_dir().join(format!("codex-voice-tts-{}.{ext}", std::process::id()))
    });

    tokio::fs::write(&out_path, &speech.bytes)
        .await
        .with_context(|| format!("failed to write audio to {}", out_path.display()))?;
    println!("out_path: {}", out_path.display());

    if !args.keep {
        tokio::fs::remove_file(&out_path)
            .await
            .with_context(|| format!("failed to delete {}", out_path.display()))?;
        println!("deleted: true");
    } else {
        println!("deleted: false (kept because --keep)");
    }

    Ok(())
}
