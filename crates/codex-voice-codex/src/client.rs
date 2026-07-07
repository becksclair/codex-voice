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
    transcribe_url: String,
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
        Ok(Self {
            auth,
            http,
            transcribe_url: TRANSCRIBE_URL.to_string(),
        })
    }

    /// Test-only hook so the HTTP contract can be exercised against a loopback
    /// mock server instead of the real Codex backend. Not part of the public API.
    #[cfg(test)]
    fn with_base_url_for_tests(
        auth: CodexAuthService,
        timeout: Duration,
        url: String,
    ) -> TranscriptionResult<Self> {
        let mut client = Self::with_timeout(auth, timeout)?;
        client.transcribe_url = url;
        Ok(client)
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
            .post(&self.transcribe_url)
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
    use axum::{
        body::Bytes,
        extract::Multipart,
        http::{header, HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Json, Router,
    };
    use tempfile::NamedTempFile;

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

    const TEST_TOKEN: &str = "test-access-token";

    fn wav_recording() -> (RecordedAudio, NamedTempFile) {
        let file = NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(file.path(), spec).unwrap();
        for _ in 0..1_600 {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();

        let recording = RecordedAudio {
            path: file.path().to_path_buf(),
            content_type: "audio/wav".into(),
            filename: "clip.wav".into(),
            duration: Duration::from_millis(100),
        };
        (recording, file)
    }

    fn auth_service_with_fixture(dir: &tempfile::TempDir) -> CodexAuthService {
        let auth_path = dir.path().join("auth.json");
        std::fs::write(
            &auth_path,
            format!(r#"{{"tokens":{{"access_token":"{TEST_TOKEN}","account_id":"acct-1"}}}}"#),
        )
        .unwrap();
        CodexAuthService::with_auth_path(auth_path)
    }

    async fn handle_transcribe_ok(
        headers: HeaderMap,
        mut multipart: Multipart,
    ) -> impl IntoResponse {
        let bearer_ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            == Some(&format!("Bearer {TEST_TOKEN}"));
        let content_type_is_multipart = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("multipart/form-data"))
            .unwrap_or(false);

        let mut saw_file_field = false;
        while let Ok(Some(field)) = multipart.next_field().await {
            if field.name() == Some("file") {
                saw_file_field = true;
            }
        }

        if bearer_ok && content_type_is_multipart && saw_file_field {
            (
                StatusCode::OK,
                Json(serde_json::json!({ "text": "hello from mock" })),
            )
        } else {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "bearer_ok={bearer_ok} content_type_is_multipart={content_type_is_multipart} saw_file_field={saw_file_field}"
                    )
                })),
            )
        }
    }

    // Take the whole request body as `Bytes` so the handler buffers the entire
    // multipart upload before responding. Without this, the server can respond
    // 500 and close while the client is still streaming the WAV, and reqwest
    // surfaces a send error instead of the 500 — a timing race that made this
    // test flaky.
    async fn handle_transcribe_service_error(_body: Bytes) -> impl IntoResponse {
        (StatusCode::INTERNAL_SERVER_ERROR, "mock service failure")
    }

    #[tokio::test]
    async fn transcribe_sends_bearer_and_multipart_and_parses_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server local addr");
        let app = Router::new().route("/transcribe", post(handle_transcribe_ok));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve transcribe endpoint");
        });

        let auth_dir = tempfile::tempdir().unwrap();
        let auth = auth_service_with_fixture(&auth_dir);
        let client = CodexTranscriptionClient::with_base_url_for_tests(
            auth,
            Duration::from_secs(5),
            format!("http://{addr}/transcribe"),
        )
        .expect("client builds");

        let (recording, _file) = wav_recording();
        let transcript = client.transcribe(&recording).await;

        server.abort();
        assert_eq!(
            transcript.expect("transcription succeeds"),
            "hello from mock"
        );
    }

    #[tokio::test]
    async fn transcribe_maps_non_2xx_to_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server local addr");
        let app = Router::new().route("/transcribe", post(handle_transcribe_service_error));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve transcribe endpoint");
        });

        let auth_dir = tempfile::tempdir().unwrap();
        let auth = auth_service_with_fixture(&auth_dir);
        let client = CodexTranscriptionClient::with_base_url_for_tests(
            auth,
            Duration::from_secs(5),
            format!("http://{addr}/transcribe"),
        )
        .expect("client builds");

        let (recording, _file) = wav_recording();
        let result = client.transcribe(&recording).await;

        server.abort();
        match result {
            Err(TranscriptionError::Request(message)) => {
                assert!(message.contains("500"), "message was: {message}");
            }
            other => panic!("expected TranscriptionError::Request, got {other:?}"),
        }
    }
}
