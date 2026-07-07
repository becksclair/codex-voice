use super::speech::web_speech_client;
use super::{ApiError, ServiceState};

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine;
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest};
use codex_voice_tts::config::{
    ElevenLabsPersonaConfig, FallbackPolicy, GooglePersonaConfig, ProviderKind, ResolvedPersona,
    ResolvedTtsConfig, SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies,
    SpeechPrepStrategy,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const WEB_SPEECH_JOB_TTL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserTtsConfig {
    version: u8,
    default_provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_persona: Option<String>,
    max_text_length: usize,
    providers: BrowserProviders,
    #[serde(skip_serializing_if = "Option::is_none")]
    speech_prep: Option<BrowserSpeechPrepConfig>,
    personas: HashMap<String, BrowserPersonaConfig>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserProviders {
    #[serde(skip_serializing_if = "Option::is_none")]
    google: Option<BrowserGoogleConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elevenlabs: Option<BrowserElevenLabsConfig>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSpeechPrepConfig {
    provider: String,
    mode: String,
    strategies: BrowserSpeechPrepStrategies,
    tag_palette: Vec<String>,
    cap_performance_tags: bool,
    browser_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    browser_fallback: Option<BrowserSpeechPrepFallbackConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    base_url: String,
    model: String,
    fallback_models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    threshold: usize,
    max_input_length: usize,
    max_length: usize,
    attempt_timeout_ms: u64,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSpeechPrepFallbackConfig {
    provider: String,
    api_key: String,
    base_url: String,
    model: String,
    fallback_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserSpeechPrepStrategies {
    google: String,
    elevenlabs: String,
    default: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGoogleConfig {
    api_key: String,
    base_url: String,
    voice: String,
    model: String,
    fallback_models: Vec<String>,
    streaming: BrowserGoogleStreamingConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_audio_tags: Option<bool>,
    max_text_length: usize,
    timeout_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    scene: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pace: Option<String>,
    constraints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElevenLabsConfig {
    api_key: String,
    base_url: String,
    model_id: String,
    streaming: BrowserElevenLabsStreamingConfig,
    apply_text_normalization: String,
    output_format: String,
    stream_gain: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_audio_tags: Option<bool>,
    max_text_length: usize,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGoogleStreamingConfig {
    transport: String,
    supported_models: Vec<String>,
    output_format: String,
    sample_rate: u32,
    channels: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElevenLabsStreamingConfig {
    transport: String,
    preferred_model: String,
    output_format: String,
    sample_rate: u32,
    channels: u16,
    chunk_length_schedule: Vec<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserPersonaConfig {
    label: String,
    description: String,
    provider: String,
    fallback_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_scene: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_sample_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_pacing: Option<String>,
    prompt_constraints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    google: Option<BrowserGooglePersonaConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elevenlabs: Option<BrowserElevenLabsPersonaConfig>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGooglePersonaConfig {
    voice_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElevenLabsPersonaConfig {
    voice_id: String,
    voice_settings: BrowserElevenLabsVoiceSettings,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElevenLabsVoiceSettings {
    stability: f64,
    similarity_boost: f64,
    style: f64,
    use_speaker_boost: bool,
    speed: f64,
}

impl BrowserTtsConfig {
    pub(crate) fn from_resolved(config: &ResolvedTtsConfig) -> Self {
        Self {
            version: 1,
            default_provider: provider_name(config.default_provider).to_string(),
            default_persona: config.default_persona.clone(),
            max_text_length: config.max_text_length,
            providers: BrowserProviders {
                google: config.google.as_ref().map(|google| BrowserGoogleConfig {
                    api_key: google.api_key.clone(),
                    base_url: google.base_url.clone(),
                    voice: google.voice.clone(),
                    model: google.model.clone(),
                    fallback_models: google.fallback_models.clone(),
                    streaming: BrowserGoogleStreamingConfig {
                        transport: "interactions-stream".to_string(),
                        supported_models: vec!["gemini-3.1-flash-tts-preview".to_string()],
                        output_format: "pcm_24000".to_string(),
                        sample_rate: 24_000,
                        channels: 1,
                    },
                    inline_audio_tags: google.inline_audio_tags,
                    max_text_length: google.max_text_length,
                    timeout_ms: duration_millis(google.timeout),
                    scene: google.scene.clone(),
                    sample_context: google.sample_context.clone(),
                    style: google.style.clone(),
                    pace: google.pace.clone(),
                    constraints: google.constraints.clone(),
                }),
                elevenlabs: config
                    .elevenlabs
                    .as_ref()
                    .map(|elevenlabs| BrowserElevenLabsConfig {
                        api_key: elevenlabs.api_key.clone(),
                        base_url: elevenlabs.base_url.clone(),
                        model_id: elevenlabs.model_id.clone(),
                        streaming: BrowserElevenLabsStreamingConfig {
                            transport: "websocket".to_string(),
                            preferred_model: "eleven_flash_v2_5".to_string(),
                            output_format: "pcm_24000".to_string(),
                            sample_rate: 24_000,
                            channels: 1,
                            chunk_length_schedule: vec![120, 160, 250, 290],
                        },
                        apply_text_normalization: elevenlabs.apply_text_normalization.clone(),
                        output_format: elevenlabs.output_format.clone(),
                        stream_gain: elevenlabs.stream_gain,
                        language_code: elevenlabs.language_code.clone(),
                        inline_audio_tags: elevenlabs.inline_audio_tags,
                        max_text_length: elevenlabs.max_text_length,
                        timeout_ms: duration_millis(elevenlabs.timeout),
                    }),
            },
            speech_prep: config
                .speech_prep
                .as_ref()
                .map(|prep| BrowserSpeechPrepConfig {
                    provider: speech_prep_provider_name(prep.provider).to_string(),
                    mode: speech_prep_mode_name(prep.mode).to_string(),
                    strategies: browser_speech_prep_strategies(prep.strategies),
                    tag_palette: prep.tag_palette.clone(),
                    cap_performance_tags: prep.cap_performance_tags,
                    browser_supported: prep.provider == SpeechPrepProviderKind::Google,
                    browser_fallback: browser_speech_prep_fallback(prep, config),
                    api_key: prep.api_key.clone(),
                    base_url: prep.base_url.clone(),
                    model: prep.model.clone(),
                    fallback_models: prep.fallback_models.clone(),
                    reasoning_effort: prep.reasoning_effort.clone(),
                    threshold: prep.threshold,
                    max_input_length: prep.max_input_length,
                    max_length: prep.max_length,
                    attempt_timeout_ms: duration_millis(prep.attempt_timeout),
                    timeout_ms: duration_millis(prep.timeout),
                }),
            personas: config
                .personas
                .iter()
                .map(|(name, persona)| (name.clone(), browser_persona(persona)))
                .collect(),
        }
    }
}

fn browser_speech_prep_fallback(
    prep: &codex_voice_tts::config::SpeechPrepConfig,
    config: &ResolvedTtsConfig,
) -> Option<BrowserSpeechPrepFallbackConfig> {
    if prep.provider != SpeechPrepProviderKind::Codex {
        return None;
    }
    let google = config.google.as_ref()?;
    Some(BrowserSpeechPrepFallbackConfig {
        provider: "google".to_string(),
        api_key: google.api_key.clone(),
        base_url: google.base_url.clone(),
        model: "google/gemini-3.5-flash".to_string(),
        fallback_models: Vec::new(),
    })
}

fn browser_speech_prep_strategies(strategies: SpeechPrepStrategies) -> BrowserSpeechPrepStrategies {
    BrowserSpeechPrepStrategies {
        google: speech_prep_strategy_name(strategies.google).to_string(),
        elevenlabs: speech_prep_strategy_name(strategies.elevenlabs).to_string(),
        default: speech_prep_strategy_name(strategies.default).to_string(),
    }
}

fn browser_persona(persona: &ResolvedPersona) -> BrowserPersonaConfig {
    BrowserPersonaConfig {
        label: persona.label.clone(),
        description: persona.description.clone(),
        provider: provider_name(persona.provider).to_string(),
        fallback_policy: fallback_policy_name(persona.fallback_policy).to_string(),
        prompt_scene: persona.prompt_scene.clone(),
        prompt_sample_context: persona.prompt_sample_context.clone(),
        prompt_style: persona.prompt_style.clone(),
        prompt_pacing: persona.prompt_pacing.clone(),
        prompt_constraints: persona.prompt_constraints.clone(),
        google: persona.google.as_ref().map(browser_google_persona),
        elevenlabs: persona.elevenlabs.as_ref().map(browser_elevenlabs_persona),
    }
}

fn browser_google_persona(google: &GooglePersonaConfig) -> BrowserGooglePersonaConfig {
    BrowserGooglePersonaConfig {
        voice_name: google.voice_name.clone(),
    }
}

fn browser_elevenlabs_persona(
    elevenlabs: &ElevenLabsPersonaConfig,
) -> BrowserElevenLabsPersonaConfig {
    BrowserElevenLabsPersonaConfig {
        voice_id: elevenlabs.voice_id.clone(),
        voice_settings: BrowserElevenLabsVoiceSettings {
            stability: elevenlabs.voice_settings.stability,
            similarity_boost: elevenlabs.voice_settings.similarity_boost,
            style: elevenlabs.voice_settings.style,
            use_speaker_boost: elevenlabs.voice_settings.use_speaker_boost,
            speed: elevenlabs.voice_settings.speed,
        },
    }
}

fn provider_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Google => "google",
        ProviderKind::ElevenLabs => "elevenlabs",
    }
}

fn speech_prep_provider_name(provider: SpeechPrepProviderKind) -> &'static str {
    match provider {
        SpeechPrepProviderKind::Google => "google",
        SpeechPrepProviderKind::Codex => "codex",
    }
}

fn fallback_policy_name(policy: FallbackPolicy) -> &'static str {
    match policy {
        FallbackPolicy::PreservePersona => "preserve-persona",
        FallbackPolicy::Strict => "strict",
    }
}

fn speech_prep_mode_name(mode: SpeechPrepMode) -> &'static str {
    match mode {
        SpeechPrepMode::Shorten => "shorten",
        SpeechPrepMode::PerformanceTags => "performance-tags",
    }
}

fn speech_prep_strategy_name(strategy: SpeechPrepStrategy) -> &'static str {
    strategy.as_name()
}

fn duration_millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) type WebSpeechJobStore = Arc<Mutex<HashMap<String, WebSpeechJobRecord>>>;

#[derive(Clone)]
pub(crate) struct WebSpeechJobRecord {
    pub(crate) state: WebSpeechJobState,
    pub(crate) updated_at: Instant,
}

impl WebSpeechJobRecord {
    pub(crate) fn new(state: WebSpeechJobState) -> Self {
        Self {
            state,
            updated_at: Instant::now(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum WebSpeechJobState {
    Pending,
    Complete(WebSpeechResponse),
    Failed(WebSpeechJobError),
}

pub(crate) fn prune_web_speech_jobs(jobs: &mut HashMap<String, WebSpeechJobRecord>) {
    prune_web_speech_jobs_at(Instant::now(), jobs);
}

pub(crate) fn prune_web_speech_jobs_at(
    now: Instant,
    jobs: &mut HashMap<String, WebSpeechJobRecord>,
) {
    jobs.retain(|_, record| now.saturating_duration_since(record.updated_at) <= WEB_SPEECH_JOB_TTL);
}

pub(crate) async fn web_config(
    State(state): State<ServiceState>,
) -> Result<impl IntoResponse, ApiError> {
    let config = state
        .tts
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .web_tts_config
        .as_ref()
        .cloned()
        .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))?;

    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(config),
    ))
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebSpeechRequest {
    input: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WebSpeechResponse {
    pub(crate) input: String,
    pub(crate) input_changed: bool,
    pub(crate) audio_base64: String,
    pub(crate) mime_type: String,
    pub(crate) format: String,
}

#[derive(Debug, Serialize)]
struct WebSpeechJobCreateResponse {
    id: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct WebSpeechJobStatusResponse {
    id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<WebSpeechResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<WebSpeechJobError>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WebSpeechJobError {
    status: u16,
    kind: &'static str,
    message: String,
}

pub(crate) async fn web_speech(
    State(state): State<ServiceState>,
    Json(body): Json<WebSpeechRequest>,
) -> Result<Json<WebSpeechResponse>, ApiError> {
    let speech_client = web_speech_client(&state)?;
    synthesize_web_speech(speech_client, body.input)
        .await
        .map(Json)
}

pub(crate) async fn web_speech_job_create(
    State(state): State<ServiceState>,
    Json(body): Json<WebSpeechRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let speech_client = web_speech_client(&state)?;
    let input = body.input;
    if input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }

    let id = web_speech_job_id();
    let mut jobs = state
        .web_speech_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    prune_web_speech_jobs(&mut jobs);
    jobs.insert(
        id.clone(),
        WebSpeechJobRecord::new(WebSpeechJobState::Pending),
    );
    drop(jobs);

    let jobs = state.web_speech_jobs.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let result = synthesize_web_speech(speech_client, input).await;
        let state = match result {
            Ok(response) => WebSpeechJobState::Complete(response),
            Err(error) => WebSpeechJobState::Failed(WebSpeechJobError::from(error)),
        };
        jobs.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(job_id, WebSpeechJobRecord::new(state));
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(WebSpeechJobCreateResponse {
            id,
            status: "pending",
        }),
    ))
}

pub(crate) async fn web_speech_job_status(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> Result<Json<WebSpeechJobStatusResponse>, ApiError> {
    let job = {
        let mut jobs = state
            .web_speech_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_web_speech_jobs(&mut jobs);
        jobs.get(&id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request("speech job was not found"))?
            .state
    };

    let response = match job {
        WebSpeechJobState::Pending => WebSpeechJobStatusResponse {
            id,
            status: "pending",
            result: None,
            error: None,
        },
        WebSpeechJobState::Complete(result) => WebSpeechJobStatusResponse {
            id,
            status: "complete",
            result: Some(result),
            error: None,
        },
        WebSpeechJobState::Failed(error) => WebSpeechJobStatusResponse {
            id,
            status: "failed",
            result: None,
            error: Some(error),
        },
    };

    Ok(Json(response))
}

async fn synthesize_web_speech(
    speech_client: Arc<dyn SpeechClient>,
    input: String,
) -> Result<WebSpeechResponse, ApiError> {
    if input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }

    let request = SpeechRequest {
        input,
        model_hint: "gpt-4o-mini-tts".to_string(),
        voice_hint: None,
        instructions: None,
        format: SpeechFormat::Wav,
        speed: None,
    };

    let original_input = request.input.clone();
    let synthesized = speech_client
        .synthesize(&request)
        .await
        .map_err(ApiError::from_speech_error)?;
    let input = synthesized
        .prepared_input
        .clone()
        .unwrap_or_else(|| original_input.clone());
    let input_changed = input != original_input;

    Ok(WebSpeechResponse {
        input,
        input_changed,
        audio_base64: base64::engine::general_purpose::STANDARD.encode(&synthesized.bytes),
        mime_type: synthesized.mime_type,
        format: synthesized.format.to_openai().to_string(),
    })
}

fn web_speech_job_id() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

impl From<ApiError> for WebSpeechJobError {
    fn from(error: ApiError) -> Self {
        Self {
            status: error.status.as_u16(),
            kind: error.kind,
            message: error.message,
        }
    }
}
