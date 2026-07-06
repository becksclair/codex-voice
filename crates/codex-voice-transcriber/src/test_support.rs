use axum::body;
use axum::http::{self, header};
use codex_voice_core::{
    SpeechClient, SpeechRequest, SpeechResult, SynthesizedSpeech, TranscriptionClient,
    TranscriptionResult,
};
use codex_voice_tts::config::{
    ElevenLabsPersonaConfig, ElevenLabsRuntimeConfig, ElevenLabsVoiceSettings, FallbackPolicy,
    GooglePersonaConfig, GoogleRuntimeConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig,
    SpeechPrepConfig, SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

pub(crate) fn test_state_with_web_tts_config(codex_upload_limit_bytes: u64) -> ServiceState {
    let mut state = test_state_with_speech(codex_upload_limit_bytes);
    state.web_tts_config = Some(crate::server::BrowserTtsConfig::from_resolved(
        &sample_tts_config(),
    ));
    state
}

pub(crate) fn test_state_with_speech_backend(
    codex_upload_limit_bytes: u64,
    speech: Option<Arc<dyn SpeechClient>>,
) -> ServiceState {
    ServiceState {
        backend: Arc::new(FakeBackend::default()),
        speech,
        web_tts_config: None,
        web_speech_jobs: Arc::new(Mutex::new(HashMap::new())),
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
            fallback_policy: FallbackPolicy::PreservePersona,
            prompt_profile: None,
            prompt_scene: Some("At home".to_string()),
            prompt_sample_context: Some("Gentle and clear".to_string()),
            prompt_style: Some("Warm".to_string()),
            prompt_accent: None,
            prompt_pacing: Some("Relaxed".to_string()),
            prompt_constraints: vec!["Do not narrate tags.".to_string()],
            google: Some(GooglePersonaConfig {
                voice_name: "Sulafat".to_string(),
                prompt_template: String::new(),
                persona_prompt: String::new(),
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
            model: "gemini-3.1-flash-tts-preview".to_string(),
            fallback_models: vec![],
            inline_audio_tags: None,
            max_text_length: 4000,
            timeout: Duration::from_secs(30),
            scene: Some("At home".to_string()),
            sample_context: Some("Gentle and clear".to_string()),
            style: Some("Warm".to_string()),
            pace: Some("Relaxed".to_string()),
            constraints: vec!["Do not narrate tags.".to_string()],
        }),
        elevenlabs: Some(ElevenLabsRuntimeConfig {
            api_key: "eleven-key".to_string(),
            base_url: "https://api.elevenlabs.io".to_string(),
            model_id: "eleven_v3".to_string(),
            apply_text_normalization: "auto".to_string(),
            output_format: "mp3_44100_128".to_string(),
            language_code: "en".to_string(),
            inline_audio_tags: None,
            max_text_length: 4000,
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
