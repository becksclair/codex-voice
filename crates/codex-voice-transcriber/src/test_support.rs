use axum::body;
use axum::http::{self, header};
use codex_voice_core::{
    SpeechClient, SpeechRequest, SpeechResult, SynthesizedSpeech, TranscriptionClient,
    TranscriptionResult,
};
use std::sync::{Arc, Mutex};

use crate::server::{ServiceAuth, ServiceState};

#[derive(Default)]
pub struct FakeBackend {
    pub seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl TranscriptionClient for FakeBackend {
    async fn transcribe(
        &self,
        recording: &codex_voice_core::RecordedAudio,
    ) -> TranscriptionResult<String> {
        self.seen
            .lock()
            .expect("fake backend lock")
            .push(recording.filename.clone());
        Ok("hello from service".into())
    }
}

#[derive(Default)]
pub struct FakeSpeechBackend {
    pub seen: Mutex<Vec<SpeechRequest>>,
    pub prepared_input: Option<String>,
}

#[async_trait::async_trait]
impl SpeechClient for FakeSpeechBackend {
    async fn synthesize(&self, request: &SpeechRequest) -> SpeechResult<SynthesizedSpeech> {
        self.seen
            .lock()
            .expect("fake speech lock")
            .push(request.clone());
        Ok(SynthesizedSpeech {
            bytes: bytes::Bytes::from_static(b"fake audio bytes"),
            format: request.format,
            mime_type: request.format.mime_type().to_string(),
            prepared_input: self.prepared_input.clone(),
        })
    }
}

pub(crate) fn test_state(codex_upload_limit_bytes: u64) -> ServiceState {
    test_state_with_speech_backend(codex_upload_limit_bytes, None)
}

pub(crate) fn test_state_with_speech(codex_upload_limit_bytes: u64) -> ServiceState {
    test_state_with_speech_backend(
        codex_upload_limit_bytes,
        Some(Arc::new(FakeSpeechBackend::default())),
    )
}

pub(crate) fn test_state_with_speech_backend(
    codex_upload_limit_bytes: u64,
    speech: Option<Arc<dyn SpeechClient>>,
) -> ServiceState {
    ServiceState {
        backend: Arc::new(FakeBackend::default()),
        speech,
        auth: ServiceAuth {
            token: "test-token".into(),
            no_auth: false,
        },
        codex_upload_limit_bytes,
        client_upload_limit_bytes: 1024 * 1024,
        chunk_seconds: 600,
        ffmpeg_binary: "definitely-not-ffmpeg".into(),
    }
}

pub fn speech_request(path: &str, body: &str, token: Option<&str>) -> http::Request<body::Body> {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder
        .body(body::Body::from(body.to_string()))
        .expect("request builds")
}

pub fn multipart_request(path: &str, body: &str, token: Option<&str>) -> http::Request<body::Body> {
    let boundary = "codex-voice-test";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"input.wav\"\r\nContent-Type: audio/wav\r\n\r\n{body}\r\n--{boundary}--\r\n"
    );
    let mut builder = http::Request::builder().method("POST").uri(path).header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={boundary}"),
    );
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder
        .body(body::Body::from(payload))
        .expect("request builds")
}

pub fn multipart_request_with_response_format(
    path: &str,
    response_format: &str,
    token: Option<&str>,
) -> http::Request<body::Body> {
    let boundary = "codex-voice-test";
    let payload = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"input.wav\"\r\nContent-Type: audio/wav\r\n\r\ntiny wav\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\n{response_format}\r\n--{boundary}--\r\n"
    );
    let mut builder = http::Request::builder().method("POST").uri(path).header(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={boundary}"),
    );
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder
        .body(body::Body::from(payload))
        .expect("request builds")
}
