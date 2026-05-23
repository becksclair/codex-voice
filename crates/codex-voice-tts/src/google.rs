use codex_voice_core::{SpeechError, SpeechFormat, SpeechRequest, SpeechResult, SynthesizedSpeech};
use reqwest::Client;

use crate::config::{GoogleRuntimeConfig, ResolvedPersona};
use crate::convert::convert_speech;
use crate::sanitize::sanitize_for_tts;

pub struct GoogleSpeechClient {
    config: GoogleRuntimeConfig,
    client: Client,
}

impl GoogleSpeechClient {
    pub fn new(config: GoogleRuntimeConfig) -> Result<Self, SpeechError> {
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

        let model = if self.config.model == request.model_hint || request.model_hint.is_empty() {
            &self.config.model
        } else {
            self.config
                .fallback_models
                .iter()
                .find(|m| *m == &request.model_hint)
                .unwrap_or(&self.config.model)
        };

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

        tracing::debug!(base_url = %self.config.base_url, model = %model, "sending Google TTS request");

        let response = self
            .client
            .post(&url)
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
            .ok_or_else(|| SpeechError::Request("Google TTS response missing candidates".into()))?;

        let first = candidates
            .first()
            .ok_or_else(|| SpeechError::Request("Google TTS response has no candidates".into()))?;

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
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
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

        let native = SynthesizedSpeech {
            bytes: audio_bytes,
            format,
            mime_type,
        };

        convert_speech(native, request.format).await
    }
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
    let base = mime.split(';').next().unwrap_or(mime).trim();
    match base {
        "audio/mpeg" | "audio/mp3" => Some(SpeechFormat::Mp3),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some(SpeechFormat::Wav),
        "audio/opus" => Some(SpeechFormat::Opus),
        "audio/aac" => Some(SpeechFormat::Aac),
        "audio/flac" => Some(SpeechFormat::Flac),
        "audio/L16" | "audio/pcm" => Some(SpeechFormat::Pcm),
        _ => None,
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
            model: "gemini-2.5-flash-preview-tts".to_string(),
            fallback_models: vec![],
            max_text_length: 1000,
            timeout: std::time::Duration::from_secs(120),
            scene: Some(
                "Lying by your side at home, chatting and daydreaming together.".to_string(),
            ),
            sample_context: Some("Sassy, sexy, enthusiastic, flirty.".to_string()),
            style: Some("Warm, sassy, playful, and flirtatious.".to_string()),
            pace: Some("Conversational and relaxed.".to_string()),
            constraints: vec![
                "Do not explain the persona or read configuration values aloud.".to_string(),
            ],
        };

        let client = GoogleSpeechClient::new(config).expect("client creation failed");
        let request = SpeechRequest {
            input: "Hello from Codex Voice live test.".to_string(),
            model_hint: "gpt-4o-mini-tts".to_string(),
            voice_hint: Some("sky".to_string()),
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
