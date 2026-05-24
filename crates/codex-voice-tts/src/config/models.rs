use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Google,
    ElevenLabs,
}

impl ProviderKind {
    pub fn from_name(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("google") {
            Some(Self::Google)
        } else if s.eq_ignore_ascii_case("elevenlabs") {
            Some(Self::ElevenLabs)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackPolicy {
    PreservePersona,
    Strict,
}

impl FallbackPolicy {
    pub fn from_name(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("preserve-persona") {
            Some(Self::PreservePersona)
        } else if s.eq_ignore_ascii_case("strict") {
            Some(Self::Strict)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTtsConfig {
    pub default_provider: ProviderKind,
    pub default_persona: Option<String>,
    pub max_text_length: usize,
    pub timeout: Duration,
    pub speech_prep: Option<SpeechPrepConfig>,
    pub google: Option<GoogleRuntimeConfig>,
    pub elevenlabs: Option<ElevenLabsRuntimeConfig>,
    pub personas: HashMap<String, ResolvedPersona>,
}

#[derive(Debug, Clone)]
pub struct SpeechPrepConfig {
    pub provider: ProviderKind,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub threshold: usize,
    pub max_input_length: usize,
    pub max_length: usize,
    pub timeout: Duration,
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
    pub fallback_policy: FallbackPolicy,
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
