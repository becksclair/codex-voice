use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use super::models::{ProviderKind, ResolvedTtsConfig};
use super::provider::{resolve_elevenlabs_config, resolve_google_config, validate_default_path};

#[derive(Debug, Deserialize)]
pub struct ReadAloudDefaultsFile {
    pub messages: MessagesConfig,
    #[serde(default)]
    pub models: Option<ModelDefaults>,
}

#[derive(Debug, Deserialize)]
pub struct MessagesConfig {
    #[serde(default)]
    pub tts: Option<TtsDefaultsConfig>,
}

#[derive(Debug, Deserialize)]
pub struct TtsDefaultsConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(rename = "maxTextLength", default)]
    pub max_text_length: Option<usize>,
    #[serde(rename = "timeoutMs", default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub providers: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    pub personas: Option<HashMap<String, PersonaConfig>>,
}

#[derive(Debug, Deserialize)]
pub struct ModelDefaults {
    #[serde(default)]
    pub providers: Option<HashMap<String, ProviderModelConfig>>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderModelConfig {
    #[serde(rename = "baseUrl", default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PersonaConfig {
    pub label: String,
    pub description: String,
    pub provider: String,
    #[serde(rename = "fallbackPolicy", default)]
    pub fallback_policy: String,
    #[serde(default)]
    pub prompt: Option<PersonaPrompt>,
    #[serde(default)]
    pub providers: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
pub struct PersonaPrompt {
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub scene: Option<String>,
    #[serde(rename = "sampleContext", default)]
    pub sample_context: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub pacing: Option<String>,
    #[serde(default)]
    pub constraints: Option<Vec<String>>,
}

impl ReadAloudDefaultsFile {
    pub fn resolve(self) -> Result<ResolvedTtsConfig, codex_voice_core::SpeechError> {
        let tts = self.messages.tts.ok_or_else(|| {
            codex_voice_core::SpeechError::Config("missing messages.tts block".into())
        })?;

        let default_provider = tts
            .provider
            .as_deref()
            .and_then(ProviderKind::from_name)
            .ok_or_else(|| {
                codex_voice_core::SpeechError::Config("missing or invalid default provider".into())
            })?;

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
                let resolved = super::provider::resolve_persona(cfg)?;
                Ok((name, resolved))
            })
            .collect::<Result<HashMap<_, _>, codex_voice_core::SpeechError>>()?;

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
