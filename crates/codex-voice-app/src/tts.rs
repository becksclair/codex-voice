use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest};
use codex_voice_tts::config::{SpeechPrepConfig, SpeechPrepMode, SpeechPrepProviderKind};
use codex_voice_tts::{
    collect_bracket_tags, ConfiguredSpeechClient, ReadAloudConfigLoader, SpeechPrepClient,
};

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

pub fn default_read_aloud_config_path() -> Result<PathBuf> {
    ReadAloudConfigLoader::default_path()
        .context("failed to resolve default read-aloud config path")
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

/// Fixed long sample used by the speech-prep benchmark. Copied verbatim from
/// the deprecated `scripts/tts_prep_benchmark.py` so measurements stay
/// comparable across the Python and Rust harnesses.
const DEFAULT_SAMPLE: &str = r#"Mara had meant to leave before the rain came, but the clouds folded themselves over the roofs with the quiet certainty of a verdict. By the time she reached the old arcade, the gutters were already spilling silver threads onto the pavement, and every shop window trembled with reflections of people hurrying home. She stopped beneath the striped awning of the watchmaker's door and held the letter against her coat as if warmth alone might change what it said.

Inside, somewhere beyond the glass, a hundred clocks disagreed about the hour. Their ticking pressed through the wood like nervous fingertips. Mara laughed once, not because anything was funny, but because the sound was the only thing that kept her from crying. She had read the letter twice on the tram and once again under the station lamp, and each reading had made the words simpler and harder: her brother was alive, he was nearby, and he had waited seven years to ask forgiveness.

A tram bell rang at the corner. The city answered with the hiss of tires and rain. Mara imagined him as he had been at nineteen, proud enough to wound anyone who loved him, frightened enough to call it freedom. She remembered the night he left, her mother's hands white around a teacup, her father sitting very straight, and herself on the stairs, too young to be included and old enough to understand that something had broken.

The watchmaker opened the door behind her. Warm air slipped out, smelling of brass polish and black tea. "Miss Vale?" he asked, peering over his spectacles.

Mara turned. For a moment she could not speak. The letter had told her to come here, but not what waited inside, not whether forgiveness would look like a man, a grave, or another door.

"Yes," she said at last, and the word came out smaller than she intended.

The watchmaker stepped aside. "He's been waiting since noon."

Mara looked once more at the rain shining in the street. Then she folded the letter carefully, as though it were something fragile and alive, and went in."#;

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const PREVIEW_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchProvider {
    Codex,
    Google,
}

impl BenchProvider {
    fn as_str(self) -> &'static str {
        match self {
            BenchProvider::Codex => "codex",
            BenchProvider::Google => "google",
        }
    }
}

/// A prep-model benchmark target. Mirrors the `DEFAULT_TARGETS` table from the
/// deprecated Python benchmark so the default model set stays identical.
#[derive(Debug, Clone, Copy)]
struct BenchTarget {
    name: &'static str,
    provider: BenchProvider,
    model: &'static str,
    reasoning_effort: Option<&'static str>,
    timeout_secs: u64,
}

const DEFAULT_TARGETS: &[BenchTarget] = &[
    BenchTarget {
        name: "gpt-5.3-codex-spark-normal",
        provider: BenchProvider::Codex,
        model: "gpt-5.3-codex-spark",
        reasoning_effort: Some("medium"),
        timeout_secs: 120,
    },
    BenchTarget {
        name: "gpt-5.4-mini-none",
        provider: BenchProvider::Codex,
        model: "gpt-5.4-mini",
        reasoning_effort: None,
        timeout_secs: 120,
    },
    BenchTarget {
        name: "gpt-5.5-none",
        provider: BenchProvider::Codex,
        model: "gpt-5.5",
        reasoning_effort: None,
        timeout_secs: 120,
    },
    BenchTarget {
        name: "gemini-3-flash-preview",
        provider: BenchProvider::Google,
        model: "google/gemini-3-flash-preview",
        reasoning_effort: None,
        timeout_secs: 30,
    },
    BenchTarget {
        name: "gemini-3.5-flash",
        provider: BenchProvider::Google,
        model: "google/gemini-3.5-flash",
        reasoning_effort: None,
        timeout_secs: 30,
    },
];

#[derive(Debug, Args)]
pub struct TtsBenchArgs {
    /// Inline sample text to benchmark. Defaults to the fixed embedded sample.
    #[arg(long, conflicts_with = "file")]
    pub text: Option<String>,
    /// Read the sample text from a file instead of the embedded default.
    #[arg(long, conflicts_with = "text")]
    pub file: Option<PathBuf>,
    /// Comma-separated target names to run. Defaults to the full default set.
    #[arg(long, value_delimiter = ',')]
    pub models: Option<Vec<String>>,
    /// Number of times to run each target.
    #[arg(long, default_value_t = 1)]
    pub iterations: u32,
    /// Print the planned requests without issuing any network calls.
    #[arg(long)]
    pub dry_run: bool,
    /// Path to `read-aloud-defaults.json` (defaults to the standard location).
    #[arg(long)]
    pub read_aloud_config: Option<PathBuf>,
    /// Base URL for Codex-provider targets.
    #[arg(long, default_value = DEFAULT_CODEX_BASE_URL)]
    pub codex_base_url: String,
    /// Path to the Codex auth file (defaults to `~/.codex/auth.json`).
    #[arg(long)]
    pub codex_auth_file: Option<PathBuf>,
}

/// Resolve the requested target names against the default table.
fn select_targets(models: &Option<Vec<String>>) -> Result<Vec<BenchTarget>> {
    let Some(names) = models else {
        return Ok(DEFAULT_TARGETS.to_vec());
    };
    let mut selected = Vec::with_capacity(names.len());
    let mut unknown = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        match DEFAULT_TARGETS.iter().find(|t| t.name == name) {
            Some(target) => selected.push(*target),
            None => unknown.push(name.to_string()),
        }
    }
    if !unknown.is_empty() {
        let known = DEFAULT_TARGETS
            .iter()
            .map(|t| t.name)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "unknown target(s): {}; known targets: {known}",
            unknown.join(", ")
        );
    }
    if selected.is_empty() {
        anyhow::bail!("no targets selected");
    }
    Ok(selected)
}

/// Collapse whitespace and truncate to a short single-line preview. Never
/// prints secrets; benchmark output is model-transformed sample text only.
fn preview(text: &str, max: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = flattened.chars().take(max).collect::<String>();
    if flattened.chars().count() > max {
        out.push('…');
    }
    out
}

fn resolve_sample(args: &TtsBenchArgs) -> Result<String> {
    match (&args.text, &args.file) {
        (Some(text), _) => Ok(text.clone()),
        (None, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read sample text from {}", path.display())),
        (None, None) => Ok(DEFAULT_SAMPLE.to_string()),
    }
}

fn default_codex_auth_file() -> Result<PathBuf> {
    let home =
        dirs::home_dir().context("could not resolve home directory for the Codex auth file")?;
    Ok(home.join(".codex").join("auth.json"))
}

/// Build a per-target speech-prep config by overriding the resolved template.
fn build_target_config(
    target: &BenchTarget,
    template: &SpeechPrepConfig,
    resolved: &codex_voice_tts::ResolvedTtsConfig,
    codex_base_url: &str,
    codex_auth_file: &Path,
) -> Result<SpeechPrepConfig> {
    let timeout = std::time::Duration::from_secs(target.timeout_secs);
    let mut config = template.clone();
    config.mode = SpeechPrepMode::PerformanceTags;
    config.model = target.model.to_string();
    config.fallback_models = Vec::new();
    config.reasoning_effort = target.reasoning_effort.map(str::to_string);
    config.attempt_timeout = timeout;
    config.timeout = timeout;

    match target.provider {
        BenchProvider::Google => {
            let (api_key, base_url) = if let Some(google) = &resolved.google {
                (google.api_key.clone(), google.base_url.clone())
            } else if template.provider == SpeechPrepProviderKind::Google {
                let key = template
                    .api_key
                    .clone()
                    .context("Google speech-prep API key not found in read-aloud defaults")?;
                (key, template.base_url.clone())
            } else {
                anyhow::bail!(
                    "Google provider is not configured in read-aloud defaults; cannot benchmark target {}",
                    target.name
                );
            };
            config.provider = SpeechPrepProviderKind::Google;
            config.api_key = Some(api_key);
            config.base_url = base_url;
            config.auth_file = None;
        }
        BenchProvider::Codex => {
            config.provider = SpeechPrepProviderKind::Codex;
            config.api_key = None;
            config.base_url = codex_base_url.trim_end_matches('/').to_string();
            config.auth_file = Some(codex_auth_file.to_path_buf());
        }
    }
    Ok(config)
}

pub async fn run_tts_bench(args: TtsBenchArgs) -> Result<()> {
    anyhow::ensure!(args.iterations >= 1, "--iterations must be at least 1");
    let targets = select_targets(&args.models)?;
    let sample = resolve_sample(&args)?;
    let input_chars = sample.chars().count();

    if args.dry_run {
        println!("dry_run: true");
        println!("iterations: {}", args.iterations);
        println!("input_chars: {input_chars}");
        println!("prompt_mode: performance-tags (inline-tags)");
        println!("model\tprovider\tprompt_mode\tinput_chars");
        for target in &targets {
            println!(
                "{}\t{}\tperformance-tags\t{}",
                target.name,
                target.provider.as_str(),
                input_chars
            );
        }
        println!("note: no network requests issued (dry-run)");
        return Ok(());
    }

    // Live run: load config for Google credentials and the speech-prep template.
    let loader = match args.read_aloud_config.clone() {
        Some(path) => ReadAloudConfigLoader::new(path),
        None => ReadAloudConfigLoader::new(
            ReadAloudConfigLoader::default_path()
                .context("failed to resolve default read-aloud config path")?,
        ),
    };
    let resolved = loader
        .load()
        .context("failed to load read-aloud defaults (needed for the benchmark)")?;
    let template = resolved.speech_prep.clone().context(
        "speech prep is disabled in read-aloud defaults; enable messages.tts.speechPrep to benchmark",
    )?;
    let codex_auth_file = match args.codex_auth_file.clone() {
        Some(path) => path,
        None => default_codex_auth_file()?,
    };

    println!("iterations: {}", args.iterations);
    println!("input_chars: {input_chars}");
    if args.iterations > 1 {
        println!("model\titeration\telapsed_ms\tchars\ttags\tpreview");
    } else {
        println!("model\telapsed_ms\tchars\ttags\tpreview");
    }

    let mut failures = 0_usize;
    let mut runs = 0_usize;
    for target in &targets {
        let config = build_target_config(
            target,
            &template,
            &resolved,
            &args.codex_base_url,
            &codex_auth_file,
        )?;
        let client = SpeechPrepClient::new(config)
            .with_context(|| format!("failed to build speech-prep client for {}", target.name))?;
        for iteration in 1..=args.iterations {
            runs += 1;
            eprintln!("running {} (iteration {iteration})...", target.name);
            let start = std::time::Instant::now();
            let outcome = client.benchmark(&sample).await;
            let elapsed_ms = start.elapsed().as_millis();
            match outcome {
                Ok(output) => {
                    let chars = output.chars().count();
                    let tags = collect_bracket_tags(&output).len();
                    let preview = preview(&output, PREVIEW_CHARS);
                    if args.iterations > 1 {
                        println!(
                            "{}\t{iteration}\t{elapsed_ms}\t{chars}\t{tags}\t{preview}",
                            target.name
                        );
                    } else {
                        println!("{}\t{elapsed_ms}\t{chars}\t{tags}\t{preview}", target.name);
                    }
                }
                Err(error) => {
                    failures += 1;
                    if args.iterations > 1 {
                        println!(
                            "{}\t{iteration}\t{elapsed_ms}\t0\t0\tERROR: {error}",
                            target.name
                        );
                    } else {
                        println!("{}\t{elapsed_ms}\t0\t0\tERROR: {error}", target.name);
                    }
                }
            }
        }
    }

    if failures > 0 {
        anyhow::bail!("{failures} of {runs} benchmark run(s) failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_targets_defaults_to_full_set() {
        let targets = select_targets(&None).unwrap();
        assert_eq!(targets.len(), DEFAULT_TARGETS.len());
    }

    #[test]
    fn select_targets_filters_by_name_preserving_order() {
        let targets = select_targets(&Some(vec![
            "gemini-3.5-flash".into(),
            "gpt-5.5-none".into(),
        ]))
        .unwrap();
        let names = targets.iter().map(|t| t.name).collect::<Vec<_>>();
        assert_eq!(names, vec!["gemini-3.5-flash", "gpt-5.5-none"]);
    }

    #[test]
    fn select_targets_rejects_unknown_names() {
        let error = select_targets(&Some(vec!["nope".into()])).unwrap_err();
        assert!(error.to_string().contains("unknown target(s): nope"));
    }

    #[test]
    fn select_targets_ignores_blank_entries() {
        let targets = select_targets(&Some(vec!["".into(), "gpt-5.5-none".into()])).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "gpt-5.5-none");
    }

    #[test]
    fn preview_collapses_whitespace_and_truncates() {
        let text = "[softly]  hello\n\nworld this is a longer preview than the limit allows here";
        let out = preview(text, 20);
        assert_eq!(out.chars().count(), 21); // 20 chars + ellipsis
        assert!(out.starts_with("[softly] hello world"));
        assert!(out.ends_with('…'));
        assert!(!out.contains('\n'));
    }

    #[test]
    fn preview_short_text_has_no_ellipsis() {
        assert_eq!(preview("[warm] hi", 80), "[warm] hi");
    }

    #[test]
    fn default_targets_match_python_benchmark_set() {
        let names = DEFAULT_TARGETS.iter().map(|t| t.name).collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "gpt-5.3-codex-spark-normal",
                "gpt-5.4-mini-none",
                "gpt-5.5-none",
                "gemini-3-flash-preview",
                "gemini-3.5-flash",
            ]
        );
    }

    #[test]
    fn resolve_sample_prefers_inline_text_then_default() {
        let inline = TtsBenchArgs {
            text: Some("custom".into()),
            file: None,
            models: None,
            iterations: 1,
            dry_run: true,
            read_aloud_config: None,
            codex_base_url: DEFAULT_CODEX_BASE_URL.into(),
            codex_auth_file: None,
        };
        assert_eq!(resolve_sample(&inline).unwrap(), "custom");

        let defaulted = TtsBenchArgs {
            text: None,
            file: None,
            models: None,
            iterations: 1,
            dry_run: true,
            read_aloud_config: None,
            codex_base_url: DEFAULT_CODEX_BASE_URL.into(),
            codex_auth_file: None,
        };
        assert_eq!(resolve_sample(&defaulted).unwrap(), DEFAULT_SAMPLE);
    }
}
