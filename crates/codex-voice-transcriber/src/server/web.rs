use super::speech::web_speech_client;
use super::{ApiError, ServiceState};

use axum::{
    extract::{FromRequest, Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine;
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest};
use codex_voice_tts::config::{
    ElevenLabsPersonaConfig, GooglePersonaConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig,
    SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies, SpeechPrepStrategy,
};
use codex_voice_tts::{
    read_codex_auth_snapshot, sync_codex_auth_snapshot, CodexAuthSnapshot, CodexAuthSyncResult,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL,
};
use serde::{Deserialize, Serialize, Serializer};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;

pub(crate) const WEB_SPEECH_JOB_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const WEB_SPEECH_MAX_TERMINAL_JOBS: usize = 16;
const WEB_SPEECH_MAX_TERMINAL_BYTES: usize = 128 * 1024 * 1024;
const WEB_SPEECH_ADMISSION_LIMIT: usize = 3;
const WEB_SPEECH_WORKER_LIMIT: usize = 1;
const BROWSER_CODEX_BASE_URL: &str = "/_codex";

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
    #[serde(skip)]
    codex_auth_file: Option<PathBuf>,
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
    codex_auth: Option<BrowserCodexAuth>,
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
struct BrowserCodexAuth {
    access_token: String,
    refresh_token: String,
    account_id: String,
    token_url: String,
    client_id: String,
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElevenLabsConfig {
    api_key: String,
    base_url: String,
    model_id: String,
    fallback_models: Vec<String>,
    streaming: BrowserElevenLabsStreamingConfig,
    apply_text_normalization: String,
    output_format: String,
    stream_gain: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_audio_tags: Option<bool>,
    max_text_length: usize,
    max_text_length_overridden: bool,
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
    provider_order: Vec<String>,
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
                    model: google.models[0].clone(),
                    fallback_models: google.models[1..].to_vec(),
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
                }),
                elevenlabs: config
                    .elevenlabs
                    .as_ref()
                    .map(|elevenlabs| BrowserElevenLabsConfig {
                        api_key: elevenlabs.api_key.clone(),
                        base_url: elevenlabs.base_url.clone(),
                        model_id: elevenlabs.models[0].clone(),
                        fallback_models: elevenlabs.models[1..].to_vec(),
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
                        max_text_length_overridden: elevenlabs.max_text_length_overridden,
                        timeout_ms: duration_millis(elevenlabs.timeout),
                    }),
            },
            speech_prep: config.speech_prep.as_ref().map(|prep| {
                let codex_auth = (prep.provider == SpeechPrepProviderKind::Codex)
                    .then_some(prep.auth_file.as_deref())
                    .flatten()
                    .and_then(|path| read_codex_auth_snapshot(path).ok())
                    .map(|auth| BrowserCodexAuth {
                        access_token: auth.access_token,
                        refresh_token: auth.refresh_token,
                        account_id: auth.account_id,
                        token_url: CODEX_OAUTH_TOKEN_URL.to_string(),
                        client_id: CODEX_OAUTH_CLIENT_ID.to_string(),
                    });
                BrowserSpeechPrepConfig {
                    provider: speech_prep_provider_name(prep.provider).to_string(),
                    mode: speech_prep_mode_name(prep.mode).to_string(),
                    strategies: browser_speech_prep_strategies(prep.strategies),
                    tag_palette: prep.tag_palette.clone(),
                    cap_performance_tags: prep.cap_performance_tags,
                    browser_supported: prep.provider == SpeechPrepProviderKind::Google
                        || codex_auth.is_some(),
                    codex_auth,
                    browser_fallback: browser_speech_prep_fallback(prep, config),
                    api_key: prep.api_key.clone(),
                    base_url: if prep.provider == SpeechPrepProviderKind::Codex {
                        BROWSER_CODEX_BASE_URL.to_string()
                    } else {
                        prep.base_url.clone()
                    },
                    model: prep.model.clone(),
                    fallback_models: prep.fallback_models.clone(),
                    reasoning_effort: prep.reasoning_effort.clone(),
                    threshold: prep.threshold,
                    max_input_length: prep.max_input_length,
                    max_length: prep.max_length,
                    attempt_timeout_ms: duration_millis(prep.attempt_timeout),
                    timeout_ms: duration_millis(prep.timeout),
                }
            }),
            personas: config
                .personas
                .iter()
                .map(|(name, persona)| (name.clone(), browser_persona(persona)))
                .collect(),
            codex_auth_file: config.speech_prep.as_ref().and_then(|prep| {
                (prep.provider == SpeechPrepProviderKind::Codex)
                    .then(|| prep.auth_file.clone())
                    .flatten()
            }),
        }
    }

    pub(crate) async fn refresh_codex_auth(mut self) -> Self {
        let Some(auth_file) = self.codex_auth_file.clone() else {
            return self;
        };
        let auth = tokio::task::spawn_blocking(move || read_codex_auth_snapshot(&auth_file))
            .await
            .ok()
            .and_then(Result::ok)
            .map(|auth| BrowserCodexAuth {
                access_token: auth.access_token,
                refresh_token: auth.refresh_token,
                account_id: auth.account_id,
                token_url: CODEX_OAUTH_TOKEN_URL.to_string(),
                client_id: CODEX_OAUTH_CLIENT_ID.to_string(),
            });
        if let Some(prep) = self.speech_prep.as_mut() {
            prep.browser_supported = auth.is_some();
            prep.codex_auth = auth;
        }
        self
    }

    fn codex_auth_file(&self) -> Option<PathBuf> {
        self.codex_auth_file.clone()
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
        fallback_policy: if persona.provider_order.len() > 1 {
            "preserve-persona"
        } else {
            "strict"
        }
        .to_string(),
        provider_order: persona
            .provider_order
            .iter()
            .map(|provider| provider_name(*provider).to_string())
            .collect(),
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

pub(crate) type WebSpeechJobStore = Arc<WebSpeechJobManager>;

pub(crate) struct WebSpeechJobManager {
    records: Mutex<HashMap<String, WebSpeechJobRecord>>,
    admission: Arc<Semaphore>,
    workers: Arc<Semaphore>,
}

impl WebSpeechJobManager {
    pub(crate) fn new() -> Self {
        Self {
            records: Mutex::new(HashMap::new()),
            admission: Arc::new(Semaphore::new(WEB_SPEECH_ADMISSION_LIMIT)),
            workers: Arc::new(Semaphore::new(WEB_SPEECH_WORKER_LIMIT)),
        }
    }
}

#[derive(Clone)]
pub(crate) struct WebSpeechJobRecord {
    pub(crate) state: WebSpeechJobState,
    pub(crate) updated_at: Instant,
    pub(crate) abort: Option<AbortHandle>,
}

impl WebSpeechJobRecord {
    pub(crate) fn new(state: WebSpeechJobState) -> Self {
        Self {
            state,
            updated_at: Instant::now(),
            abort: None,
        }
    }
}

#[derive(Clone)]
pub(crate) enum WebSpeechJobState {
    Pending { phase: &'static str },
    Complete(Arc<WebSpeechResponse>),
    Failed(WebSpeechJobError),
}

pub(crate) fn prune_web_speech_jobs(jobs: &mut HashMap<String, WebSpeechJobRecord>) {
    prune_web_speech_jobs_at(Instant::now(), jobs);
}

fn enforce_web_speech_terminal_budget(jobs: &mut HashMap<String, WebSpeechJobRecord>) {
    loop {
        let terminal_count = jobs
            .values()
            .filter(|record| !matches!(record.state, WebSpeechJobState::Pending { .. }))
            .count();
        let terminal_bytes = jobs
            .values()
            .map(|record| match &record.state {
                WebSpeechJobState::Complete(result) => result.audio_base64.len(),
                _ => 0,
            })
            .sum::<usize>();
        if terminal_count <= WEB_SPEECH_MAX_TERMINAL_JOBS
            && terminal_bytes <= WEB_SPEECH_MAX_TERMINAL_BYTES
        {
            break;
        }
        let Some(oldest) = jobs
            .iter()
            .filter(|(_, record)| !matches!(record.state, WebSpeechJobState::Pending { .. }))
            .min_by_key(|(_, record)| record.updated_at)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        jobs.remove(&oldest);
    }
}

pub(crate) fn prune_web_speech_jobs_at(
    now: Instant,
    jobs: &mut HashMap<String, WebSpeechJobRecord>,
) {
    jobs.retain(|_, record| now.saturating_duration_since(record.updated_at) <= WEB_SPEECH_JOB_TTL);
}

pub(crate) async fn web_config(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    authorize_web_config_origin(&headers)?;
    let config = {
        let tts = state
            .tts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tts.web_tts_config
            .as_ref()
            .cloned()
            .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))?
    }
    .refresh_codex_auth()
    .await;

    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(config),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserCodexAuthSyncRequest {
    access_token: String,
    refresh_token: String,
    account_id: String,
}

pub(crate) async fn web_codex_auth_sync(
    State(state): State<ServiceState>,
    request: Request,
) -> Result<StatusCode, ApiError> {
    authorize_web_config_origin(request.headers())?;
    let Json(request) = Json::<BrowserCodexAuthSyncRequest>::from_request(request, &state)
        .await
        .map_err(ApiError::json_rejection)?;
    if request.access_token.trim().is_empty()
        || request.refresh_token.trim().is_empty()
        || request.account_id.trim().is_empty()
    {
        return Err(ApiError::bad_request(
            "Codex auth synchronization requires a complete credential bundle",
        ));
    }
    let auth_file = {
        let tts = state
            .tts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tts.web_tts_config
            .as_ref()
            .and_then(BrowserTtsConfig::codex_auth_file)
            .ok_or_else(|| ApiError::service_unavailable("Codex browser auth is not configured"))?
    };
    let incoming = CodexAuthSnapshot {
        access_token: request.access_token,
        refresh_token: request.refresh_token,
        account_id: request.account_id,
    };
    let result =
        tokio::task::spawn_blocking(move || sync_codex_auth_snapshot(&auth_file, &incoming))
            .await
            .map_err(|error| ApiError::internal(format!("Codex auth sync task failed: {error}")))?
            .map_err(|_| ApiError::service_unavailable("Codex auth synchronization failed"))?;
    match result {
        CodexAuthSyncResult::Updated | CodexAuthSyncResult::Unchanged => Ok(StatusCode::NO_CONTENT),
        CodexAuthSyncResult::RejectedOlder => Err(ApiError::conflict(
            "Browser Codex auth is older than the configured credentials",
        )),
        CodexAuthSyncResult::RejectedAccount => Err(ApiError::conflict(
            "Browser Codex auth belongs to a different account",
        )),
        CodexAuthSyncResult::RejectedInvalid => Err(ApiError::bad_request(
            "Browser Codex auth access token is invalid",
        )),
    }
}

fn authorize_web_config_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin
        .to_str()
        .map_err(|_| ApiError::forbidden("web config origin is not allowed"))?;
    let allowed = matches!(
        origin,
        "https://voice.heliasar.com" | "http://localhost:5173" | "http://127.0.0.1:5173"
    );
    if allowed {
        Ok(())
    } else {
        Err(ApiError::forbidden("web config origin is not allowed"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebSpeechRequest {
    input: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    speech_prep_enabled: Option<bool>,
    #[serde(default)]
    speech_prep_model: Option<String>,
    #[serde(default)]
    speech_prep_reasoning_effort: Option<String>,
    #[serde(default)]
    speech_prep_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WebSpeechResponse {
    pub(crate) input: String,
    pub(crate) input_changed: bool,
    pub(crate) audio_base64: String,
    pub(crate) mime_type: String,
    pub(crate) format: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WebSpeechPrepResponse {
    pub(crate) input: String,
    pub(crate) input_changed: bool,
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
    phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<SharedWebSpeechResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<WebSpeechJobError>,
}

#[derive(Debug, Clone)]
struct SharedWebSpeechResponse(Arc<WebSpeechResponse>);

impl Serialize for SharedWebSpeechResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
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
    let _worker = state
        .web_speech_jobs
        .workers
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("TTS service is busy; try again shortly"))?;
    synthesize_web_speech(speech_client, body).await.map(Json)
}

pub(crate) async fn web_speech_prep(
    State(state): State<ServiceState>,
    Json(body): Json<WebSpeechRequest>,
) -> Result<Json<WebSpeechPrepResponse>, ApiError> {
    let speech_client = web_speech_client(&state)?;
    let _worker = state
        .web_speech_jobs
        .workers
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("TTS service is busy; try again shortly"))?;
    let request = web_speech_request(body)?;
    let original = request.input.clone();
    let input = speech_client
        .prepare(&request)
        .await
        .map_err(ApiError::from_speech_error)?;
    Ok(Json(WebSpeechPrepResponse {
        input_changed: input != original,
        input,
    }))
}

pub(crate) async fn web_speech_job_create(
    State(state): State<ServiceState>,
    Json(body): Json<WebSpeechRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let speech_client = web_speech_client(&state)?;
    if body.input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }

    let admission = state
        .web_speech_jobs
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::too_many_requests("TTS queue is full; try again shortly"))?;

    let id = web_speech_job_id();
    let mut jobs = state
        .web_speech_jobs
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    prune_web_speech_jobs(&mut jobs);
    jobs.insert(
        id.clone(),
        WebSpeechJobRecord::new(WebSpeechJobState::Pending { phase: "queued" }),
    );
    drop(jobs);

    let jobs = state.web_speech_jobs.clone();
    let job_id = id.clone();
    let task = tokio::spawn(async move {
        let worker = match jobs.workers.clone().acquire_owned().await {
            Ok(worker) => worker,
            Err(_) => return,
        };
        {
            let mut records = jobs
                .records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(record) = records.get_mut(&job_id) else {
                return;
            };
            record.state = WebSpeechJobState::Pending { phase: "running" };
            record.updated_at = Instant::now();
        }
        let result = synthesize_web_speech(speech_client, body).await;
        let next_state = match result {
            Ok(response) => WebSpeechJobState::Complete(Arc::new(response)),
            Err(error) => WebSpeechJobState::Failed(WebSpeechJobError::from(error)),
        };
        let mut records = jobs
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = records.get_mut(&job_id) {
            record.state = next_state;
            record.updated_at = Instant::now();
            record.abort = None;
            enforce_web_speech_terminal_budget(&mut records);
        }
        drop(worker);
        drop(admission);
    });
    let abort = task.abort_handle();
    drop(task);
    if let Some(record) = state
        .web_speech_jobs
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(&id)
    {
        record.abort = Some(abort);
    }

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
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        prune_web_speech_jobs(&mut jobs);
        jobs.get(&id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("speech job was not found"))?
            .state
    };

    let response = match job {
        WebSpeechJobState::Pending { phase } => WebSpeechJobStatusResponse {
            id,
            status: "pending",
            phase: Some(phase),
            result: None,
            error: None,
        },
        WebSpeechJobState::Complete(result) => WebSpeechJobStatusResponse {
            id,
            status: "complete",
            phase: None,
            result: Some(SharedWebSpeechResponse(result)),
            error: None,
        },
        WebSpeechJobState::Failed(error) => WebSpeechJobStatusResponse {
            id,
            status: "failed",
            phase: None,
            result: None,
            error: Some(error),
        },
    };

    Ok(Json(response))
}

pub(crate) async fn web_speech_job_delete(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> StatusCode {
    let record = state
        .web_speech_jobs
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id);
    if let Some(abort) = record.and_then(|record| record.abort) {
        abort.abort();
    }
    StatusCode::NO_CONTENT
}

async fn synthesize_web_speech(
    speech_client: Arc<dyn SpeechClient>,
    body: WebSpeechRequest,
) -> Result<WebSpeechResponse, ApiError> {
    let request = web_speech_request(body)?;

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

fn web_speech_request(body: WebSpeechRequest) -> Result<SpeechRequest, ApiError> {
    if body.input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }
    Ok(SpeechRequest {
        input: body.input,
        provider_hint: body.provider,
        model_hint: body.model.unwrap_or_else(|| "gpt-4o-mini-tts".to_string()),
        voice_hint: body.voice,
        speech_prep_enabled: body.speech_prep_enabled,
        speech_prep_model_hint: body.speech_prep_model,
        speech_prep_reasoning_effort: body.speech_prep_reasoning_effort,
        speech_prep_timeout_ms: body.speech_prep_timeout_ms,
        instructions: None,
        format: SpeechFormat::Wav,
        speed: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_job_manager_enforces_admission_and_worker_limits() {
        let manager = WebSpeechJobManager::new();
        let _admitted: Vec<_> = (0..WEB_SPEECH_ADMISSION_LIMIT)
            .map(|_| manager.admission.clone().try_acquire_owned().unwrap())
            .collect();
        assert!(manager.admission.clone().try_acquire_owned().is_err());

        let _worker = manager.workers.clone().try_acquire_owned().unwrap();
        assert!(manager.workers.clone().try_acquire_owned().is_err());
    }
}
