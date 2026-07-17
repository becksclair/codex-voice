use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use codex_voice_core::SpeechError;

use super::models::{
    ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ElevenLabsVoiceSettings, GooglePersonaConfig,
    GoogleRuntimeConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig, SpeechPrepConfig,
    SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies, SpeechPrepStrategy,
};
use super::serde::{
    AdvancedSpeechPrepConfig, ElevenLabsVoiceSettingsInput, SpeechPrepModeInput,
    SpeechPrepProviderInput, SpeechPrepStrategiesInput, SpeechPrepStrategyInput, VoiceBackend,
    VoiceConfig, VoiceConfigFile,
};

const CONFIG_VERSION: u8 = 1;
const DEFAULT_GOOGLE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const DEFAULT_ELEVENLABS_BASE_URL: &str = "https://api.elevenlabs.io";
const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_CODEX_SPEECH_PREP_MODEL: &str = "gpt-5.6-luna";
const DEFAULT_GOOGLE_SPEECH_PREP_MODEL: &str = "google/gemini-3.5-flash";
const DEFAULT_GOOGLE_VOICE: &str = "Sulafat";
const DEFAULT_GOOGLE_MAX_INPUT_CHARS: usize = 6_000;
const DEFAULT_ELEVENLABS_MAX_INPUT_CHARS: usize = 6_000;
const DEFAULT_ELEVENLABS_V3_MAX_INPUT_CHARS: usize = 5_000;
const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_PREP_THRESHOLD_CHARS: usize = 120;
const DEFAULT_PREP_MAX_INPUT_CHARS: usize = 12_000;
const DEFAULT_PREP_MAX_OUTPUT_CHARS: usize = 6_000;
const DEFAULT_PREP_TIMEOUT_MS: u64 = 30_000;
const MIN_TIMEOUT_MS: u64 = 250;
const MAX_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_SHORTEN_MIN_OUTPUT_CHARS: usize = 4_000;
const DEFAULT_TAG_PALETTE: &[&str] = &[
    "excited",
    "delighted",
    "playful",
    "brightly",
    "nervous",
    "uneasy",
    "fearful",
    "frustrated",
    "angry",
    "stern",
    "sorrowful",
    "wistful",
    "choked up",
    "calm",
    "reassuring",
    "tender",
    "vulnerable",
    "affectionate",
    "proud",
    "determined",
    "amused",
    "dryly",
    "deadpan",
    "relieved",
    "sleepy",
    "serious",
    "urgent",
    "teasing",
    "warmly",
    "softly",
    "flatly",
    "breathless",
    "sigh",
    "laughs",
    "laughing",
    "gasps",
    "whispers",
    "exhales",
    "shaky breath",
    "light chuckle",
    "snorts",
    "scoffs",
    "sigh of relief",
    "hesitates",
    "pause",
    "long pause",
    "voice breaks",
    "swallows",
    "leans closer",
    "under breath",
    "smiling",
    "moan",
];

pub fn resolve_file(file: VoiceConfigFile) -> Result<ResolvedTtsConfig, SpeechError> {
    if file.version != CONFIG_VERSION {
        return config_error("$.version", format!("expected {CONFIG_VERSION}"));
    }
    if file.voices.is_empty() {
        return config_error("$.voices", "at least one voice is required");
    }
    if file.voices.keys().any(|name| name.trim().is_empty()) {
        return config_error("$.voices", "voice names must not be empty");
    }
    if !file.voices.contains_key(&file.default_voice) {
        return config_error(
            "$.defaultVoice",
            format!("voice {:?} is not defined", file.default_voice),
        );
    }
    if file.providers.google.is_none() && google_overrides_present(&file.advanced.providers.google)
    {
        return config_error(
            "$.advanced.providers.google",
            "Google overrides require $.providers.google",
        );
    }
    if file.providers.elevenlabs.is_none()
        && elevenlabs_overrides_present(&file.advanced.providers.elevenlabs)
    {
        return config_error(
            "$.advanced.providers.elevenlabs",
            "ElevenLabs overrides require $.providers.elevenlabs",
        );
    }

    let google = file
        .providers
        .google
        .map(|provider| resolve_google(provider, &file.advanced.providers.google))
        .transpose()?;
    let elevenlabs = file
        .providers
        .elevenlabs
        .map(|provider| resolve_elevenlabs(provider, &file.advanced.providers.elevenlabs))
        .transpose()?;

    if google.is_none() && elevenlabs.is_none() {
        return config_error("$.providers", "at least one provider is required");
    }

    let personas = file
        .voices
        .into_iter()
        .map(|(name, voice)| {
            resolve_voice(&name, voice, google.is_some(), elevenlabs.is_some())
                .map(|voice| (name, voice))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let default = personas.get(&file.default_voice).expect("validated above");
    let max_text_length = google
        .as_ref()
        .map(|provider| provider.max_text_length)
        .into_iter()
        .chain(elevenlabs.as_ref().map(|provider| provider.max_text_length))
        .max()
        .unwrap_or(DEFAULT_GOOGLE_MAX_INPUT_CHARS);
    let timeout = google
        .as_ref()
        .map(|provider| provider.timeout)
        .into_iter()
        .chain(elevenlabs.as_ref().map(|provider| provider.timeout))
        .max()
        .unwrap_or(Duration::from_millis(DEFAULT_PROVIDER_TIMEOUT_MS));
    let speech_prep = resolve_speech_prep(&file.advanced.speech_prep, google.as_ref())?;

    Ok(ResolvedTtsConfig {
        default_provider: default.provider,
        default_persona: Some(file.default_voice),
        max_text_length,
        timeout,
        speech_prep,
        google,
        elevenlabs,
        personas,
    })
}

fn resolve_google(
    config: super::serde::GoogleProviderConfig,
    advanced: &super::serde::AdvancedGoogleProviderConfig,
) -> Result<GoogleRuntimeConfig, SpeechError> {
    let models = validate_models("$.providers.google.models", config.models)?;
    validate_env_name(
        advanced.api_key_env.as_deref(),
        "$.advanced.providers.google.apiKeyEnv",
    )?;
    let api_key = crate::secret::resolve_provider_key(
        advanced.api_key_env.as_deref(),
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
    )?;
    let base_url = resolve_url(
        advanced.base_url.as_deref(),
        DEFAULT_GOOGLE_BASE_URL,
        "$.advanced.providers.google.baseUrl",
    )?;
    let timeout = resolve_timeout(
        advanced.timeout_ms,
        DEFAULT_PROVIDER_TIMEOUT_MS,
        "$.advanced.providers.google.timeoutMs",
    )?;
    let max_text_length = resolve_max_chars(
        advanced.max_input_chars,
        DEFAULT_GOOGLE_MAX_INPUT_CHARS,
        "$.advanced.providers.google.maxInputChars",
    )?;
    Ok(GoogleRuntimeConfig {
        api_key,
        base_url,
        voice: DEFAULT_GOOGLE_VOICE.to_string(),
        models,
        inline_audio_tags: advanced.inline_audio_tags,
        max_text_length,
        timeout,
    })
}

fn resolve_elevenlabs(
    config: super::serde::ElevenLabsProviderConfig,
    advanced: &super::serde::AdvancedElevenLabsProviderConfig,
) -> Result<ElevenLabsRuntimeConfig, SpeechError> {
    let models = validate_models("$.providers.elevenlabs.models", config.models)?;
    if !matches!(config.text_normalization.as_str(), "auto" | "on" | "off") {
        return config_error(
            "$.providers.elevenlabs.textNormalization",
            "expected auto, on, or off",
        );
    }
    if !config.stream_gain.is_finite() || !(0.1..=8.0).contains(&config.stream_gain) {
        return config_error(
            "$.providers.elevenlabs.streamGain",
            "expected a finite number from 0.1 through 8.0",
        );
    }
    validate_env_name(
        advanced.api_key_env.as_deref(),
        "$.advanced.providers.elevenlabs.apiKeyEnv",
    )?;
    let api_key = crate::secret::resolve_provider_key(
        advanced.api_key_env.as_deref(),
        "ELEVENLABS_API_KEY",
        "ELEVEN_API_KEY",
    )?;
    let base_url = resolve_url(
        advanced.base_url.as_deref(),
        DEFAULT_ELEVENLABS_BASE_URL,
        "$.advanced.providers.elevenlabs.baseUrl",
    )?;
    let timeout = resolve_timeout(
        advanced.timeout_ms,
        DEFAULT_PROVIDER_TIMEOUT_MS,
        "$.advanced.providers.elevenlabs.timeoutMs",
    )?;
    let default_max = if elevenlabs_v3(&models[0]) {
        DEFAULT_ELEVENLABS_V3_MAX_INPUT_CHARS
    } else {
        DEFAULT_ELEVENLABS_MAX_INPUT_CHARS
    };
    let max_text_length = resolve_max_chars(
        advanced.max_input_chars,
        default_max,
        "$.advanced.providers.elevenlabs.maxInputChars",
    )?;
    let language_code = advanced
        .language_code
        .as_deref()
        .map(|value| {
            validate_nonempty(value, "$.advanced.providers.elevenlabs.languageCode")?;
            Ok::<_, SpeechError>(value.trim().to_string())
        })
        .transpose()?;
    let output_format = advanced
        .output_format
        .as_deref()
        .map(|value| {
            validate_nonempty(value, "$.advanced.providers.elevenlabs.outputFormat")?;
            Ok::<_, SpeechError>(value.trim().to_string())
        })
        .transpose()?
        .unwrap_or_else(|| "mp3_44100_128".to_string());
    Ok(ElevenLabsRuntimeConfig {
        api_key,
        base_url,
        models,
        apply_text_normalization: config.text_normalization,
        output_format,
        stream_gain: config.stream_gain,
        language_code,
        inline_audio_tags: advanced.inline_audio_tags,
        max_text_length,
        max_text_length_overridden: advanced.max_input_chars.is_some(),
        timeout,
    })
}

fn resolve_voice(
    name: &str,
    config: VoiceConfig,
    google_configured: bool,
    elevenlabs_configured: bool,
) -> Result<ResolvedPersona, SpeechError> {
    let path = format!("$.voices.{name}");
    validate_nonempty(&config.label, &format!("{path}.label"))?;
    validate_nonempty(&config.description, &format!("{path}.description"))?;
    if config.backends.is_empty() {
        return config_error(
            format!("{path}.backends"),
            "at least one backend is required",
        );
    }
    let mut seen = HashSet::new();
    let mut provider_order = Vec::with_capacity(config.backends.len());
    let mut google = None;
    let mut elevenlabs = None;
    for (index, backend) in config.backends.into_iter().enumerate() {
        let backend_path = format!("{path}.backends[{index}]");
        match backend {
            VoiceBackend::Google { voice } => {
                if !google_configured {
                    return config_error(backend_path, "Google provider is not configured");
                }
                if !seen.insert(ProviderKind::Google) {
                    return config_error(backend_path, "duplicate Google backend");
                }
                validate_nonempty(&voice, &format!("{backend_path}.voice"))?;
                provider_order.push(ProviderKind::Google);
                google = Some(GooglePersonaConfig { voice_name: voice });
            }
            VoiceBackend::Elevenlabs { voice_id, settings } => {
                if !elevenlabs_configured {
                    return config_error(backend_path, "ElevenLabs provider is not configured");
                }
                if !seen.insert(ProviderKind::ElevenLabs) {
                    return config_error(backend_path, "duplicate ElevenLabs backend");
                }
                validate_nonempty(&voice_id, &format!("{backend_path}.voiceId"))?;
                validate_voice_settings(&settings, &format!("{backend_path}.settings"))?;
                provider_order.push(ProviderKind::ElevenLabs);
                elevenlabs = Some(ElevenLabsPersonaConfig {
                    voice_id,
                    voice_settings: ElevenLabsVoiceSettings {
                        stability: settings.stability,
                        similarity_boost: settings.similarity_boost,
                        style: settings.style,
                        use_speaker_boost: settings.speaker_boost,
                        speed: settings.speed,
                    },
                });
            }
        }
    }
    Ok(ResolvedPersona {
        label: config.label,
        description: config.description,
        provider: provider_order[0],
        provider_order,
        prompt_scene: clean_optional(config.prompt.scene),
        prompt_sample_context: clean_optional(config.prompt.sample_context),
        prompt_style: clean_optional(config.prompt.style),
        prompt_pacing: clean_optional(config.prompt.pace),
        prompt_constraints: config
            .prompt
            .constraints
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        google,
        elevenlabs,
    })
}

fn resolve_speech_prep(
    advanced: &AdvancedSpeechPrepConfig,
    google: Option<&GoogleRuntimeConfig>,
) -> Result<Option<SpeechPrepConfig>, SpeechError> {
    let provider = match advanced.provider.unwrap_or(SpeechPrepProviderInput::Codex) {
        SpeechPrepProviderInput::Google => SpeechPrepProviderKind::Google,
        SpeechPrepProviderInput::Codex => SpeechPrepProviderKind::Codex,
    };
    let default_model = match provider {
        SpeechPrepProviderKind::Google => DEFAULT_GOOGLE_SPEECH_PREP_MODEL,
        SpeechPrepProviderKind::Codex => DEFAULT_CODEX_SPEECH_PREP_MODEL,
    };
    let models = validate_models(
        "$.advanced.speechPrep.models",
        advanced
            .models
            .clone()
            .unwrap_or_else(|| vec![default_model.to_string()]),
    )?;
    let mode = match advanced
        .mode
        .unwrap_or(SpeechPrepModeInput::PerformanceTags)
    {
        SpeechPrepModeInput::Shorten => SpeechPrepMode::Shorten,
        SpeechPrepModeInput::PerformanceTags => SpeechPrepMode::PerformanceTags,
    };
    if let Some(base_url) = advanced.base_url.as_deref() {
        resolve_url(
            Some(base_url),
            DEFAULT_CODEX_BASE_URL,
            "$.advanced.speechPrep.baseUrl",
        )?;
    }
    if advanced
        .auth_file
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return config_error("$.advanced.speechPrep.authFile", "must not be empty");
    }
    let total_timeout = resolve_timeout(
        advanced.total_timeout_ms,
        DEFAULT_PREP_TIMEOUT_MS,
        "$.advanced.speechPrep.totalTimeoutMs",
    )?;
    let attempt_timeout = resolve_timeout(
        advanced.attempt_timeout_ms,
        DEFAULT_PREP_TIMEOUT_MS,
        "$.advanced.speechPrep.attemptTimeoutMs",
    )?;
    if attempt_timeout > total_timeout {
        return config_error(
            "$.advanced.speechPrep.attemptTimeoutMs",
            "must not exceed totalTimeoutMs",
        );
    }
    let max_input_length = resolve_max_chars(
        advanced.max_input_chars,
        DEFAULT_PREP_MAX_INPUT_CHARS,
        "$.advanced.speechPrep.maxInputChars",
    )?;
    let mut max_length = resolve_max_chars(
        advanced.max_output_chars,
        DEFAULT_PREP_MAX_OUTPUT_CHARS,
        "$.advanced.speechPrep.maxOutputChars",
    )?;
    if max_length > max_input_length {
        return config_error(
            "$.advanced.speechPrep.maxOutputChars",
            "must not exceed maxInputChars",
        );
    }
    let shorten_floor = DEFAULT_SHORTEN_MIN_OUTPUT_CHARS.min(max_length);
    let default_threshold = if mode == SpeechPrepMode::Shorten {
        shorten_floor
    } else {
        DEFAULT_PREP_THRESHOLD_CHARS
    };
    let mut threshold = advanced.threshold_chars.unwrap_or(default_threshold);
    if threshold > max_input_length {
        return config_error(
            "$.advanced.speechPrep.thresholdChars",
            "must not exceed maxInputChars",
        );
    }
    if mode == SpeechPrepMode::Shorten {
        threshold = threshold.max(shorten_floor);
        max_length = max_length.max(shorten_floor);
    }
    let strategies = advanced
        .strategies
        .map(resolve_strategies)
        .unwrap_or_default();
    let tag_palette = advanced.tag_palette.clone().unwrap_or_else(|| {
        DEFAULT_TAG_PALETTE
            .iter()
            .map(|tag| (*tag).to_string())
            .collect()
    });
    if tag_palette.is_empty() || tag_palette.iter().any(|tag| tag.trim().is_empty()) {
        return config_error(
            "$.advanced.speechPrep.tagPalette",
            "must contain at least one nonempty tag",
        );
    }
    let reasoning_effort = advanced.reasoning_effort.as_deref().map(str::trim);
    if reasoning_effort == Some("") {
        return config_error("$.advanced.speechPrep.reasoningEffort", "must not be empty");
    }
    let reasoning_effort = reasoning_effort
        .filter(|effort| !effort.eq_ignore_ascii_case("none"))
        .map(str::to_string);
    if advanced.enabled == Some(false) {
        return Ok(None);
    }
    let base_url = match provider {
        SpeechPrepProviderKind::Google => resolve_url(
            advanced
                .base_url
                .as_deref()
                .or_else(|| google.map(|provider| provider.base_url.as_str())),
            DEFAULT_GOOGLE_BASE_URL,
            "$.advanced.speechPrep.baseUrl",
        )?,
        SpeechPrepProviderKind::Codex => resolve_url(
            advanced.base_url.as_deref(),
            DEFAULT_CODEX_BASE_URL,
            "$.advanced.speechPrep.baseUrl",
        )?
        .trim_end_matches('/')
        .to_string(),
    };
    let api_key = match provider {
        SpeechPrepProviderKind::Google => Some(
            google
                .ok_or_else(|| {
                    SpeechError::Config(
                        "invalid value at $.advanced.speechPrep.provider: Google is not configured"
                            .into(),
                    )
                })?
                .api_key
                .clone(),
        ),
        SpeechPrepProviderKind::Codex => None,
    };
    let auth_file = (provider == SpeechPrepProviderKind::Codex).then(|| {
        advanced
            .auth_file
            .clone()
            .unwrap_or_else(default_codex_auth_file)
    });
    Ok(Some(SpeechPrepConfig {
        provider,
        mode,
        api_key,
        base_url,
        model: models[0].clone(),
        fallback_models: models[1..].to_vec(),
        auth_file,
        reasoning_effort,
        strategies,
        tag_palette,
        cap_performance_tags: advanced.cap_performance_tags.unwrap_or(false),
        threshold,
        max_input_length,
        max_length,
        attempt_timeout,
        timeout: total_timeout,
    }))
}

fn google_overrides_present(config: &super::serde::AdvancedGoogleProviderConfig) -> bool {
    config.base_url.is_some()
        || config.api_key_env.is_some()
        || config.timeout_ms.is_some()
        || config.max_input_chars.is_some()
        || config.inline_audio_tags.is_some()
}

fn elevenlabs_overrides_present(config: &super::serde::AdvancedElevenLabsProviderConfig) -> bool {
    config.base_url.is_some()
        || config.api_key_env.is_some()
        || config.timeout_ms.is_some()
        || config.max_input_chars.is_some()
        || config.inline_audio_tags.is_some()
        || config.output_format.is_some()
        || config.language_code.is_some()
}

fn resolve_strategies(input: SpeechPrepStrategiesInput) -> SpeechPrepStrategies {
    SpeechPrepStrategies {
        google: resolve_strategy(input.google),
        elevenlabs: resolve_strategy(input.elevenlabs),
        default: resolve_strategy(input.default),
    }
}

fn resolve_strategy(input: SpeechPrepStrategyInput) -> SpeechPrepStrategy {
    match input {
        SpeechPrepStrategyInput::InlineTags => SpeechPrepStrategy::InlineTags,
        SpeechPrepStrategyInput::StyleInstruction => SpeechPrepStrategy::StyleInstruction,
        SpeechPrepStrategyInput::Off => SpeechPrepStrategy::Off,
    }
}

fn validate_models(path: &str, models: Vec<String>) -> Result<Vec<String>, SpeechError> {
    if models.is_empty() {
        return config_error(path, "at least one model is required");
    }
    let models = models
        .into_iter()
        .map(|model| model.trim().to_string())
        .collect::<Vec<_>>();
    if models.iter().any(String::is_empty) {
        return config_error(path, "models must be nonempty strings");
    }
    let unique = models.iter().collect::<HashSet<_>>();
    if unique.len() != models.len() {
        return config_error(path, "models must be unique");
    }
    Ok(models)
}

fn validate_env_name(value: Option<&str>, path: &str) -> Result<(), SpeechError> {
    let Some(value) = value else {
        return Ok(());
    };
    let mut chars = value.chars();
    let valid_first = chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic());
    if !valid_first || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        return config_error(path, "expected a valid environment variable name");
    }
    Ok(())
}

fn resolve_url(value: Option<&str>, default: &str, path: &str) -> Result<String, SpeechError> {
    let value = value.unwrap_or(default).trim();
    let parsed = reqwest::Url::parse(value).map_err(|_| {
        SpeechError::Config(format!(
            "invalid value at {path}: expected an absolute HTTP or HTTPS URL"
        ))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return config_error(path, "expected an absolute HTTP or HTTPS URL");
    }
    Ok(value.trim_end_matches('/').to_string())
}

fn resolve_timeout(value: Option<u64>, default: u64, path: &str) -> Result<Duration, SpeechError> {
    let value = value.unwrap_or(default);
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&value) {
        return config_error(
            path,
            format!("expected {MIN_TIMEOUT_MS}..={MAX_TIMEOUT_MS}"),
        );
    }
    Ok(Duration::from_millis(value))
}

fn resolve_max_chars(
    value: Option<usize>,
    default: usize,
    path: &str,
) -> Result<usize, SpeechError> {
    let value = value.unwrap_or(default);
    if value < 80 {
        return config_error(path, "expected at least 80 characters");
    }
    Ok(value)
}

fn validate_voice_settings(
    settings: &ElevenLabsVoiceSettingsInput,
    path: &str,
) -> Result<(), SpeechError> {
    for (name, value) in [
        ("stability", settings.stability),
        ("similarityBoost", settings.similarity_boost),
        ("style", settings.style),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return config_error(
                format!("{path}.{name}"),
                "expected a finite value from 0 through 1",
            );
        }
    }
    if !settings.speed.is_finite() || !(0.7..=1.2).contains(&settings.speed) {
        return config_error(
            format!("{path}.speed"),
            "expected a finite value from 0.7 through 1.2",
        );
    }
    Ok(())
}

fn validate_nonempty(value: &str, path: &str) -> Result<(), SpeechError> {
    if value.trim().is_empty() {
        config_error(path, "must not be empty")
    } else {
        Ok(())
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn default_codex_auth_file() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
        .join("auth.json")
}

fn elevenlabs_v3(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model == "eleven_v3" || model.starts_with("eleven_v3_")
}

fn config_error<T>(
    path: impl AsRef<str>,
    message: impl std::fmt::Display,
) -> Result<T, SpeechError> {
    Err(SpeechError::Config(format!(
        "invalid value at {}: {message}",
        path.as_ref()
    )))
}
