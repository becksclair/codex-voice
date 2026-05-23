use async_trait::async_trait;
use codex_voice_core::{
    RecordedAudio, TranscriptionClient, TranscriptionError, TranscriptionResult,
};
use reqwest::multipart;
use std::time::Duration;

use crate::auth::CodexAuthService;

const TRANSCRIBE_URL: &str = "https://chatgpt.com/backend-api/transcribe";

#[derive(Debug, Clone)]
pub struct CodexTranscriptionClient {
    auth: CodexAuthService,
    http: reqwest::Client,
}

impl CodexTranscriptionClient {
    pub fn new(auth: CodexAuthService) -> TranscriptionResult<Self> {
        Self::with_timeout(auth, Duration::from_secs(60))
    }

    pub fn with_timeout(auth: CodexAuthService, timeout: Duration) -> TranscriptionResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| {
                TranscriptionError::Request(format!("failed to build HTTP client: {error}"))
            })?;
        Ok(Self { auth, http })
    }
}

#[async_trait]
impl TranscriptionClient for CodexTranscriptionClient {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
        let auth = tokio::task::spawn_blocking({
            let auth_service = self.auth.clone();
            move || auth_service.read_or_refresh()
        })
        .await
        .map_err(|e| TranscriptionError::Auth(format!("auth task failed: {e}")))??;

        let file_part = multipart::Part::file(&recording.path)
            .await
            .map_err(|error| {
                TranscriptionError::Request(format!(
                    "failed to open {}: {error}",
                    recording.path.display()
                ))
            })?;
        let file_part = file_part
            .file_name(recording.filename.clone())
            .mime_str(&recording.content_type)
            .map_err(|error| {
                TranscriptionError::Request(format!("invalid multipart mime: {error}"))
            })?;
        let form = multipart::Form::new().part("file", file_part);

        let mut request = self
            .http
            .post(TRANSCRIBE_URL)
            .bearer_auth(&auth.access_token)
            .header("originator", "Codex Desktop")
            .header("User-Agent", "Codex Voice/0.1.0")
            .header("Accept", "application/json")
            .multipart(form);
        if let Some(account_id) = auth.account_id.as_deref() {
            request = request.header("ChatGPT-Account-Id", account_id);
        }

        let response = request
            .send()
            .await
            .map_err(|error| TranscriptionError::Request(error.to_string()))?;
        let status = response.status();
        if let Some(len) = response.content_length() {
            if len > 256 * 1024 {
                return Err(TranscriptionError::Request(format!(
                    "transcription failed with HTTP {status}; response body is {len} bytes, above 256 KiB cap"
                )));
            }
        }
        let text = response.text().await.map_err(|error| {
            TranscriptionError::Request(format!("failed to read response: {error}"))
        })?;
        if !status.is_success() {
            let redacted = codex_voice_core::redact_diagnostics(&text);
            let truncated = if redacted.len() > 1200 {
                let mut t = redacted;
                t.truncate(1200);
                t.push_str("...");
                t
            } else {
                redacted
            };
            return Err(TranscriptionError::Request(format!(
                "transcription failed with HTTP {status}: {truncated}"
            )));
        }
        parse_transcript(&text)
    }
}

#[derive(Debug, serde::Deserialize)]
struct TranscriptResponse {
    text: Option<String>,
    transcript: Option<String>,
}

pub fn parse_transcript(body: &str) -> TranscriptionResult<String> {
    if let Ok(parsed) = serde_json::from_str::<TranscriptResponse>(body) {
        if let Some(text) = parsed.text {
            return Ok(text);
        }
        if let Some(text) = parsed.transcript {
            return Ok(text);
        }
        return Err(TranscriptionError::Request(
            "transcription response JSON did not include text".into(),
        ));
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(TranscriptionError::Request(
            "empty transcription response".into(),
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_transcript_text() {
        assert_eq!(
            parse_transcript(r#"{"text":"hello"}"#).unwrap(),
            "hello".to_string()
        );
    }

    #[test]
    fn rejects_json_without_transcript_text() {
        assert!(parse_transcript(r#"{"status":"ok"}"#).is_err());
    }
}
