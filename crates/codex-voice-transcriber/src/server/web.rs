use super::speech::web_speech_client;
use super::{ApiError, ServiceState};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest};
use codex_voice_tts::config::{
    ElevenLabsPersonaConfig, GooglePersonaConfig, ProviderKind, ResolvedPersona, ResolvedTtsConfig,
    SpeechPrepMode,
};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;

pub(crate) const WEB_SPEECH_JOB_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const WEB_SPEECH_MAX_TERMINAL_JOBS: usize = 16;
const WEB_SPEECH_MAX_TERMINAL_BYTES: usize = 128 * 1024 * 1024;
const WEB_SPEECH_ADMISSION_LIMIT: usize = 3;
const WEB_SPEECH_WORKER_LIMIT: usize = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserTtsConfig {
    version: u8,
    default_provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_persona: Option<String>,
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
    mode: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserGoogleConfig {
    #[serde(skip)]
    api_key: String,
    #[serde(skip)]
    base_url: String,
    voice: String,
    models: Vec<String>,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserElevenLabsConfig {
    api_key: String,
    base_url: String,
    models: Vec<String>,
    apply_text_normalization: String,
    stream_gain: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_code: Option<String>,
    timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserPersonaConfig {
    label: String,
    description: String,
    provider: String,
    provider_order: Vec<String>,
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
            version: 2,
            default_provider: provider_name(config.default_provider).to_string(),
            default_persona: config.default_persona.clone(),
            providers: BrowserProviders {
                google: config.google.as_ref().map(|google| BrowserGoogleConfig {
                    api_key: google.api_key.clone(),
                    base_url: google.base_url.clone(),
                    voice: google.voice.clone(),
                    models: google.models.clone(),
                    timeout_ms: duration_millis(google.timeout),
                }),
                elevenlabs: config
                    .elevenlabs
                    .as_ref()
                    .map(|elevenlabs| BrowserElevenLabsConfig {
                        api_key: elevenlabs.api_key.clone(),
                        base_url: elevenlabs.base_url.clone(),
                        models: elevenlabs.models.clone(),
                        apply_text_normalization: elevenlabs.apply_text_normalization.clone(),
                        stream_gain: elevenlabs.stream_gain,
                        language_code: elevenlabs.language_code.clone(),
                        timeout_ms: duration_millis(elevenlabs.timeout),
                    }),
            },
            speech_prep: config
                .speech_prep
                .as_ref()
                .map(|prep| BrowserSpeechPrepConfig {
                    mode: speech_prep_mode_name(prep.mode).to_string(),
                    model: prep.model.clone(),
                }),
            personas: config
                .personas
                .iter()
                .map(|(name, persona)| (name.clone(), browser_persona(persona)))
                .collect(),
        }
    }
}

fn browser_persona(persona: &ResolvedPersona) -> BrowserPersonaConfig {
    BrowserPersonaConfig {
        label: persona.label.clone(),
        description: persona.description.clone(),
        provider: provider_name(persona.provider).to_string(),
        provider_order: persona
            .provider_order
            .iter()
            .map(|provider| provider_name(*provider).to_string())
            .collect(),
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

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn provider_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Google => "google",
        ProviderKind::ElevenLabs => "elevenlabs",
    }
}

fn speech_prep_mode_name(mode: SpeechPrepMode) -> &'static str {
    match mode {
        SpeechPrepMode::Shorten => "shorten",
        SpeechPrepMode::PerformanceTags => "performance-tags",
    }
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
    };

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
pub(crate) struct BrowserGoogleStreamRequest {
    input: String,
    model: String,
    voice: String,
}

pub(crate) async fn web_google_stream(
    State(state): State<ServiceState>,
    Json(body): Json<BrowserGoogleStreamRequest>,
) -> Result<Response, ApiError> {
    if body.input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }
    let google = {
        let tts = state
            .tts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tts.web_tts_config
            .as_ref()
            .and_then(|config| config.providers.google.clone())
            .ok_or_else(|| ApiError::service_unavailable("Google TTS is not configured"))?
    };
    if !google.models.iter().any(|model| model == &body.model)
        || !body.model.to_ascii_lowercase().starts_with("gemini-3.1-")
    {
        return Err(ApiError::bad_request(
            "selected Google model does not support direct streaming",
        ));
    }
    let upstream = google_stream_client()
        .post(format!(
            "{}/interactions",
            google
                .base_url
                .trim_end_matches('/')
                .trim_end_matches("/models")
        ))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("Api-Revision", "2026-05-20")
        .header("x-goog-api-key", &google.api_key)
        .json(&json!({
            "model": body.model,
            "input": google_stream_prompt(&body.input),
            "response_format": { "type": "audio" },
            "generation_config": {
                "speech_config": [{ "voice": body.voice }]
            },
            "stream": true
        }))
        .send()
        .await
        .map_err(|error| {
            ApiError::service_unavailable(format!("Google streaming request failed: {error}"))
        })?;
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|error| {
            ApiError::service_unavailable(format!("Google streaming response failed: {error}"))
        })
}

fn google_stream_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

fn google_stream_prompt(input: &str) -> String {
    format!(
        "Read the following text aloud exactly as written.\n\
         Do not add narration or commentary. Do not paraphrase.\n\n\
         Text:\n\"\"\"{input}\"\"\""
    )
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
    speech_prep_shorten_enabled: Option<bool>,
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
        speech_prep_shorten_enabled: body.speech_prep_shorten_enabled,
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
