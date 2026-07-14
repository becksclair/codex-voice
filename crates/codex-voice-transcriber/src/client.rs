use async_trait::async_trait;
use bytes::Bytes;
use codex_voice_core::{
    RecordedAudio, SpeechError, SpeechFormat, SpeechResult, SynthesizedSpeech, TranscriptionClient,
    TranscriptionError, TranscriptionResult,
};
use reqwest::multipart;
use serde::Serialize;
use std::sync::OnceLock;
use std::{path::Path, time::Duration};
use tokio::time;

use super::discovery;
use super::upload;

const MAX_PROBE_BYTES: usize = 8 * 1024;

/// A process-wide client used only for `/healthz` probes. Discovery may probe
/// dozens of times while polling for a self-hosted server to come up; building
/// a fresh `reqwest::Client` (connection pool + resolver) per probe is wasted
/// work, and the per-request timeout is applied on the request instead.
fn probe_client() -> &'static reqwest::Client {
    static PROBE_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    PROBE_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .build()
            .expect("reqwest client builds")
    })
}

#[derive(Clone)]
pub struct LocalTranscriberClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl LocalTranscriberClient {
    pub(crate) fn from_service(
        base_url: String,
        token: String,
        runtime_timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            base_url,
            token,
            http: reqwest::Client::builder()
                .timeout(runtime_timeout)
                .build()?,
        })
    }

    pub async fn discover(probe_timeout: Duration, runtime_timeout: Duration) -> Option<Self> {
        let candidate = discovery::resolve_discovery_candidate()?;
        Self::discover_candidate(candidate, probe_timeout, runtime_timeout).await
    }

    pub async fn connect_desktop_origin(
        root_url: &str,
        probe_timeout: Duration,
        runtime_timeout: Duration,
    ) -> Option<Self> {
        let client = Self::from_service(
            format!("{}/v1", root_url.trim_end_matches('/')),
            String::new(),
            runtime_timeout,
        )
        .ok()?;
        client
            .health_responding(probe_timeout)
            .await
            .then_some(client)
    }

    /// Discovers using only the discovery file, ignoring the
    /// `CODEX_VOICE_TRANSCRIBER_URL` env override, which a stale value would
    /// otherwise shadow on every probe.
    pub async fn discover_from_file(
        probe_timeout: Duration,
        runtime_timeout: Duration,
    ) -> Option<Self> {
        let candidate = discovery::discovery_candidate_from_parts(
            None,
            None,
            discovery::read_discovery_file(),
            discovery::pid_is_running,
        )?;
        Self::discover_candidate(candidate, probe_timeout, runtime_timeout).await
    }

    /// Like [`Self::discover_from_file`], but only accepts a discovery entry
    /// written by this process. Used when polling for a server this process
    /// just spawned in-process, so an external server that writes the shared
    /// discovery file in the same window cannot be mistaken for it.
    pub async fn discover_own_file(
        probe_timeout: Duration,
        runtime_timeout: Duration,
    ) -> Option<Self> {
        let candidate = discovery::discovery_candidate_from_parts(
            None,
            None,
            discovery::read_discovery_file().filter(|file| file.pid == std::process::id()),
            discovery::pid_is_running,
        )?;
        Self::discover_candidate(candidate, probe_timeout, runtime_timeout).await
    }

    async fn discover_candidate(
        candidate: discovery::DiscoveryCandidate,
        probe_timeout: Duration,
        runtime_timeout: Duration,
    ) -> Option<Self> {
        let url = discovery::health_url(&candidate.base_url);
        match time::timeout(
            probe_timeout,
            probe_client()
                .get(&url)
                .timeout(probe_timeout)
                .bearer_auth(&candidate.token)
                .send(),
        )
        .await
        {
            Ok(Ok(response)) if response.status().is_success() => {
                tracing::info!(url = %candidate.base_url, "local transcriber service is healthy");
                // Only now — on a healthy probe — build the real client with
                // the request timeout that transcription/speech calls need.
                let http = reqwest::Client::builder()
                    .timeout(runtime_timeout)
                    .build()
                    .expect("reqwest client builds");
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

    /// The base URL this client sends requests to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The service root URL (`scheme://host:port`) with any OpenAI-style path
    /// suffix (e.g. `/v1`) and trailing slashes stripped, suitable for joining
    /// browser routes such as `/web`.
    pub fn web_root_url(&self) -> String {
        discovery::root_url(&self.base_url)
    }

    pub async fn desktop_ready(&self, probe_timeout: Duration) -> bool {
        #[derive(serde::Deserialize)]
        struct Health {
            capabilities: discovery::ServiceCapabilities,
        }

        let url = discovery::health_url(&self.base_url);
        let response = match self.http.get(&url).timeout(probe_timeout).send().await {
            Ok(response) if response.status().is_success() => response,
            _ => return false,
        };
        match response.json::<Health>().await {
            Ok(health) => health.capabilities.speech && health.capabilities.desktop,
            Err(_) => false,
        }
    }

    pub async fn wait_for_desktop_ready(
        &self,
        probe_timeout: Duration,
        max_wait: Duration,
    ) -> bool {
        let deadline = time::Instant::now() + max_wait;
        loop {
            if self.desktop_ready(probe_timeout).await {
                return true;
            }
            if time::Instant::now() >= deadline {
                return false;
            }
            time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn health_responding(&self, probe_timeout: Duration) -> bool {
        let url = discovery::health_url(&self.base_url);
        self.http
            .get(url)
            .timeout(probe_timeout)
            .send()
            .await
            .is_ok()
    }

    pub async fn create_desktop_intent(&self, text: &str) -> Result<String, String> {
        #[derive(Serialize)]
        struct RequestBody<'a> {
            text: &'a str,
        }
        #[derive(serde::Deserialize)]
        struct ResponseBody {
            id: String,
        }

        let url = format!("{}/web/desktop-intents", self.web_root_url());
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&RequestBody { text })
            .send()
            .await
            .map_err(|error| format!("request to {url} failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .bytes()
                .await
                .map_err(|error| format!("failed to read desktop intent response: {error}"))?;
            return Err(format!(
                "desktop intent endpoint returned {status}: {}",
                response_body_preview(&body)
            ));
        }
        let body = response
            .json::<ResponseBody>()
            .await
            .map_err(|error| format!("invalid desktop intent response: {error}"))?;
        Ok(body.id)
    }

    pub async fn delete_desktop_intent(&self, id: &str) {
        let url = format!("{}/web/desktop-intents/{}", self.web_root_url(), id);
        let _ = self.http.delete(url).bearer_auth(&self.token).send().await;
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
            prepared_input: None,
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
            codex_voice_core::truncate_utf8(&redacted, MAX_PROBE_BYTES)
        )
    } else {
        redacted
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
            let message = if body.len() > MAX_PROBE_BYTES {
                format!(
                    "{}... (truncated)",
                    codex_voice_core::truncate_utf8(&body, MAX_PROBE_BYTES)
                )
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

    #[test]
    fn base_url_exposes_configured_url() {
        let client = LocalTranscriberClient {
            base_url: "http://127.0.0.1:1234".to_string(),
            token: "test-token".to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(1))
                .build()
                .expect("client builds"),
        };

        assert_eq!(client.base_url(), "http://127.0.0.1:1234");
    }

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
