use codex_voice_core::{SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech};
use reqwest::Client;

use crate::config::{ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ResolvedPersona};
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

    pub fn supports_inline_audio_tags(&self, request: &SpeechRequest) -> bool {
        let model_id = resolve_model_id(&request.model_hint, &self.config.model_id)
            .unwrap_or_else(|_| self.config.model_id.clone());
        self.config
            .inline_audio_tags
            .unwrap_or_else(|| elevenlabs_model_supports_inline_audio_tags(&model_id))
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
        let body = build_request_body(
            &sanitized,
            &model_id,
            &self.config.language_code,
            &self.config.apply_text_normalization,
            request.speed,
            persona_settings,
        );

        let url = format!("{}/v1/text-to-speech/{}", self.config.base_url, voice_id);

        tracing::debug!(url = %url, voice_id = %voice_id, "sending ElevenLabs TTS request");

        let bytes = tokio::time::timeout(self.config.timeout, async {
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

            response.bytes().await.map_err(|e| {
                SpeechError::Request(format!("failed to read ElevenLabs audio bytes: {}", e))
            })
        })
        .await
        .map_err(|_| {
            SpeechError::Request(format!(
                "ElevenLabs request timed out after {}s",
                self.config.timeout.as_secs()
            ))
        })??;

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
            prepared_input: None,
        };

        convert_speech(native, request.format).await
    }
}

/// Clamp and round ElevenLabs speed in f64 so serde_json serializes it cleanly.
/// f32 values like 1.2 arrive as 1.2000000476837158, so converting after
/// f32 rounding preserves the artifact instead of removing it.
fn normalize_speed(speed: f64) -> f64 {
    if speed.is_nan() {
        return 1.0;
    }

    let clamped = speed.clamp(0.7, 1.2);
    (clamped * 100.0).round() / 100.0
}

fn resolve_speed(
    request_speed: Option<f32>,
    persona_settings: Option<&ElevenLabsPersonaConfig>,
) -> f64 {
    request_speed
        .map(f64::from)
        .or_else(|| persona_settings.map(|e| e.voice_settings.speed))
        .unwrap_or(1.0)
}

fn build_request_body(
    text: &str,
    model_id: &str,
    language_code: &str,
    apply_text_normalization: &str,
    request_speed: Option<f32>,
    persona_settings: Option<&ElevenLabsPersonaConfig>,
) -> serde_json::Value {
    let speed = normalize_speed(resolve_speed(request_speed, persona_settings));
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

    serde_json::json!({
        "text": text,
        "model_id": model_id,
        "voice_settings": voice_settings,
        "language_code": language_code,
        "apply_text_normalization": apply_text_normalization,
    })
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

fn elevenlabs_model_supports_inline_audio_tags(model_id: &str) -> bool {
    let normalized = model_id.to_ascii_lowercase();
    normalized == "eleven_v3" || normalized.starts_with("eleven_v3_")
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

#[cfg(test)]
mod tests {
    use super::{build_request_body, elevenlabs_model_supports_inline_audio_tags, normalize_speed};
    use crate::config::{ElevenLabsPersonaConfig, ElevenLabsVoiceSettings};

    #[test]
    fn normalize_speed_serializes_upper_bound_without_f32_artifact() {
        let speed = normalize_speed(f64::from(1.2_f32));
        let body = serde_json::json!({ "voice_settings": { "speed": speed } });

        assert_eq!(body["voice_settings"]["speed"], 1.2);
        assert!(body.to_string().contains(r#""speed":1.2"#));
        assert!(!body.to_string().contains("1.2000000476837158"));
    }

    #[test]
    fn normalize_speed_clamps_to_elevenlabs_bounds() {
        assert_eq!(normalize_speed(0.1), 0.7);
        assert_eq!(normalize_speed(2.0), 1.2);
    }

    #[test]
    fn normalize_speed_defaults_nan_to_valid_speed() {
        assert_eq!(normalize_speed(f64::NAN), 1.0);
    }

    #[test]
    fn request_body_sends_upper_bound_speed_without_f32_artifact() {
        let body = build_request_body(
            "hello",
            "eleven_flash_v2_5",
            "en",
            "auto",
            Some(1.2_f32),
            None,
        )
        .to_string();

        assert!(
            body.contains(r#""speed":1.2"#),
            "request body should send speed 1.2 exactly, got {body}"
        );
        assert!(
            !body.contains("1.2000000476837158"),
            "request body leaked f32 artifact: {body}"
        );
    }

    #[test]
    fn request_body_preserves_persona_settings_and_normalizes_speed() {
        let persona = ElevenLabsPersonaConfig {
            voice_id: "voice-id".to_string(),
            voice_settings: ElevenLabsVoiceSettings {
                stability: 0.5,
                similarity_boost: 0.75,
                style: 0.3,
                use_speaker_boost: true,
                speed: 0.9,
            },
        };

        let body = build_request_body(
            "hello",
            "eleven_flash_v2_5",
            "en",
            "auto",
            Some(1.2_f32),
            Some(&persona),
        );

        assert_eq!(body["voice_settings"]["stability"], 0.5);
        assert_eq!(body["voice_settings"]["similarity_boost"], 0.75);
        assert_eq!(body["voice_settings"]["style"], 0.3);
        assert_eq!(body["voice_settings"]["use_speaker_boost"], true);
        assert_eq!(body["voice_settings"]["speed"], 1.2);

        let serialized = body.to_string();
        assert!(serialized.contains(r#""speed":1.2"#));
        assert!(!serialized.contains("1.2000000476837158"));
    }

    #[test]
    fn request_body_uses_persona_speed_without_downcasting_when_request_speed_absent() {
        let persona = ElevenLabsPersonaConfig {
            voice_id: "voice-id".to_string(),
            voice_settings: ElevenLabsVoiceSettings {
                stability: 0.5,
                similarity_boost: 0.75,
                style: 0.3,
                use_speaker_boost: true,
                speed: 1.185,
            },
        };

        let body = build_request_body(
            "hello",
            "eleven_flash_v2_5",
            "en",
            "auto",
            None,
            Some(&persona),
        );

        assert_eq!(body["voice_settings"]["speed"], 1.19);
        assert!(body.to_string().contains(r#""speed":1.19"#));
    }

    #[test]
    fn request_body_does_not_serialize_nan_speed_as_null() {
        let body = build_request_body(
            "hello",
            "eleven_flash_v2_5",
            "en",
            "auto",
            Some(f32::NAN),
            None,
        );

        assert_eq!(body["voice_settings"]["speed"], 1.0);
        assert!(body.to_string().contains(r#""speed":1.0"#));
        assert!(!body.to_string().contains(r#""speed":null"#));
    }

    #[test]
    fn elevenlabs_inline_audio_tags_are_model_gated() {
        assert!(elevenlabs_model_supports_inline_audio_tags("eleven_v3"));
        assert!(elevenlabs_model_supports_inline_audio_tags(
            "eleven_v3_alpha"
        ));
        assert!(!elevenlabs_model_supports_inline_audio_tags(
            "eleven_multilingual_v2"
        ));
    }
}
