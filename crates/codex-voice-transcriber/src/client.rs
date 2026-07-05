use async_trait::async_trait;
use bytes::Bytes;
use codex_voice_core::{
    RecordedAudio, SpeechError, SpeechFormat, SpeechResult, SynthesizedSpeech, TranscriptionClient,
    TranscriptionError, TranscriptionResult,
};
use reqwest::multipart;
use serde::Serialize;
use std::{path::Path, time::Duration};
use tokio::time;

use super::discovery;
use super::upload;

const MAX_PROBE_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct LocalTranscriberClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl LocalTranscriberClient {
    pub async fn discover(probe_timeout: Duration, runtime_timeout: Duration) -> Option<Self> {
        let candidate = discovery::resolve_discovery_candidate()?;
        Self::discover_candidate(candidate, probe_timeout, runtime_timeout).await
    }

    async fn discover_candidate(
        candidate: discovery::DiscoveryCandidate,
        probe_timeout: Duration,
        runtime_timeout: Duration,
    ) -> Option<Self> {
        let http = reqwest::Client::builder()
            .timeout(runtime_timeout)
            .build()
            .expect("reqwest client builds");
        let url = discovery::health_url(&candidate.base_url);
        match time::timeout(
            probe_timeout,
            http.get(&url).bearer_auth(&candidate.token).send(),
        )
        .await
        {
            Ok(Ok(response)) if response.status().is_success() => {
                tracing::info!(url = %candidate.base_url, "local transcriber service is healthy");
                Some(Self {
                    base_url: candidate.base_url,
                    token: candidate.token,
                    http,
                })
            }
            Ok(Ok(response)) => {
                tracing::info!(
                    url = %candidate.base_url,
                    status = %response.status(),
                    "local transcriber service returned non-success status"
                );
                None
            }
            Ok(Err(error)) => {
                tracing::info!(url = %candidate.base_url, %error, "local transcriber service is unreachable");
                None
            }
            Err(_) => {
                tracing::info!(url = %candidate.base_url, "local transcriber service probe timed out");
                None
            }
        }
    }

    pub async fn synthesize_speech(&self, input: &str) -> SpeechResult<SynthesizedSpeech> {
        if input.trim().is_empty() {
            return Err(SpeechError::Unsupported("input is required".into()));
        }
        let url = discovery::speech_url(&self.base_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&LocalSpeechRequest {
                model: "gpt-4o-mini-tts",
                input,
                response_format: "wav",
            })
            .send()
            .await
            .map_err(|error| SpeechError::Request(format!("request to {url} failed: {error}")))?;
        let status = response.status();
        let format = response
            .headers()
            .get("X-Codex-Voice-Format")
            .and_then(|value| value.to_str().ok())
            .and_then(SpeechFormat::from_openai)
            .unwrap_or(SpeechFormat::Wav);
        let mime_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(format.mime_type())
            .to_string();
        let body = response.bytes().await.map_err(|error| {
            SpeechError::Request(format!("failed to read response body: {error}"))
        })?;
        if !status.is_success() {
            let message = response_body_preview(&body);
            return Err(SpeechError::Service {
                status: status.as_u16(),
                message: format!("local speech endpoint returned {status}: {message}"),
            });
        }
        Ok(SynthesizedSpeech {
            bytes: body,
            format,
            mime_type,
        })
    }
}

#[derive(Serialize)]
struct LocalSpeechRequest<'a> {
    model: &'static str,
    input: &'a str,
    response_format: &'static str,
}

fn response_body_preview(body: &Bytes) -> String {
    let text = String::from_utf8_lossy(body);
    let redacted = codex_voice_core::redact_diagnostics(&text);
    if redacted.len() > MAX_PROBE_BYTES {
        format!(
            "{}... (truncated)",
            truncate_utf8(&redacted, MAX_PROBE_BYTES)
        )
    } else {
        redacted
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[async_trait]
impl TranscriptionClient for LocalTranscriberClient {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
        let url = discovery::transcription_url(&self.base_url);
        let part = multipart::Part::file(&recording.path)
            .await
            .map_err(|error| {
                TranscriptionError::Request(format!(
                    "failed to open {}: {error}",
                    recording.path.display()
                ))
            })?
            .file_name(recording.filename.clone())
            .mime_str(&recording.content_type)
            .map_err(|error| TranscriptionError::Request(format!("invalid mime type: {error}")))?;
        let form = multipart::Form::new().part("file", part);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                TranscriptionError::Request(format!("request to {url} failed: {error}"))
            })?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            TranscriptionError::Request(format!("failed to read response body: {error}"))
        })?;
        if !status.is_success() {
            let message = if body.len() > MAX_PROBE_BYTES {
                format!("{}... (truncated)", truncate_utf8(&body, MAX_PROBE_BYTES))
            } else {
                body
            };
            return Err(TranscriptionError::Service {
                status: status.as_u16(),
                message: format!("local transcriber returned {status}: {message}"),
            });
        }
        upload::parse_openai_transcription_response(&body)
    }
}

pub async fn transcribe_path(
    backend: &dyn TranscriptionClient,
    path: &Path,
    filename: &str,
    content_type: &str,
) -> TranscriptionResult<String> {
    let recording = RecordedAudio {
        path: path.to_path_buf(),
        content_type: content_type.to_string(),
        filename: filename.to_string(),
        // Duration is not consumed by the local transcriber endpoint.
        duration: Duration::default(),
    };
    backend.transcribe(&recording).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body,
        http::{header, HeaderMap, HeaderName, StatusCode},
        routing::{get, post},
        Json, Router,
    };
    use serde_json::Value;

    #[tokio::test]
    async fn discover_sends_bearer_token_to_health_probe() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server local addr");
        let app = Router::new().route(
            "/healthz",
            get(|headers: HeaderMap| async move {
                if headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer test-token")
                {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve health probe");
        });

        let candidate = discovery::DiscoveryCandidate {
            base_url: format!("http://{addr}"),
            token: "test-token".to_string(),
        };

        let discovered = LocalTranscriberClient::discover_candidate(
            candidate,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await;

        server.abort();
        assert!(discovered.is_some());
    }

    #[tokio::test]
    async fn synthesize_speech_sends_openai_wav_request_without_voice() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server local addr");
        let app = Router::new().route(
            "/v1/audio/speech",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer test-token")
                );
                assert_eq!(body["model"], "gpt-4o-mini-tts");
                assert_eq!(body["input"], "hello");
                assert_eq!(body["response_format"], "wav");
                assert!(body.get("voice").is_none());
                (
                    [
                        (header::CONTENT_TYPE, "audio/wav"),
                        (HeaderName::from_static("x-codex-voice-format"), "wav"),
                    ],
                    body::Body::from("audio"),
                )
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve speech endpoint");
        });
        let client = LocalTranscriberClient {
            base_url: format!("http://{addr}"),
            token: "test-token".to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .expect("client builds"),
        };

        let speech = client.synthesize_speech("hello").await.expect("speech ok");

        server.abort();
        assert_eq!(&speech.bytes[..], b"audio");
        assert_eq!(speech.format, SpeechFormat::Wav);
        assert_eq!(speech.mime_type, "audio/wav");
    }

    #[test]
    fn response_body_preview_truncates_on_utf8_boundary() {
        let body = Bytes::from(format!("{}é", "a".repeat(MAX_PROBE_BYTES - 1)));

        let preview = response_body_preview(&body);

        assert!(preview.ends_with("... (truncated)"));
        assert!(!preview.contains('é'));
    }
}
