use codex_voice_core::{SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech};
use reqwest::Client;

use crate::config::{ElevenLabsRuntimeConfig, ResolvedPersona};
use crate::convert::convert_speech;
use crate::sanitize::sanitize_for_tts;

pub struct ElevenLabsSpeechClient {
    config: ElevenLabsRuntimeConfig,
    client: Client,
}

impl ElevenLabsSpeechClient {
    pub fn new(config: ElevenLabsRuntimeConfig) -> Result<Self, SpeechError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| SpeechError::Request(format!("failed to build HTTP client: {}", e)))?;
        Ok(Self { config, client })
    }

    pub async fn synthesize(
        &self,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        native_voice: Option<&str>,
    ) -> SpeechResult<SynthesizedSpeech> {
        let sanitized = sanitize_for_tts(&request.input, self.config.max_text_length)?;

        let voice_id = persona
            .and_then(|p| p.elevenlabs.as_ref())
            .map(|e| e.voice_id.as_str())
            .or(native_voice)
            .unwrap_or("");

        if voice_id.is_empty() {
            return Err(SpeechError::Config(
                "ElevenLabs voice_id is required but not configured for this persona".into(),
            ));
        }

        let model_id = resolve_model_id(&request.model_hint, &self.config.model_id)?;

        let persona_settings = persona.and_then(|p| p.elevenlabs.as_ref());
        let speed = normalize_speed(
            request
                .speed
                .or_else(|| persona_settings.map(|e| e.voice_settings.speed as f32))
                .unwrap_or(1.0),
        );

        let voice_settings = if let Some(e) = persona_settings {
            serde_json::json!({
                "stability": e.voice_settings.stability,
                "similarity_boost": e.voice_settings.similarity_boost,
                "style": e.voice_settings.style,
                "use_speaker_boost": e.voice_settings.use_speaker_boost,
                "speed": speed,
            })
        } else {
            serde_json::json!({ "speed": speed })
        };

        let body = serde_json::json!({
            "text": sanitized,
            "model_id": model_id,
            "voice_settings": voice_settings,
            "language_code": self.config.language_code,
            "apply_text_normalization": self.config.apply_text_normalization,
        });

        let url = format!("{}/v1/text-to-speech/{}", self.config.base_url, voice_id);

        tracing::debug!(url = %url, voice_id = %voice_id, "sending ElevenLabs TTS request");

        let response = self
            .client
            .post(&url)
            .query(&[("output_format", &self.config.output_format)])
            .header("xi-api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SpeechError::Request(format!("ElevenLabs request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 || text.contains("quota_exceeded") {
                return Err(SpeechError::RateLimited(format!(
                    "ElevenLabs quota/rate limit error: {text}"
                )));
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(SpeechError::Auth(format!(
                    "ElevenLabs authentication error: {text}"
                )));
            }
            return Err(SpeechError::Service {
                status: status.as_u16(),
                message: format!("ElevenLabs error: {}", text),
            });
        }

        let bytes = response.bytes().await.map_err(|e| {
            SpeechError::Request(format!("failed to read ElevenLabs audio bytes: {}", e))
        })?;

        if bytes.is_empty() {
            return Err(SpeechError::Request(
                "ElevenLabs returned empty audio".into(),
            ));
        }

        let format = format_from_elevenlabs_output(&self.config.output_format);
        let mime_type = format.mime_type().to_string();

        let native = SynthesizedSpeech {
            bytes,
            format,
            mime_type,
        };

        convert_speech(native, request.format).await
    }
}

/// Clamp and round ElevenLabs speed to avoid f32 serialization artifacts
/// (e.g. 1.2 → 1.2000000476837158) that its strict validator rejects.
fn normalize_speed(speed: f32) -> f32 {
    let clamped = speed.clamp(0.7, 1.2);
    (clamped * 100.0).round() / 100.0
}

fn resolve_model_id(model_hint: &str, configured: &str) -> SpeechResult<String> {
    if model_hint.is_empty()
        || model_hint == configured
        || model_hint.starts_with("tts-")
        || model_hint.starts_with("gpt-")
    {
        return Ok(configured.to_string());
    }

    if model_hint.starts_with("eleven_") {
        return Ok(model_hint.to_string());
    }

    Err(SpeechError::Unsupported(format!(
        "unsupported ElevenLabs model override {model_hint:?}; use an ElevenLabs model id or omit model to use configured {configured:?}"
    )))
}

fn format_from_elevenlabs_output(output_format: &str) -> SpeechFormat {
    if output_format.starts_with("mp3") {
        SpeechFormat::Mp3
    } else if output_format.starts_with("wav") {
        SpeechFormat::Wav
    } else if output_format.starts_with("pcm") {
        SpeechFormat::Pcm
    } else if output_format.starts_with("opus") {
        SpeechFormat::Opus
    } else {
        SpeechFormat::Mp3
    }
}
