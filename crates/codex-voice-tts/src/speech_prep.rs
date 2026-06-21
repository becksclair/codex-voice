use codex_voice_core::{SpeechError, SpeechResult};
use reqwest::Client;

use crate::config::SpeechPrepConfig;
use crate::sanitize::sanitize_for_tts;

pub struct SpeechPrepClient {
    config: SpeechPrepConfig,
    client: Client,
}

impl SpeechPrepClient {
    pub fn new(config: SpeechPrepConfig) -> Result<Self, SpeechError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| {
                SpeechError::Request(format!("failed to build speech prep client: {e}"))
            })?;
        Ok(Self { config, client })
    }

    pub fn should_prepare(&self, text: &str) -> bool {
        let chars = text.chars().count();
        chars > self.config.threshold && chars > self.config.max_length
    }

    pub async fn prepare(&self, text: &str) -> SpeechResult<Option<String>> {
        if !self.should_prepare(text) {
            return Ok(None);
        }

        let input = prepare_input_for_prompt(text, self.config.max_input_length)?;
        let prompt = build_prompt(&input, self.config.max_length);
        let model = normalize_google_model_name(&self.config.model);
        let url = format!("{}/models/{}:generateContent", self.config.base_url, model);
        let max_output_tokens = (self.config.max_length / 3).clamp(64, 512);
        let body = serde_json::json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": prompt }]
                }
            ],
            "generationConfig": {
                "temperature": 0.2,
                "maxOutputTokens": max_output_tokens
            }
        });

        let prepared = tokio::time::timeout(self.config.timeout, async {
            let response = self
                .client
                .post(&url)
                .header("x-goog-api-key", &self.config.api_key)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| SpeechError::Request(format!("speech prep request failed: {e}")))?;

            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(SpeechError::Service {
                    status: status.as_u16(),
                    message: format!("speech prep error: {text}"),
                });
            }

            let json: serde_json::Value = response.json().await.map_err(|e| {
                SpeechError::Request(format!("failed to parse speech prep response: {e}"))
            })?;
            extract_text(&json).ok_or_else(|| {
                SpeechError::Request("speech prep response missing text output".into())
            })
        })
        .await
        .map_err(|_| {
            SpeechError::Request(format!(
                "speech prep request timed out after {}s",
                self.config.timeout.as_secs()
            ))
        })??;
        let sanitized = sanitize_for_tts(&prepared, usize::MAX)?;
        let shortened = truncate_chars(&sanitized, self.config.max_length);

        if shortened.is_empty() {
            return Err(SpeechError::Request(
                "speech prep returned empty text".into(),
            ));
        }

        Ok(Some(shortened))
    }
}

fn build_prompt(text: &str, max_length: usize) -> String {
    format!(
        "Prepare this text for text-to-speech playback. Preserve the user's meaning, key facts, decisions, and the full requested message. Shorten only when necessary to stay under {max_length} characters. Remove repetition, code blocks, URLs, file paths, and formatting noise. Return only natural speakable prose, no markdown, no preamble, no labels.\n\nText:\n\"\"\"{text}\"\"\""
    )
}

fn prepare_input_for_prompt(text: &str, max_input_length: usize) -> SpeechResult<String> {
    let sanitized = sanitize_for_tts(text, usize::MAX)?;
    Ok(truncate_chars(&sanitized, max_input_length))
}

fn extract_text(json: &serde_json::Value) -> Option<String> {
    let parts = json
        .get("candidates")?
        .as_array()?
        .first()?
        .get("content")?
        .get("parts")?
        .as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn normalize_google_model_name(model: &str) -> &str {
    model.strip_prefix("google/").unwrap_or(model)
}

fn truncate_chars(text: &str, max_length: usize) -> String {
    if text.chars().count() <= max_length {
        return text.to_string();
    }
    let limit = max_length.saturating_sub(1);
    let mut truncated = text.chars().take(limit).collect::<String>();
    truncated.truncate(truncated.trim_end().len());
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_provider_prefix_from_google_model() {
        assert_eq!(
            normalize_google_model_name("google/gemini-3-flash-preview"),
            "gemini-3-flash-preview"
        );
        assert_eq!(
            normalize_google_model_name("gemini-3-flash-preview"),
            "gemini-3-flash-preview"
        );
    }

    #[test]
    fn truncates_on_character_boundary() {
        assert_eq!(truncate_chars("hello world", 20), "hello world");
        assert_eq!(truncate_chars("hello world", 6), "hello…");
    }

    #[test]
    fn extracts_text_parts_from_response() {
        let json = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "short"}, {"text": "summary"}]
                }
            }]
        });
        assert_eq!(extract_text(&json).unwrap(), "short summary");
    }

    #[test]
    fn speech_prep_config_keeps_input_cap_at_or_above_threshold() {
        let config = SpeechPrepConfig {
            provider: crate::config::ProviderKind::Google,
            api_key: "key".to_string(),
            base_url: "https://example.test".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            threshold: 700,
            max_input_length: 700,
            max_length: 420,
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();

        assert!(client.should_prepare(&"x".repeat(701)));
    }

    #[test]
    fn speech_prep_skips_text_that_already_fits_output_limit() {
        let config = SpeechPrepConfig {
            provider: crate::config::ProviderKind::Google,
            api_key: "key".to_string(),
            base_url: "https://example.test".to_string(),
            model: "gemini-3-flash-preview".to_string(),
            threshold: 500,
            max_input_length: 12_000,
            max_length: 3000,
            timeout: std::time::Duration::from_secs(1),
        };
        let client = SpeechPrepClient::new(config).unwrap();

        assert!(!client.should_prepare(&"x".repeat(700)));
    }

    #[test]
    fn speech_prep_input_is_sanitized_and_truncated_without_rejection() {
        let text = format!("{} ```ignored code```", "word ".repeat(100));

        let result = prepare_input_for_prompt(&text, 40).unwrap();

        assert!(result.chars().count() <= 40);
        assert!(!result.contains("```"));
        assert!(result.ends_with('…'));
    }
}
