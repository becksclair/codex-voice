use async_trait::async_trait;
use codex_voice_core::{
    RecordedAudio, TranscriptionClient, TranscriptionError, TranscriptionResult,
};
use reqwest::multipart;
use std::{path::Path, time::Duration};
use tokio::time;

use super::discovery;
use super::upload;

const MAX_PROBE_BYTES: u64 = 8 * 1024;

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
            let message = if body.len() > MAX_PROBE_BYTES as usize {
                format!("{}... (truncated)", &body[..MAX_PROBE_BYTES as usize])
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
        http::{header, HeaderMap, StatusCode},
        routing::get,
        Router,
    };

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
}
