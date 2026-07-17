use std::collections::HashMap;

use codex_voice_core::SpeechError;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::models::ResolvedTtsConfig;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoiceConfigFile {
    pub version: u8,
    pub default_voice: String,
    pub providers: ProviderConfigs,
    pub voices: HashMap<String, VoiceConfig>,
    #[serde(default)]
    pub advanced: AdvancedConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfigs {
    #[serde(default)]
    pub google: Option<GoogleProviderConfig>,
    #[serde(default)]
    pub elevenlabs: Option<ElevenLabsProviderConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoogleProviderConfig {
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElevenLabsProviderConfig {
    pub models: Vec<String>,
    #[serde(default = "default_text_normalization")]
    pub text_normalization: String,
    #[serde(default = "default_stream_gain")]
    pub stream_gain: f64,
}

fn default_text_normalization() -> String {
    "auto".to_string()
}

fn default_stream_gain() -> f64 {
    2.0
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoiceConfig {
    pub label: String,
    pub description: String,
    #[serde(default)]
    pub prompt: VoicePrompt,
    pub backends: Vec<VoiceBackend>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoicePrompt {
    #[serde(default)]
    pub scene: Option<String>,
    #[serde(default)]
    pub sample_context: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub pace: Option<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase", deny_unknown_fields)]
pub enum VoiceBackend {
    #[serde(rename_all = "camelCase")]
    Google { voice: String },
    #[serde(rename_all = "camelCase")]
    Elevenlabs {
        voice_id: String,
        #[serde(default)]
        settings: ElevenLabsVoiceSettingsInput,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElevenLabsVoiceSettingsInput {
    #[serde(default = "default_stability")]
    pub stability: f64,
    #[serde(default = "default_similarity_boost")]
    pub similarity_boost: f64,
    #[serde(default)]
    pub style: f64,
    #[serde(default = "default_true")]
    pub speaker_boost: bool,
    #[serde(default = "default_speed")]
    pub speed: f64,
}

impl Default for ElevenLabsVoiceSettingsInput {
    fn default() -> Self {
        Self {
            stability: default_stability(),
            similarity_boost: default_similarity_boost(),
            style: 0.0,
            speaker_boost: true,
            speed: default_speed(),
        }
    }
}

fn default_stability() -> f64 {
    0.5
}

fn default_similarity_boost() -> f64 {
    0.75
}

fn default_true() -> bool {
    true
}

fn default_speed() -> f64 {
    1.0
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedConfig {
    #[serde(default)]
    pub providers: AdvancedProviderConfigs,
    #[serde(default, rename = "speechPrep")]
    pub speech_prep: AdvancedSpeechPrepConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedProviderConfigs {
    #[serde(default)]
    pub google: AdvancedGoogleProviderConfig,
    #[serde(default)]
    pub elevenlabs: AdvancedElevenLabsProviderConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedGoogleProviderConfig {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_input_chars: Option<usize>,
    #[serde(default)]
    pub inline_audio_tags: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedElevenLabsProviderConfig {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_input_chars: Option<usize>,
    #[serde(default)]
    pub inline_audio_tags: Option<bool>,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default)]
    pub language_code: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedSpeechPrepConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub provider: Option<SpeechPrepProviderInput>,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Option<SpeechPrepModeInput>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub auth_file: Option<std::path::PathBuf>,
    #[serde(default)]
    pub threshold_chars: Option<usize>,
    #[serde(default)]
    pub max_input_chars: Option<usize>,
    #[serde(default)]
    pub max_output_chars: Option<usize>,
    #[serde(default)]
    pub attempt_timeout_ms: Option<u64>,
    #[serde(default)]
    pub total_timeout_ms: Option<u64>,
    #[serde(default)]
    pub strategies: Option<SpeechPrepStrategiesInput>,
    #[serde(default)]
    pub tag_palette: Option<Vec<String>>,
    #[serde(default)]
    pub cap_performance_tags: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeechPrepProviderInput {
    Google,
    Codex,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum SpeechPrepModeInput {
    #[serde(rename = "shorten")]
    Shorten,
    #[serde(rename = "performance-tags")]
    PerformanceTags,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum SpeechPrepStrategyInput {
    #[serde(rename = "inline-tags")]
    InlineTags,
    #[serde(rename = "style-instruction")]
    StyleInstruction,
    #[serde(rename = "off")]
    Off,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechPrepStrategiesInput {
    pub google: SpeechPrepStrategyInput,
    pub elevenlabs: SpeechPrepStrategyInput,
    pub default: SpeechPrepStrategyInput,
}

impl VoiceConfigFile {
    pub fn parse(raw: &str) -> Result<Self, SpeechError> {
        let value: Value = serde_json::from_str(raw).map_err(|error| {
            SpeechError::Config(format!("failed to parse config JSON: {error}"))
        })?;
        validate_shape(&value)?;
        serde_json::from_value(value)
            .map_err(|error| SpeechError::Config(format!("failed to decode config at $: {error}")))
    }

    pub fn resolve(self) -> Result<ResolvedTtsConfig, SpeechError> {
        super::provider::resolve_file(self)
    }
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, SpeechError> {
    value
        .as_object()
        .ok_or_else(|| SpeechError::Config(format!("invalid value at {path}: expected object")))
}

fn array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>, SpeechError> {
    value
        .as_array()
        .ok_or_else(|| SpeechError::Config(format!("invalid value at {path}: expected array")))
}

fn validate_keys(
    map: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> Result<(), SpeechError> {
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(SpeechError::Config(format!(
            "unknown field at {path}.{key}"
        )));
    }
    Ok(())
}

fn validate_string(value: &Value, path: &str) -> Result<(), SpeechError> {
    if value.is_string() {
        Ok(())
    } else {
        Err(SpeechError::Config(format!(
            "invalid value at {path}: expected string"
        )))
    }
}

fn validate_bool(value: &Value, path: &str) -> Result<(), SpeechError> {
    if value.is_boolean() {
        Ok(())
    } else {
        Err(SpeechError::Config(format!(
            "invalid value at {path}: expected boolean"
        )))
    }
}

fn validate_number(value: &Value, path: &str) -> Result<(), SpeechError> {
    if value.is_number() {
        Ok(())
    } else {
        Err(SpeechError::Config(format!(
            "invalid value at {path}: expected number"
        )))
    }
}

fn validate_integer(value: &Value, path: &str) -> Result<(), SpeechError> {
    if value.as_u64().is_some() {
        Ok(())
    } else {
        Err(SpeechError::Config(format!(
            "invalid value at {path}: expected nonnegative integer"
        )))
    }
}

fn validate_optional(
    map: &Map<String, Value>,
    key: &str,
    path: &str,
    validate: fn(&Value, &str) -> Result<(), SpeechError>,
) -> Result<(), SpeechError> {
    if let Some(value) = map.get(key) {
        validate(value, &format!("{path}.{key}"))?;
    }
    Ok(())
}

fn required<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a Value, SpeechError> {
    map.get(key)
        .ok_or_else(|| SpeechError::Config(format!("missing field at {path}.{key}")))
}

fn validate_one_of(value: &Value, path: &str, allowed: &[&str]) -> Result<(), SpeechError> {
    validate_string(value, path)?;
    let value = value.as_str().expect("validated as string");
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(SpeechError::Config(format!(
            "invalid value at {path}: expected one of {}",
            allowed.join(", ")
        )))
    }
}

fn validate_string_array(value: &Value, path: &str) -> Result<(), SpeechError> {
    for (index, value) in array(value, path)?.iter().enumerate() {
        validate_string(value, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn validate_shape(value: &Value) -> Result<(), SpeechError> {
    let root = object(value, "$")?;
    validate_keys(
        root,
        &["version", "defaultVoice", "providers", "voices", "advanced"],
        "$",
    )?;
    let version = required(root, "version", "$")?;
    validate_integer(version, "$.version")?;
    if version.as_u64() != Some(1) {
        return Err(SpeechError::Config(
            "invalid value at $.version: expected 1".into(),
        ));
    }
    validate_string(required(root, "defaultVoice", "$")?, "$.defaultVoice")?;

    let providers = root
        .get("providers")
        .ok_or_else(|| SpeechError::Config("missing field at $.providers".into()))?;
    let providers = object(providers, "$.providers")?;
    validate_keys(providers, &["google", "elevenlabs"], "$.providers")?;
    for (name, value) in providers {
        let path = format!("$.providers.{name}");
        let provider = object(value, &path)?;
        match name.as_str() {
            "google" => validate_keys(provider, &["models"], &path)?,
            "elevenlabs" => validate_keys(
                provider,
                &["models", "textNormalization", "streamGain"],
                &path,
            )?,
            _ => unreachable!(),
        }
        validate_string_array(
            required(provider, "models", &path)?,
            &format!("{path}.models"),
        )?;
        validate_optional(provider, "textNormalization", &path, validate_string)?;
        validate_optional(provider, "streamGain", &path, validate_number)?;
    }

    let voices = root
        .get("voices")
        .ok_or_else(|| SpeechError::Config("missing field at $.voices".into()))?;
    for (name, value) in object(voices, "$.voices")? {
        let path = format!("$.voices.{name}");
        let voice = object(value, &path)?;
        validate_keys(
            voice,
            &["label", "description", "prompt", "backends"],
            &path,
        )?;
        validate_string(required(voice, "label", &path)?, &format!("{path}.label"))?;
        validate_string(
            required(voice, "description", &path)?,
            &format!("{path}.description"),
        )?;
        if let Some(prompt) = voice.get("prompt") {
            let prompt_path = format!("{path}.prompt");
            let prompt = object(prompt, &prompt_path)?;
            validate_keys(
                prompt,
                &["scene", "sampleContext", "style", "pace", "constraints"],
                &prompt_path,
            )?;
            for key in ["scene", "sampleContext", "style", "pace"] {
                validate_optional(prompt, key, &prompt_path, validate_string)?;
            }
            if let Some(constraints) = prompt.get("constraints") {
                validate_string_array(constraints, &format!("{prompt_path}.constraints"))?;
            }
        }
        {
            let backends = required(voice, "backends", &path)?;
            for (index, backend) in array(backends, &format!("{path}.backends"))?
                .iter()
                .enumerate()
            {
                let backend_path = format!("{path}.backends[{index}]");
                let backend = object(backend, &backend_path)?;
                let provider =
                    backend
                        .get("provider")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            SpeechError::Config(format!(
                                "invalid value at {backend_path}.provider: expected string"
                            ))
                        })?;
                match provider {
                    "google" => {
                        validate_keys(backend, &["provider", "voice"], &backend_path)?;
                        validate_string(
                            required(backend, "voice", &backend_path)?,
                            &format!("{backend_path}.voice"),
                        )?;
                    }
                    "elevenlabs" => {
                        validate_keys(
                            backend,
                            &["provider", "voiceId", "settings"],
                            &backend_path,
                        )?;
                        validate_string(
                            required(backend, "voiceId", &backend_path)?,
                            &format!("{backend_path}.voiceId"),
                        )?;
                        if let Some(settings) = backend.get("settings") {
                            let settings_path = format!("{backend_path}.settings");
                            let settings = object(settings, &settings_path)?;
                            validate_keys(
                                settings,
                                &[
                                    "stability",
                                    "similarityBoost",
                                    "style",
                                    "speakerBoost",
                                    "speed",
                                ],
                                &settings_path,
                            )?;
                            for key in ["stability", "similarityBoost", "style", "speed"] {
                                validate_optional(settings, key, &settings_path, validate_number)?;
                            }
                            validate_optional(
                                settings,
                                "speakerBoost",
                                &settings_path,
                                validate_bool,
                            )?;
                        }
                    }
                    _ => {
                        return Err(SpeechError::Config(format!(
                        "invalid value at {backend_path}.provider: expected google or elevenlabs"
                    )))
                    }
                }
            }
        }
    }

    if let Some(advanced) = root.get("advanced") {
        let advanced = object(advanced, "$.advanced")?;
        validate_keys(advanced, &["providers", "speechPrep"], "$.advanced")?;
        if let Some(providers) = advanced.get("providers") {
            let providers = object(providers, "$.advanced.providers")?;
            validate_keys(providers, &["google", "elevenlabs"], "$.advanced.providers")?;
            for (name, value) in providers {
                let path = format!("$.advanced.providers.{name}");
                let provider = object(value, &path)?;
                let mut keys = vec![
                    "baseUrl",
                    "apiKeyEnv",
                    "timeoutMs",
                    "maxInputChars",
                    "inlineAudioTags",
                ];
                if name == "elevenlabs" {
                    keys.extend(["outputFormat", "languageCode"]);
                }
                validate_keys(provider, &keys, &path)?;
                for key in ["baseUrl", "apiKeyEnv", "outputFormat", "languageCode"] {
                    validate_optional(provider, key, &path, validate_string)?;
                }
                for key in ["timeoutMs", "maxInputChars"] {
                    validate_optional(provider, key, &path, validate_integer)?;
                }
                validate_optional(provider, "inlineAudioTags", &path, validate_bool)?;
            }
        }
        if let Some(prep) = advanced.get("speechPrep") {
            let prep = object(prep, "$.advanced.speechPrep")?;
            validate_keys(
                prep,
                &[
                    "enabled",
                    "provider",
                    "models",
                    "mode",
                    "reasoningEffort",
                    "baseUrl",
                    "authFile",
                    "thresholdChars",
                    "maxInputChars",
                    "maxOutputChars",
                    "attemptTimeoutMs",
                    "totalTimeoutMs",
                    "strategies",
                    "tagPalette",
                    "capPerformanceTags",
                ],
                "$.advanced.speechPrep",
            )?;
            if let Some(models) = prep.get("models") {
                validate_string_array(models, "$.advanced.speechPrep.models")?;
            }
            if let Some(tags) = prep.get("tagPalette") {
                validate_string_array(tags, "$.advanced.speechPrep.tagPalette")?;
            }
            if let Some(strategies) = prep.get("strategies") {
                let strategies = object(strategies, "$.advanced.speechPrep.strategies")?;
                validate_keys(
                    strategies,
                    &["google", "elevenlabs", "default"],
                    "$.advanced.speechPrep.strategies",
                )?;
                for key in ["google", "elevenlabs", "default"] {
                    validate_one_of(
                        required(strategies, key, "$.advanced.speechPrep.strategies")?,
                        &format!("$.advanced.speechPrep.strategies.{key}"),
                        &["inline-tags", "style-instruction", "off"],
                    )?;
                }
            }
            if let Some(provider) = prep.get("provider") {
                validate_one_of(
                    provider,
                    "$.advanced.speechPrep.provider",
                    &["google", "codex"],
                )?;
            }
            if let Some(mode) = prep.get("mode") {
                validate_one_of(
                    mode,
                    "$.advanced.speechPrep.mode",
                    &["shorten", "performance-tags"],
                )?;
            }
            for key in ["reasoningEffort", "baseUrl", "authFile"] {
                validate_optional(prep, key, "$.advanced.speechPrep", validate_string)?;
            }
            for key in [
                "thresholdChars",
                "maxInputChars",
                "maxOutputChars",
                "attemptTimeoutMs",
                "totalTimeoutMs",
            ] {
                validate_optional(prep, key, "$.advanced.speechPrep", validate_integer)?;
            }
            validate_optional(prep, "enabled", "$.advanced.speechPrep", validate_bool)?;
            validate_optional(
                prep,
                "capPerformanceTags",
                "$.advanced.speechPrep",
                validate_bool,
            )?;
        }
    }
    Ok(())
}
