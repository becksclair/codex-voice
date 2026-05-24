use std::collections::HashMap;
use std::time::Duration;

use super::models::{
    ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ElevenLabsVoiceSettings, FallbackPolicy,
    GooglePersonaConfig, GoogleRuntimeConfig, ProviderKind, ResolvedPersona, SpeechPrepConfig,
};
use serde_json::Value;

pub fn validate_default_path(
    default_provider: ProviderKind,
    default_persona: Option<&str>,
    google: &Option<GoogleRuntimeConfig>,
    elevenlabs: &Option<ElevenLabsRuntimeConfig>,
    personas: &HashMap<String, ResolvedPersona>,
) -> Result<(), codex_voice_core::SpeechError> {
    let Some(persona_name) = default_persona else {
        return validate_provider(default_provider, google, elevenlabs).map_err(|message| {
            codex_voice_core::SpeechError::Config(format!(
                "default provider is not usable: {message}"
            ))
        });
    };

    let persona = personas.get(persona_name).ok_or_else(|| {
        codex_voice_core::SpeechError::Config(format!(
            "default persona {persona_name:?} is not defined"
        ))
    })?;

    if persona_provider_usable(persona, persona.provider, google, elevenlabs) {
        return Ok(());
    }

    if persona.fallback_policy == FallbackPolicy::PreservePersona {
        let fallback = match persona.provider {
            ProviderKind::Google => ProviderKind::ElevenLabs,
            ProviderKind::ElevenLabs => ProviderKind::Google,
        };
        if persona_provider_usable(persona, fallback, google, elevenlabs) {
            return Ok(());
        }
    }

    Err(codex_voice_core::SpeechError::Config(format!(
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

fn json_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn json_f64(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn json_bool(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn json_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
}

fn json_string(value: &Value, key: &str, default: &str) -> String {
    json_str(value, key).unwrap_or(default).to_string()
}

fn json_string_opt(value: &Value, key: &str) -> Option<String> {
    json_str(value, key).map(String::from)
}

fn json_string_vec(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn persona_provider_usable(
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

pub fn resolve_google_config(
    providers: &HashMap<String, Value>,
    models: &HashMap<String, super::serde::ProviderModelConfig>,
    max_text_length: usize,
    timeout: Duration,
) -> Result<Option<GoogleRuntimeConfig>, codex_voice_core::SpeechError> {
    let Some(val) = providers.get("google") else {
        return Ok(None);
    };

    let api_key =
        crate::secret::resolve_secret(val.get("apiKey"), "GEMINI_API_KEY", "GOOGLE_API_KEY")?;

    let base_url = json_str(val, "baseUrl")
        .or_else(|| models.get("google").and_then(|m| m.base_url.as_deref()))
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta")
        .to_string();

    let voice = json_string(val, "voice", "Sulafat");
    let model = json_string(val, "model", "gemini-2.5-flash-preview-tts");
    let fallback_models = json_string_vec(val, "fallbackModels");

    let scene = json_string_opt(val, "scene");
    let sample_context = json_string_opt(val, "sampleContext");
    let style = json_string_opt(val, "style");
    let pace = json_string_opt(val, "pace");
    let constraints = json_string_vec(val, "constraints");

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

pub fn resolve_speech_prep_config(
    raw: Option<&Value>,
    providers: &HashMap<String, Value>,
    models: &HashMap<String, super::serde::ProviderModelConfig>,
    max_text_length: usize,
) -> Result<Option<SpeechPrepConfig>, codex_voice_core::SpeechError> {
    let Some(val) = raw else {
        return Ok(None);
    };

    if val.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Ok(None);
    }

    let provider_name = json_str(val, "provider").unwrap_or("google");
    let provider = ProviderKind::from_name(provider_name).ok_or_else(|| {
        codex_voice_core::SpeechError::Config(format!(
            "invalid speechPrep provider: {provider_name}"
        ))
    })?;
    if provider != ProviderKind::Google {
        return Err(codex_voice_core::SpeechError::Config(
            "speechPrep currently supports only the google provider".into(),
        ));
    }

    let google_provider = providers.get("google");
    let api_key = crate::secret::resolve_secret(
        val.get("apiKey")
            .or_else(|| google_provider.and_then(|provider| provider.get("apiKey"))),
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
    )?;
    let base_url = json_str(val, "baseUrl")
        .or_else(|| google_provider.and_then(|provider| json_str(provider, "baseUrl")))
        .or_else(|| models.get("google").and_then(|m| m.base_url.as_deref()))
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta")
        .to_string();
    let model = json_string(val, "model", "gemini-3-flash-preview");
    let threshold = json_usize(val, "threshold")
        .unwrap_or(500)
        .min(max_text_length);
    let max_input_length = json_usize(val, "maxInputLength")
        .unwrap_or(12_000)
        .max(threshold);
    let max_length = json_usize(val, "maxLength")
        .unwrap_or(500)
        .max(80)
        .min(max_text_length);
    let timeout = val
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(20))
        .min(Duration::from_secs(30));

    Ok(Some(SpeechPrepConfig {
        provider,
        api_key,
        base_url,
        model,
        threshold,
        max_input_length,
        max_length,
        timeout,
    }))
}

pub fn resolve_elevenlabs_config(
    providers: &HashMap<String, Value>,
    models: &HashMap<String, super::serde::ProviderModelConfig>,
    max_text_length: usize,
    timeout: Duration,
) -> Result<Option<ElevenLabsRuntimeConfig>, codex_voice_core::SpeechError> {
    let Some(val) = providers.get("elevenlabs") else {
        return Ok(None);
    };

    let api_key =
        crate::secret::resolve_secret(val.get("apiKey"), "ELEVENLABS_API_KEY", "ELEVEN_API_KEY")?;

    let base_url = json_str(val, "baseUrl")
        .or_else(|| models.get("elevenlabs").and_then(|m| m.base_url.as_deref()))
        .unwrap_or("https://api.elevenlabs.io")
        .to_string();

    let model_id = json_string(val, "modelId", "eleven_multilingual_v2");
    let apply_text_normalization = json_string(val, "applyTextNormalization", "auto");
    let output_format = json_string(val, "outputFormat", "mp3_44100_128");
    let language_code = json_string(val, "languageCode", "en");

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

pub fn resolve_persona(
    cfg: super::serde::PersonaConfig,
) -> Result<ResolvedPersona, codex_voice_core::SpeechError> {
    let provider = ProviderKind::from_name(&cfg.provider).ok_or_else(|| {
        codex_voice_core::SpeechError::Config(format!("invalid persona provider: {}", cfg.provider))
    })?;

    let prompt = cfg.prompt;

    let google = cfg
        .providers
        .as_ref()
        .and_then(|m| m.get("google"))
        .map(|raw| GooglePersonaConfig {
            voice_name: json_string(raw, "voiceName", "Sulafat"),
            prompt_template: json_string(raw, "promptTemplate", "audio-profile-v1"),
            persona_prompt: json_string(raw, "personaPrompt", ""),
        });

    let elevenlabs = cfg
        .providers
        .as_ref()
        .and_then(|m| m.get("elevenlabs"))
        .map(|raw| {
            let voice_settings = raw.get("voiceSettings");
            ElevenLabsPersonaConfig {
                voice_id: json_string(raw, "voiceId", ""),
                voice_settings: ElevenLabsVoiceSettings {
                    stability: voice_settings
                        .and_then(|settings| json_f64(settings, "stability"))
                        .unwrap_or(0.5),
                    similarity_boost: voice_settings
                        .and_then(|settings| json_f64(settings, "similarityBoost"))
                        .unwrap_or(0.75),
                    style: voice_settings
                        .and_then(|settings| json_f64(settings, "style"))
                        .unwrap_or(0.0),
                    use_speaker_boost: voice_settings
                        .and_then(|settings| json_bool(settings, "useSpeakerBoost"))
                        .unwrap_or(true),
                    speed: voice_settings
                        .and_then(|settings| json_f64(settings, "speed"))
                        .unwrap_or(1.0),
                },
            }
        });

    Ok(ResolvedPersona {
        label: cfg.label,
        description: cfg.description,
        provider,
        fallback_policy: FallbackPolicy::from_name(&cfg.fallback_policy)
            .unwrap_or(FallbackPolicy::Strict),
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
