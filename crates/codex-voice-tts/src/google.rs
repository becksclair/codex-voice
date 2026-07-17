use codex_voice_core::{SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech};
use reqwest::Client;

use crate::config::{GoogleRuntimeConfig, ResolvedPersona};
use crate::convert::convert_speech;
use crate::provider::TtsProvider;
use crate::provider_timeout::tts_timeout_for_input;
use crate::sanitize::sanitize_for_tts;

pub struct GoogleSpeechClient {
    config: GoogleRuntimeConfig,
    client: Client,
}

impl GoogleSpeechClient {
    pub fn new(config: GoogleRuntimeConfig) -> Result<Self, SpeechError> {
        let client = Client::builder()
            .build()
            .map_err(|e| SpeechError::Request(format!("failed to build HTTP client: {}", e)))?;
        Ok(Self { config, client })
    }

    pub fn supports_inline_audio_tags(&self, request: &SpeechRequest) -> bool {
        let model = self.resolved_model_id(request);
        self.config
            .inline_audio_tags
            .unwrap_or_else(|| google_model_supports_inline_audio_tags(model))
    }

    pub fn resolved_model_id<'a>(&'a self, request: &'a SpeechRequest) -> &'a str {
        self.config
            .models
            .iter()
            .find(|model| *model == &request.model_hint)
            .unwrap_or(&self.config.models[0])
    }

    pub fn max_text_length(&self) -> usize {
        self.config.max_text_length
    }

    pub async fn synthesize(
        &self,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        native_voice: Option<&str>,
    ) -> SpeechResult<SynthesizedSpeech> {
        let sanitized = sanitize_for_tts(&request.input, self.config.max_text_length)?;

        let model = self.resolved_model_id(request);

        let voice_name = persona
            .and_then(|p| p.google.as_ref())
            .map(|g| g.voice_name.as_str())
            .or(native_voice)
            .unwrap_or(self.config.voice.as_str());

        let prompt = build_prompt(&sanitized, persona, request.instructions.as_deref());

        let url = format!("{}/models/{}:generateContent", self.config.base_url, model);

        let body = serde_json::json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": prompt }]
                }
            ],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": {
                    "voiceConfig": {
                        "prebuiltVoiceConfig": {
                            "voiceName": voice_name
                        }
                    }
                }
            }
        });

        let timeout = tts_timeout_for_input(self.config.timeout, &sanitized);

        tracing::debug!(
            base_url = %self.config.base_url,
            model = %model,
            timeout_secs = timeout.as_secs(),
            text_chars = sanitized.chars().count(),
            "sending Google TTS request"
        );

        let native = tokio::time::timeout(timeout, async {
            let response = self
                .client
                .post(&url)
                .timeout(timeout)
                .header("x-goog-api-key", &self.config.api_key)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| SpeechError::Request(format!("Google TTS request failed: {}", e)))?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(SpeechError::Service {
                    status: status.as_u16(),
                    message: format!("Google TTS error: {}", text),
                });
            }

            let json: serde_json::Value = response.json().await.map_err(|e| {
                SpeechError::Request(format!("failed to parse Google TTS response: {}", e))
            })?;

            // Extract inlineData from candidates[0].content.parts
            let candidates = json
                .get("candidates")
                .and_then(|c| c.as_array())
                .ok_or_else(|| {
                    SpeechError::Request("Google TTS response missing candidates".into())
                })?;

            let first = candidates.first().ok_or_else(|| {
                SpeechError::Request("Google TTS response has no candidates".into())
            })?;

            let parts = first
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
                .ok_or_else(|| {
                    SpeechError::Request("Google TTS response missing content parts".into())
                })?;

            let mut audio_bytes = bytes::Bytes::new();
            let mut mime_type = "audio/wav".to_string();

            for part in parts {
                if let Some(inline) = part.get("inlineData") {
                    if let Some(data) = inline.get("data").and_then(|d| d.as_str()) {
                        audio_bytes = bytes::Bytes::from(
                            base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                data,
                            )
                            .map_err(|e| {
                                SpeechError::Request(format!(
                                    "failed to decode base64 audio: {}",
                                    e
                                ))
                            })?,
                        );
                    }
                    if let Some(mt) = inline.get("mimeType").and_then(|m| m.as_str()) {
                        mime_type = mt.to_string();
                    }
                    break;
                }
            }

            if audio_bytes.is_empty() {
                return Err(SpeechError::Request(
                    "Google TTS returned empty audio".into(),
                ));
            }

            let format = guess_format_from_mime(&mime_type).unwrap_or(SpeechFormat::Wav);

            Ok(SynthesizedSpeech {
                bytes: audio_bytes,
                format,
                mime_type,
                prepared_input: None,
            })
        })
        .await
        .map_err(|_| {
            SpeechError::Request(format!(
                "Google TTS request timed out after {}s",
                timeout.as_secs()
            ))
        })??;

        convert_speech(native, request.format).await
    }
}

#[async_trait::async_trait]
impl TtsProvider for GoogleSpeechClient {
    fn supports_inline_audio_tags(&self, request: &SpeechRequest) -> bool {
        GoogleSpeechClient::supports_inline_audio_tags(self, request)
    }

    fn resolved_model_id(&self, request: &SpeechRequest) -> SpeechResult<String> {
        Ok(GoogleSpeechClient::resolved_model_id(self, request).to_string())
    }

    fn max_text_length(&self, _request: &SpeechRequest) -> usize {
        GoogleSpeechClient::max_text_length(self)
    }

    async fn synthesize(
        &self,
        request: &SpeechRequest,
        persona: Option<&ResolvedPersona>,
        native_voice: Option<&str>,
    ) -> SpeechResult<SynthesizedSpeech> {
        GoogleSpeechClient::synthesize(self, request, persona, native_voice).await
    }
}

fn google_model_supports_inline_audio_tags(model: &str) -> bool {
    let normalized = model
        .strip_prefix("google/")
        .unwrap_or(model)
        .to_ascii_lowercase();
    normalized.contains("gemini-3.1") && normalized.contains("tts")
}

fn build_prompt(
    text: &str,
    persona: Option<&ResolvedPersona>,
    instructions: Option<&str>,
) -> String {
    let mut prompt = String::with_capacity(text.len() + 512);
    prompt.push_str("Read the following text aloud.\n\n");

    if let Some(p) = persona {
        prompt.push_str("Delivery profile:\n");
        if let Some(scene) = &p.prompt_scene {
            prompt.push_str("- scene: ");
            prompt.push_str(scene);
            prompt.push('\n');
        }
        if let Some(style) = &p.prompt_style {
            prompt.push_str("- style: ");
            prompt.push_str(style);
            prompt.push('\n');
        }
        if let Some(pacing) = &p.prompt_pacing {
            prompt.push_str("- pace: ");
            prompt.push_str(pacing);
            prompt.push('\n');
        }
        for constraint in &p.prompt_constraints {
            prompt.push_str("- constraint: ");
            prompt.push_str(constraint);
            prompt.push('\n');
        }
        prompt.push('\n');

        if let Some(sample) = &p.prompt_sample_context {
            prompt.push_str("Sample context: ");
            prompt.push_str(sample);
            prompt.push_str("\n\n");
        }
    }

    if let Some(instr) = instructions {
        prompt.push_str("Additional delivery hints:\n");
        prompt.push_str("- ");
        prompt.push_str(instr);
        prompt.push_str("\n\n");
    }

    prompt.push_str("Important:\n");
    prompt.push_str("- speak the text exactly as written\n");
    prompt.push_str("- do not add narration or commentary\n");
    prompt.push_str("- do not change wording or paraphrase\n\n");
    prompt.push_str("Text:\n\"\"\"");
    prompt.push_str(text);
    prompt.push_str("\"\"\"");

    prompt
}

fn guess_format_from_mime(mime: &str) -> Option<SpeechFormat> {
    // Strip parameters (e.g. "audio/L16;codec=pcm;rate=24000" -> "audio/L16")
    let base = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "audio/mpeg" | "audio/mp3" => Some(SpeechFormat::Mp3),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some(SpeechFormat::Wav),
        "audio/opus" => Some(SpeechFormat::Opus),
        "audio/aac" => Some(SpeechFormat::Aac),
        "audio/flac" => Some(SpeechFormat::Flac),
        "audio/l16" | "audio/pcm" => Some(SpeechFormat::Pcm),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{google_model_supports_inline_audio_tags, guess_format_from_mime};
    use codex_voice_core::SpeechFormat;

    #[test]
    fn guess_format_handles_lowercase_l16() {
        assert_eq!(
            guess_format_from_mime("audio/l16; rate=24000; channels=1"),
            Some(SpeechFormat::Pcm)
        );
    }

    #[test]
    fn guess_format_handles_uppercase_l16() {
        assert_eq!(
            guess_format_from_mime("audio/L16;codec=pcm;rate=24000"),
            Some(SpeechFormat::Pcm)
        );
    }

    #[test]
    fn guess_format_handles_wav_variants() {
        assert_eq!(guess_format_from_mime("audio/wav"), Some(SpeechFormat::Wav));
        assert_eq!(
            guess_format_from_mime("audio/x-wav"),
            Some(SpeechFormat::Wav)
        );
        assert_eq!(
            guess_format_from_mime("audio/wave"),
            Some(SpeechFormat::Wav)
        );
    }

    #[test]
    fn guess_format_returns_none_for_unknown() {
        assert_eq!(guess_format_from_mime("audio/ogg"), None);
        assert_eq!(guess_format_from_mime("text/plain"), None);
    }

    #[test]
    fn google_inline_audio_tags_are_model_gated() {
        assert!(google_model_supports_inline_audio_tags(
            "google/gemini-3.1-flash-preview-tts"
        ));
        assert!(google_model_supports_inline_audio_tags(
            "gemini-3.1-flash-tts"
        ));
        assert!(!google_model_supports_inline_audio_tags(
            "gemini-2.5-flash-preview-tts"
        ));
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::config::GoogleRuntimeConfig;
    use codex_voice_core::{SpeechFormat, SpeechRequest};

    /// Live Google TTS integration test.
    ///
    /// This test is ignored by default. To run it:
    ///
    /// ```bash
    /// export GOOGLE_API_KEY=your-key-here
    /// export CODEX_VOICE_TTS_LIVE=1
    /// cargo test -p codex-voice-tts google_live_synthesize -- --ignored
    /// ```
    #[tokio::test]
    #[ignore]
    async fn google_live_synthesize() {
        let live = std::env::var("CODEX_VOICE_TTS_LIVE").unwrap_or_default();
        if live != "1" {
            eprintln!("Skipping live Google TTS test; set CODEX_VOICE_TTS_LIVE=1 to enable");
            return;
        }

        let api_key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .expect("GEMINI_API_KEY or GOOGLE_API_KEY must be set for live test");

        let config = GoogleRuntimeConfig {
            api_key,
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            voice: "Sulafat".to_string(),
            models: vec!["gemini-2.5-flash-preview-tts".to_string()],
            inline_audio_tags: None,
            max_text_length: 1000,
            timeout: std::time::Duration::from_secs(120),
        };

        let client = GoogleSpeechClient::new(config).expect("client creation failed");
        let request = SpeechRequest {
            input: "Hello from Codex Voice live test.".to_string(),
            provider_hint: None,
            model_hint: "gpt-4o-mini-tts".to_string(),
            voice_hint: Some("sky".to_string()),
            speech_prep_enabled: None,
            speech_prep_model_hint: None,
            speech_prep_reasoning_effort: None,
            speech_prep_timeout_ms: None,
            instructions: None,
            format: SpeechFormat::Wav,
            speed: None,
        };

        let speech = client
            .synthesize(&request, None, None)
            .await
            .expect("live Google TTS synthesis failed");

        assert!(!speech.bytes.is_empty(), "synthesized audio is empty");
        assert!(
            speech.bytes.len() > 100,
            "synthesized audio seems too small: {} bytes",
            speech.bytes.len()
        );

        // Print header bytes for format diagnosis
        let header_preview: Vec<String> = speech.bytes[..16.min(speech.bytes.len())]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        eprintln!(
            "Live Google TTS ok: {} bytes, content-type: {}, header: [{}]",
            speech.bytes.len(),
            speech.mime_type,
            header_preview.join(", ")
        );

        // Validate WAV header only if mime type actually claims wav
        if speech.mime_type.contains("wav") {
            assert_eq!(
                &speech.bytes[..4],
                b"RIFF",
                "WAV header does not start with RIFF"
            );
            assert_eq!(
                &speech.bytes[8..12],
                b"WAVE",
                "WAV header does not contain WAVE at offset 8"
            );
        }
    }
}
