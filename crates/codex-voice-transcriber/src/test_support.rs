use axum::body;
use axum::http::{self, header};
use codex_voice_core::{
    SpeechClient, SpeechRequest, SpeechResult, SynthesizedSpeech, TranscriptionClient,
    TranscriptionResult,
};
use codex_voice_tts::config::{
    ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ElevenLabsVoiceSettings, GooglePersonaConfig,
    GoogleRuntimeConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig, SpeechPrepConfig,
    SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::server::{web::WebSpeechJobManager, ServiceAuth, ServiceState, TtsServiceState};

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

/// Fake backend for exercising bounded-concurrency transcription of chunked
/// uploads. Each call sleeps for the delay configured for its chunk index
/// (parsed from the `chunk-<index>.wav` filename produced by
/// `upload::filename_for_path`) and returns `"part-<index>"`, so tests can
/// assert both ordering (via the returned text) and overlap (via
/// `max_active`).
pub struct DelayedFakeBackend {
    delays: Vec<Duration>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl DelayedFakeBackend {
    pub fn new(delays: Vec<Duration>) -> Self {
        Self {
            delays,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    /// High-water mark of concurrently in-flight `transcribe` calls.
    pub fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

fn chunk_index_from_filename(filename: &str) -> usize {
    filename
        .strip_prefix("chunk-")
        .and_then(|rest| rest.strip_suffix(".wav"))
        .and_then(|index| index.parse().ok())
        .unwrap_or_else(|| panic!("test filename {filename:?} must look like chunk-<index>.wav"))
}

#[async_trait::async_trait]
impl TranscriptionClient for DelayedFakeBackend {
    async fn transcribe(
        &self,
        recording: &codex_voice_core::RecordedAudio,
    ) -> TranscriptionResult<String> {
        let index = chunk_index_from_filename(&recording.filename);
        let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now_active, Ordering::SeqCst);
        tokio::time::sleep(self.delays[index]).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(format!("part-{index}"))
    }
}

#[derive(Default)]
pub struct FakeSpeechBackend {
    pub seen: Mutex<Vec<SpeechRequest>>,
    pub prepared_input: Option<String>,
}

#[async_trait::async_trait]
impl SpeechClient for FakeSpeechBackend {
    async fn prepare(&self, request: &SpeechRequest) -> SpeechResult<String> {
        self.seen
            .lock()
            .expect("fake speech lock")
            .push(request.clone());
        Ok(self
            .prepared_input
            .clone()
            .unwrap_or_else(|| request.input.clone()))
    }

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

pub(crate) fn test_state_with_web_tts_config(codex_upload_limit_bytes: u64) -> ServiceState {
    let config = sample_tts_config();
    test_state_with_speech_and_config(codex_upload_limit_bytes, Some(config))
}

pub(crate) fn test_state_with_speech_and_config(
    codex_upload_limit_bytes: u64,
    tts_config: Option<ResolvedTtsConfig>,
) -> ServiceState {
    let speech = Some(Arc::new(FakeSpeechBackend::default()) as Arc<dyn SpeechClient>);
    test_state_with_speech_backend_and_config(codex_upload_limit_bytes, speech, tts_config)
}

pub(crate) fn test_state_with_speech_backend(
    codex_upload_limit_bytes: u64,
    speech: Option<Arc<dyn SpeechClient>>,
) -> ServiceState {
    test_state_with_speech_backend_and_config(codex_upload_limit_bytes, speech, None)
}

pub(crate) fn test_state_with_speech_backend_and_config(
    codex_upload_limit_bytes: u64,
    speech: Option<Arc<dyn SpeechClient>>,
    tts_config: Option<ResolvedTtsConfig>,
) -> ServiceState {
    ServiceState {
        backend: Arc::new(FakeBackend::default()),
        tts: Arc::new(std::sync::RwLock::new(TtsServiceState::from_parts(
            speech,
            tts_config.as_ref(),
        ))),
        web_speech_jobs: Arc::new(WebSpeechJobManager::new()),
        desktop_intents: Arc::new(Mutex::new(HashMap::new())),
        web_dist_override: None,
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

pub(crate) fn sample_tts_config() -> ResolvedTtsConfig {
    let mut personas = HashMap::new();
    personas.insert(
        "sky".to_string(),
        ResolvedPersona {
            label: "Sky".to_string(),
            description: "Warm test voice".to_string(),
            provider: ProviderKind::Google,
            provider_order: vec![ProviderKind::Google, ProviderKind::ElevenLabs],
            prompt_scene: Some("At home".to_string()),
            prompt_sample_context: Some("Gentle and clear".to_string()),
            prompt_style: Some("Warm".to_string()),
            prompt_pacing: Some("Relaxed".to_string()),
            prompt_constraints: vec!["Do not narrate tags.".to_string()],
            google: Some(GooglePersonaConfig {
                voice_name: "Sulafat".to_string(),
            }),
            elevenlabs: Some(ElevenLabsPersonaConfig {
                voice_id: "eleven-voice".to_string(),
                voice_settings: ElevenLabsVoiceSettings {
                    stability: 0.5,
                    similarity_boost: 0.75,
                    style: 0.25,
                    use_speaker_boost: true,
                    speed: 1.0,
                },
            }),
        },
    );

    ResolvedTtsConfig {
        default_provider: ProviderKind::Google,
        default_persona: Some("sky".to_string()),
        max_text_length: 4000,
        timeout: Duration::from_secs(30),
        speech_prep: Some(SpeechPrepConfig {
            provider: SpeechPrepProviderKind::Google,
            mode: SpeechPrepMode::PerformanceTags,
            api_key: Some("google-prep-key".to_string()),
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            model: "google/gemini-3.5-flash".to_string(),
            fallback_models: Vec::new(),
            auth_file: None,
            reasoning_effort: None,
            strategies: SpeechPrepStrategies::default(),
            tag_palette: vec![
                "tender".to_string(),
                "softly".to_string(),
                "amused".to_string(),
            ],
            cap_performance_tags: false,
            threshold: 120,
            max_input_length: 12000,
            max_length: 4000,
            attempt_timeout: Duration::from_secs(4),
            timeout: Duration::from_secs(20),
        }),
        google: Some(GoogleRuntimeConfig {
            api_key: "google-tts-key".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            voice: "Sulafat".to_string(),
            models: vec!["gemini-3.1-flash-tts-preview".to_string()],
            inline_audio_tags: None,
            max_text_length: 4000,
            timeout: Duration::from_secs(30),
        }),
        elevenlabs: Some(ElevenLabsRuntimeConfig {
            api_key: "eleven-key".to_string(),
            base_url: "https://api.elevenlabs.io".to_string(),
            models: vec!["eleven_v3".to_string()],
            apply_text_normalization: "auto".to_string(),
            output_format: "mp3_44100_128".to_string(),
            stream_gain: 2.0,
            language_code: None,
            inline_audio_tags: None,
            max_text_length: 4000,
            max_text_length_overridden: true,
            timeout: Duration::from_secs(30),
        }),
        personas,
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
