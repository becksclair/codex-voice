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

pub async fn doctor_tts(args: TtsDoctor) -> Result<()> {
    let config_path = match args.read_aloud_config {
        Some(p) => p,
        None => ReadAloudConfigLoader::default_path()
            .context("failed to resolve default read-aloud config path")?,
    };

    println!("config_path: {}", config_path.display());

    let loader = ReadAloudConfigLoader::new(config_path.clone());
    let config = match loader.load() {
        Ok(c) => c,
        Err(e) => {
            println!("config_load: failed ({e})");
            return Err(anyhow::anyhow!("failed to load TTS config: {e}"));
        }
    };

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

    let client = ConfiguredSpeechClient::try_new(config)
        .context("failed to create TTS client from config")?;

    let format = match args.response_format.as_deref() {
        None | Some("") => SpeechFormat::Mp3,
        Some(format) => SpeechFormat::from_openai(format).ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported response_format: {format:?}; supported values are mp3, opus, aac, flac, wav, pcm"
            )
        })?,
    };

    let request = SpeechRequest {
        input: text.clone(),
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

    std::fs::write(&out_path, &speech.bytes)
        .with_context(|| format!("failed to write audio to {}", out_path.display()))?;
    println!("out_path: {}", out_path.display());

    if !args.keep {
        std::fs::remove_file(&out_path)
            .with_context(|| format!("failed to delete {}", out_path.display()))?;
        println!("deleted: true");
    } else {
        println!("deleted: false (kept because --keep)");
    }

    Ok(())
}
