use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use codex_voice_core::SpeechError;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Google,
    ElevenLabs,
}

impl ProviderKind {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "google" => Some(Self::Google),
            "elevenlabs" => Some(Self::ElevenLabs),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTtsConfig {
    pub default_provider: ProviderKind,
    pub default_persona: Option<String>,
    pub max_text_length: usize,
    pub timeout: Duration,
    pub google: Option<GoogleRuntimeConfig>,
    pub elevenlabs: Option<ElevenLabsRuntimeConfig>,
    pub personas: HashMap<String, ResolvedPersona>,
}

#[derive(Debug, Clone)]
pub struct GoogleRuntimeConfig {
    pub api_key: String,
    pub base_url: String,
    pub voice: String,
    pub model: String,
    pub fallback_models: Vec<String>,
    pub max_text_length: usize,
    pub timeout: Duration,
    pub scene: Option<String>,
    pub sample_context: Option<String>,
    pub style: Option<String>,
    pub pace: Option<String>,
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ElevenLabsRuntimeConfig {
    pub api_key: String,
    pub base_url: String,
    pub model_id: String,
    pub apply_text_normalization: String,
    pub output_format: String,
    pub language_code: String,
    pub max_text_length: usize,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ResolvedPersona {
    pub label: String,
    pub description: String,
    pub provider: ProviderKind,
    pub fallback_policy: String,
    pub prompt_profile: Option<String>,
    pub prompt_scene: Option<String>,
    pub prompt_sample_context: Option<String>,
    pub prompt_style: Option<String>,
    pub prompt_accent: Option<String>,
    pub prompt_pacing: Option<String>,
    pub prompt_constraints: Vec<String>,
    pub google: Option<GooglePersonaConfig>,
    pub elevenlabs: Option<ElevenLabsPersonaConfig>,
}

#[derive(Debug, Clone)]
pub struct GooglePersonaConfig {
    pub voice_name: String,
    pub prompt_template: String,
    pub persona_prompt: String,
}

#[derive(Debug, Clone)]
pub struct ElevenLabsPersonaConfig {
    pub voice_id: String,
    pub voice_settings: ElevenLabsVoiceSettings,
}

#[derive(Debug, Clone)]
pub struct ElevenLabsVoiceSettings {
    pub stability: f64,
    pub similarity_boost: f64,
    pub style: f64,
    pub use_speaker_boost: bool,
    pub speed: f64,
}

#[derive(Debug, Clone)]
pub struct ReadAloudConfigLoader {
    pub path: PathBuf,
}

impl ReadAloudConfigLoader {
    pub fn default_path() -> Result<PathBuf, SpeechError> {
        let home = dirs::home_dir()
            .ok_or_else(|| SpeechError::Config("could not resolve home directory".into()))?;
        Ok(home.join(".codex").join("read-aloud-defaults.json"))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<ResolvedTtsConfig, SpeechError> {
        let raw = std::fs::read_to_string(&self.path).map_err(|e| {
            SpeechError::Config(format!("failed to read config at {:?}: {}", self.path, e))
        })?;
        let file: ReadAloudDefaultsFile = serde_json::from_str(&raw)
            .map_err(|e| SpeechError::Config(format!("failed to parse config: {}", e)))?;
        file.resolve()
    }
}

// ---------------------------------------------------------------------------
// Serde structs (mirrors read-aloud-defaults.json shape loosely)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ReadAloudDefaultsFile {
    messages: MessagesConfig,
    #[serde(default)]
    models: Option<ModelDefaults>,
}

#[derive(Debug, Deserialize)]
struct MessagesConfig {
    #[serde(default)]
    tts: Option<TtsDefaultsConfig>,
}

#[derive(Debug, Deserialize)]
struct TtsDefaultsConfig {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    persona: Option<String>,
    #[serde(rename = "maxTextLength", default)]
    max_text_length: Option<usize>,
    #[serde(rename = "timeoutMs", default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    providers: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    personas: Option<HashMap<String, PersonaConfig>>,
}

#[derive(Debug, Deserialize)]
struct ModelDefaults {
    #[serde(default)]
    providers: Option<HashMap<String, ProviderModelConfig>>,
}

#[derive(Debug, Deserialize)]
struct ProviderModelConfig {
    #[serde(rename = "baseUrl", default)]
    base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PersonaConfig {
    label: String,
    description: String,
    provider: String,
    #[serde(rename = "fallbackPolicy", default)]
    fallback_policy: String,
    #[serde(default)]
    prompt: Option<PersonaPrompt>,
    #[serde(default)]
    providers: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct PersonaPrompt {
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    scene: Option<String>,
    #[serde(rename = "sampleContext", default)]
    sample_context: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    accent: Option<String>,
    #[serde(default)]
    pacing: Option<String>,
    #[serde(default)]
    constraints: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Resolution logic
// ---------------------------------------------------------------------------

impl ReadAloudDefaultsFile {
    fn resolve(self) -> Result<ResolvedTtsConfig, SpeechError> {
        let tts = self
            .messages
            .tts
            .ok_or_else(|| SpeechError::Config("missing messages.tts block".into()))?;

        let default_provider = tts
            .provider
            .as_deref()
            .and_then(ProviderKind::from_name)
            .ok_or_else(|| SpeechError::Config("missing or invalid default provider".into()))?;

        let max_text_length = tts.max_text_length.unwrap_or(1000);
        let timeout = Duration::from_millis(tts.timeout_ms.unwrap_or(120_000));

        let providers = tts.providers.unwrap_or_default();
        let models = self.models.and_then(|m| m.providers).unwrap_or_default();

        let google = resolve_google_config(&providers, &models, max_text_length, timeout)?;
        let elevenlabs = resolve_elevenlabs_config(&providers, &models, max_text_length, timeout)?;

        let personas = tts
            .personas
            .unwrap_or_default()
            .into_iter()
            .map(|(name, cfg)| {
                let resolved = resolve_persona(&name, cfg, &providers)?;
                Ok((name, resolved))
            })
            .collect::<Result<HashMap<_, _>, SpeechError>>()?;

        validate_default_path(
            default_provider,
            tts.persona.as_deref(),
            &google,
            &elevenlabs,
            &personas,
        )?;

        Ok(ResolvedTtsConfig {
            default_provider,
            default_persona: tts.persona,
            max_text_length,
            timeout,
            google,
            elevenlabs,
            personas,
        })
    }
}

fn validate_default_path(
    default_provider: ProviderKind,
    default_persona: Option<&str>,
    google: &Option<GoogleRuntimeConfig>,
    elevenlabs: &Option<ElevenLabsRuntimeConfig>,
    personas: &HashMap<String, ResolvedPersona>,
) -> Result<(), SpeechError> {
    let Some(persona_name) = default_persona else {
        return validate_provider(default_provider, google, elevenlabs).map_err(|message| {
            SpeechError::Config(format!("default provider is not usable: {message}"))
        });
    };

    let persona = personas.get(persona_name).ok_or_else(|| {
        SpeechError::Config(format!("default persona {persona_name:?} is not defined"))
    })?;

    if persona_provider_usable(persona, persona.provider, google, elevenlabs) {
        return Ok(());
    }

    if persona.fallback_policy == "preserve-persona" {
        let fallback = match persona.provider {
            ProviderKind::Google => ProviderKind::ElevenLabs,
            ProviderKind::ElevenLabs => ProviderKind::Google,
        };
        if persona_provider_usable(persona, fallback, google, elevenlabs) {
            return Ok(());
        }
    }

    Err(SpeechError::Config(format!(
        "default persona {persona_name:?} has no usable configured TTS provider"
    )))
}

fn validate_provider(
    provider: ProviderKind,
    google: &Option<GoogleRuntimeConfig>,
    elevenlabs: &Option<ElevenLabsRuntimeConfig>,
) -> Result<(), &'static str> {
    match provider {
        ProviderKind::Google if google.is_some() => Ok(()),
        ProviderKind::ElevenLabs if elevenlabs.is_some() => Ok(()),
        ProviderKind::Google => Err("Google is selected but not configured"),
        ProviderKind::ElevenLabs => Err("ElevenLabs is selected but not configured"),
    }
}

fn persona_provider_usable(
    persona: &ResolvedPersona,
    provider: ProviderKind,
    google: &Option<GoogleRuntimeConfig>,
    elevenlabs: &Option<ElevenLabsRuntimeConfig>,
) -> bool {
    match provider {
        ProviderKind::Google => google.is_some(),
        ProviderKind::ElevenLabs => {
            elevenlabs.is_some()
                && persona
                    .elevenlabs
                    .as_ref()
                    .is_some_and(|cfg| !cfg.voice_id.trim().is_empty())
        }
    }
}

fn resolve_google_config(
    providers: &HashMap<String, serde_json::Value>,
    models: &HashMap<String, ProviderModelConfig>,
    max_text_length: usize,
    timeout: Duration,
) -> Result<Option<GoogleRuntimeConfig>, SpeechError> {
    let Some(val) = providers.get("google") else {
        return Ok(None);
    };

    let api_key =
        crate::secret::resolve_secret(val.get("apiKey"), "GEMINI_API_KEY", "GOOGLE_API_KEY")?;

    let base_url = val
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .or_else(|| models.get("google").and_then(|m| m.base_url.as_deref()))
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta")
        .to_string();

    let voice = val
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("Sulafat")
        .to_string();
    let model = val
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini-2.5-flash-preview-tts")
        .to_string();

    let fallback_models: Vec<String> = val
        .get("fallbackModels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let scene = val.get("scene").and_then(|v| v.as_str()).map(String::from);
    let sample_context = val
        .get("sampleContext")
        .and_then(|v| v.as_str())
        .map(String::from);
    let style = val.get("style").and_then(|v| v.as_str()).map(String::from);
    let pace = val.get("pace").and_then(|v| v.as_str()).map(String::from);

    let constraints: Vec<String> = val
        .get("constraints")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(Some(GoogleRuntimeConfig {
        api_key,
        base_url,
        voice,
        model,
        fallback_models,
        max_text_length,
        timeout,
        scene,
        sample_context,
        style,
        pace,
        constraints,
    }))
}

fn resolve_elevenlabs_config(
    providers: &HashMap<String, serde_json::Value>,
    models: &HashMap<String, ProviderModelConfig>,
    max_text_length: usize,
    timeout: Duration,
) -> Result<Option<ElevenLabsRuntimeConfig>, SpeechError> {
    let Some(val) = providers.get("elevenlabs") else {
        return Ok(None);
    };

    let api_key = crate::secret::resolve_secret(
        val.get("apiKey"),
        "ELEVENLABS_API_KEY",
        "ELEVENLABS_API_KEY",
    )?;

    let base_url = val
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .or_else(|| models.get("elevenlabs").and_then(|m| m.base_url.as_deref()))
        .unwrap_or("https://api.elevenlabs.io")
        .to_string();

    let model_id = val
        .get("modelId")
        .and_then(|v| v.as_str())
        .unwrap_or("eleven_multilingual_v2")
        .to_string();
    let apply_text_normalization = val
        .get("applyTextNormalization")
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_string();
    let output_format = val
        .get("outputFormat")
        .and_then(|v| v.as_str())
        .unwrap_or("mp3_44100_128")
        .to_string();
    let language_code = val
        .get("languageCode")
        .and_then(|v| v.as_str())
        .unwrap_or("en")
        .to_string();

    Ok(Some(ElevenLabsRuntimeConfig {
        api_key,
        base_url,
        model_id,
        apply_text_normalization,
        output_format,
        language_code,
        max_text_length,
        timeout,
    }))
}

fn resolve_persona(
    _name: &str,
    cfg: PersonaConfig,
    _providers: &HashMap<String, serde_json::Value>,
) -> Result<ResolvedPersona, SpeechError> {
    let provider = ProviderKind::from_name(&cfg.provider).ok_or_else(|| {
        SpeechError::Config(format!("invalid persona provider: {}", cfg.provider))
    })?;

    let prompt = cfg.prompt;

    let google = cfg
        .providers
        .as_ref()
        .and_then(|m| m.get("google"))
        .map(|raw| GooglePersonaConfig {
            voice_name: raw
                .get("voiceName")
                .and_then(|v| v.as_str())
                .unwrap_or("Sulafat")
                .to_string(),
            prompt_template: raw
                .get("promptTemplate")
                .and_then(|v| v.as_str())
                .unwrap_or("audio-profile-v1")
                .to_string(),
            persona_prompt: raw
                .get("personaPrompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });

    let elevenlabs = cfg
        .providers
        .as_ref()
        .and_then(|m| m.get("elevenlabs"))
        .map(|raw| ElevenLabsPersonaConfig {
            voice_id: raw
                .get("voiceId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            voice_settings: ElevenLabsVoiceSettings {
                stability: raw
                    .get("voiceSettings")
                    .and_then(|v| v.get("stability"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5),
                similarity_boost: raw
                    .get("voiceSettings")
                    .and_then(|v| v.get("similarityBoost"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.75),
                style: raw
                    .get("voiceSettings")
                    .and_then(|v| v.get("style"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                use_speaker_boost: raw
                    .get("voiceSettings")
                    .and_then(|v| v.get("useSpeakerBoost"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
                speed: raw
                    .get("voiceSettings")
                    .and_then(|v| v.get("speed"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0),
            },
        });

    Ok(ResolvedPersona {
        label: cfg.label,
        description: cfg.description,
        provider,
        fallback_policy: cfg.fallback_policy,
        prompt_profile: prompt.as_ref().and_then(|p| p.profile.clone()),
        prompt_scene: prompt.as_ref().and_then(|p| p.scene.clone()),
        prompt_sample_context: prompt.as_ref().and_then(|p| p.sample_context.clone()),
        prompt_style: prompt.as_ref().and_then(|p| p.style.clone()),
        prompt_accent: prompt.as_ref().and_then(|p| p.accent.clone()),
        prompt_pacing: prompt.as_ref().and_then(|p| p.pacing.clone()),
        prompt_constraints: prompt
            .as_ref()
            .and_then(|p| p.constraints.clone())
            .unwrap_or_default(),
        google,
        elevenlabs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn minimal_config() -> &'static str {
        r#"
        {
            "messages": {
                "tts": {
                    "provider": "google",
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_MINIMAL" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#
    }

    #[test]
    fn parse_minimal_google_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");
        std::fs::write(&path, minimal_config()).unwrap();

        std::env::set_var("TEST_GOOGLE_KEY_MINIMAL", "test-google-key-value");
        let loader = ReadAloudConfigLoader::new(path);
        let resolved = loader.load().unwrap();

        assert_eq!(resolved.default_provider, ProviderKind::Google);
        assert_eq!(resolved.default_persona.as_deref(), None);
        assert_eq!(resolved.max_text_length, 1000);
        assert!(resolved.google.is_some());
        assert!(resolved.elevenlabs.is_none());

        let google = resolved.google.unwrap();
        assert_eq!(google.api_key, "test-google-key-value");
        assert_eq!(google.voice, "Sulafat");
        assert_eq!(google.model, "gemini-2.5-flash-preview-tts");
    }

    #[test]
    fn resolve_voice_sky_to_persona() {
        // Persona resolution is tested by ensuring the 'sky' persona exists after loading
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");

        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "elevenlabs",
                    "persona": "sky",
                    "providers": {
                        "elevenlabs": {
                            "apiKey": { "source": "env", "id": "TEST_ELEVEN_KEY" },
                            "baseUrl": "https://api.elevenlabs.io",
                            "modelId": "eleven_v3"
                        },
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    },
                    "personas": {
                        "sky": {
                            "label": "Sky",
                            "description": "Warm and playful",
                            "provider": "elevenlabs",
                            "fallbackPolicy": "preserve-persona",
                            "providers": {
                                "elevenlabs": {
                                    "voiceId": "2tM0Teq5Piex0mNtlZnm",
                                    "voiceSettings": { "stability": 0.5, "similarityBoost": 0.75, "style": 0.3, "useSpeakerBoost": true, "speed": 1.2 }
                                },
                                "google": {
                                    "voiceName": "Sulafat",
                                    "promptTemplate": "audio-profile-v1",
                                    "personaPrompt": "Build Sky's spoken delivery..."
                                }
                            }
                        }
                    }
                }
            }
        }
        "#;
        std::fs::write(&path, config).unwrap();
        std::env::set_var("TEST_ELEVEN_KEY", "test-eleven-key");
        std::env::set_var("TEST_GOOGLE_KEY", "test-google-key");

        let loader = ReadAloudConfigLoader::new(path);
        let resolved = loader.load().unwrap();

        assert_eq!(resolved.default_provider, ProviderKind::ElevenLabs);
        let sky = resolved.personas.get("sky").expect("sky persona missing");
        assert_eq!(sky.provider, ProviderKind::ElevenLabs);
        assert_eq!(sky.fallback_policy, "preserve-persona");
        assert!(sky.elevenlabs.is_some());
        assert!(sky.google.is_some());

        let eleven = sky.elevenlabs.as_ref().unwrap();
        assert_eq!(eleven.voice_id, "2tM0Teq5Piex0mNtlZnm");
        assert_eq!(eleven.voice_settings.speed, 1.2);
    }

    #[test]
    fn missing_tts_block_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");
        std::fs::write(&path, r#"{"messages": {}}"#).unwrap();
        let loader = ReadAloudConfigLoader::new(path);
        let err = loader.load().expect_err("should fail");
        assert!(err.to_string().contains("missing messages.tts block"));
    }

    #[test]
    fn rejects_unusable_default_provider() {
        let config = r#"
        {
            "messages": {
                "tts": {
                    "provider": "elevenlabs",
                    "providers": {
                        "google": {
                            "apiKey": { "source": "env", "id": "TEST_GOOGLE_KEY_UNUSABLE_DEFAULT" },
                            "voice": "Sulafat",
                            "model": "gemini-2.5-flash-preview-tts"
                        }
                    }
                }
            }
        }
        "#;
        std::env::set_var("TEST_GOOGLE_KEY_UNUSABLE_DEFAULT", "key");
        let file: ReadAloudDefaultsFile = serde_json::from_str(config).unwrap();
        let err = file
            .resolve()
            .expect_err("default provider should be unusable");
        assert!(err
            .to_string()
            .contains("ElevenLabs is selected but not configured"));
    }

    #[test]
    fn max_text_length_enforced_at_parse_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read-aloud-defaults.json");
        std::fs::write(&path, minimal_config()).unwrap();
        std::env::set_var("TEST_GOOGLE_KEY_MINIMAL", "test-google-key-value");

        let loader = ReadAloudConfigLoader::new(path);
        let resolved = loader.load().unwrap();
        assert_eq!(resolved.max_text_length, 1000);
    }
}
