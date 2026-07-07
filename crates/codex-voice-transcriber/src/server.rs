use anyhow::{Context, Result};
use axum::{
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use bytes::Bytes;
use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest, TranscriptionClient};
use codex_voice_tts::{
    config::{
        ElevenLabsPersonaConfig, FallbackPolicy, GooglePersonaConfig, ProviderKind,
        ResolvedPersona, ResolvedTtsConfig, SpeechPrepMode, SpeechPrepProviderKind,
        SpeechPrepStrategies, SpeechPrepStrategy,
    },
    ConfiguredSpeechClient, ReadAloudConfigLoader,
};

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant, SystemTime},
};
use tokio::net::TcpListener;

use super::chunking::{self, MAX_GENERATED_CHUNKS, PCM_BYTES_PER_SECOND};
use super::client;
use super::discovery::{
    discovery_path, remove_discovery_file_if_current, resolve_or_generate_token, service_root_url,
    write_discovery_file, ServiceCapabilities, TranscriberDiscoveryFile,
};
use super::upload::{self, Upload};

const SPEECH_BODY_LIMIT_BYTES: usize = 64 * 1024;
const MULTIPART_OVERHEAD_BYTES: u64 = 64 * 1024;
const WEB_ICON_192: &[u8] = include_bytes!("../assets/web/icon-192.png");
const WEB_ICON_512: &[u8] = include_bytes!("../assets/web/icon-512.png");
const WEB_ICON_MASKABLE_512: &[u8] = include_bytes!("../assets/web/icon-maskable-512.png");
const WEB_APPLE_TOUCH_ICON: &[u8] = include_bytes!("../assets/web/apple-touch-icon.png");
const WEB_BUILD_REVISION: &str = env!("CODEX_VOICE_WEB_REVISION");
const WEB_SPEECH_JOB_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const TTS_CONFIG_WATCH_INTERVAL: Duration = Duration::from_secs(2);
const TTS_CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
const WEB_SW_BODY_JS: &str = include_str!("../assets/web/sw.js");
const WEB_APP_HTML: &str = include_str!("../assets/web/app.html");

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

#[derive(Clone)]
pub(crate) struct ServiceState {
    pub(crate) backend: Arc<dyn TranscriptionClient>,
    pub(crate) tts: Arc<RwLock<TtsServiceState>>,
    pub(crate) web_speech_jobs: WebSpeechJobStore,
    pub(crate) auth: ServiceAuth,
    pub(crate) codex_upload_limit_bytes: u64,
    pub(crate) client_upload_limit_bytes: u64,
    pub(crate) chunk_seconds: u64,
    pub(crate) ffmpeg_binary: String,
}

#[derive(Clone)]
pub(crate) struct TtsServiceState {
    pub(crate) speech: Option<Arc<dyn SpeechClient>>,
    pub(crate) web_tts_config: Option<BrowserTtsConfig>,
}

impl TtsServiceState {
    pub(crate) fn from_parts(
        speech: Option<Arc<dyn SpeechClient>>,
        tts_config: Option<&ResolvedTtsConfig>,
    ) -> Self {
        Self {
            speech,
            web_tts_config: tts_config.map(BrowserTtsConfig::from_resolved),
        }
    }

    fn configured(speech: Arc<dyn SpeechClient>, tts_config: &ResolvedTtsConfig) -> Self {
        Self {
            speech: Some(speech),
            web_tts_config: Some(BrowserTtsConfig::from_resolved(tts_config)),
        }
    }
}

pub(crate) type WebSpeechJobStore = Arc<Mutex<HashMap<String, WebSpeechJobRecord>>>;

#[derive(Clone)]
pub(crate) struct WebSpeechJobRecord {
    state: WebSpeechJobState,
    updated_at: Instant,
}

impl WebSpeechJobRecord {
    fn new(state: WebSpeechJobState) -> Self {
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

fn prune_web_speech_jobs(jobs: &mut HashMap<String, WebSpeechJobRecord>) {
    let now = Instant::now();
    jobs.retain(|_, record| now.duration_since(record.updated_at) <= WEB_SPEECH_JOB_TTL);
}

#[derive(Clone)]
pub(crate) struct ServiceAuth {
    pub(crate) token: String,
    pub(crate) no_auth: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigFingerprint {
    modified: SystemTime,
    len: u64,
}

async fn watch_tts_config(tts: Arc<RwLock<TtsServiceState>>, path: PathBuf) {
    tracing::info!(path = %path.display(), "watching TTS config for live reload");
    let mut last_seen = None;

    loop {
        let current = config_fingerprint(&path).await;
        if current != last_seen {
            tokio::time::sleep(TTS_CONFIG_RELOAD_DEBOUNCE).await;
            let stable = config_fingerprint(&path).await;
            if stable == current {
                match stable {
                    Some(_) => match reload_tts_config_once(&tts, &path).await {
                        Ok(()) => tracing::info!(
                            path = %path.display(),
                            "TTS config reloaded successfully"
                        ),
                        Err(error) => tracing::warn!(
                            path = %path.display(),
                            %error,
                            "TTS config reload failed; keeping previous working config"
                        ),
                    },
                    None => tracing::warn!(
                        path = %path.display(),
                        "TTS config disappeared; keeping previous working config"
                    ),
                }
                last_seen = stable;
            }
        }

        tokio::time::sleep(TTS_CONFIG_WATCH_INTERVAL).await;
    }
}

async fn config_fingerprint(path: &FsPath) -> Option<ConfigFingerprint> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    Some(ConfigFingerprint {
        modified: metadata.modified().ok()?,
        len: metadata.len(),
    })
}

async fn reload_tts_config_once(tts: &Arc<RwLock<TtsServiceState>>, path: &FsPath) -> Result<()> {
    let path = path.to_path_buf();
    let (speech, config) = tokio::task::spawn_blocking(move || {
        let loader = ReadAloudConfigLoader::new(path);
        let config = loader.load().context("failed to load read-aloud config")?;
        let client = ConfiguredSpeechClient::try_new(config.clone())
            .context("failed to create TTS client from config")?;
        if !client.has_any_provider() {
            anyhow::bail!("TTS config parsed but no usable provider is configured");
        }
        Ok::<_, anyhow::Error>((Arc::new(client) as Arc<dyn SpeechClient>, config))
    })
    .await
    .context("TTS config reload task failed")??;

    *tts.write().expect("TTS state lock") = TtsServiceState::configured(speech, &config);
    Ok(())
}

pub async fn serve(
    config: super::ServeConfig,
    speech: Option<Arc<dyn SpeechClient>>,
    tts_config: Option<ResolvedTtsConfig>,
    tts_config_path: Option<PathBuf>,
) -> Result<()> {
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind audio service on {}", config.bind))?;
    let local_addr = listener.local_addr()?;
    let backend = Arc::new(CodexTranscriptionClient::with_timeout(
        CodexAuthService::new()?,
        super::DEFAULT_SERVICE_TIMEOUT,
    )?);
    let root_url = service_root_url(local_addr);
    let token = resolve_or_generate_token(&config.token_env);

    let capabilities = ServiceCapabilities {
        transcriptions: true,
        speech: speech.is_some(),
    };
    let discovery = TranscriberDiscoveryFile::new(root_url, token, capabilities.clone());
    write_discovery_file(&discovery)?;

    let tts = Arc::new(RwLock::new(TtsServiceState::from_parts(
        speech,
        tts_config.as_ref(),
    )));
    if let Some(path) = tts_config_path {
        tokio::spawn(watch_tts_config(tts.clone(), path));
    }

    let app = service_router(ServiceState {
        backend,
        tts,
        web_speech_jobs: Arc::new(Mutex::new(HashMap::new())),
        auth: ServiceAuth {
            token: discovery.token.clone(),
            no_auth: config.no_auth,
        },
        codex_upload_limit_bytes: config.codex_upload_limit_bytes,
        client_upload_limit_bytes: config.client_upload_limit_bytes,
        chunk_seconds: config.chunk_seconds,
        ffmpeg_binary: config.ffmpeg_binary,
    });

    println!("Codex Voice audio service listening on {}", discovery.url);
    println!("OpenAI-compatible base URL: {}", discovery.openai_base_url);
    println!(
        "Capabilities: transcriptions={} speech={}",
        capabilities.transcriptions, capabilities.speech
    );
    println!("Discovery file: {}", discovery_path().display());

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    remove_discovery_file_if_current(&discovery);
    result.context("audio service failed")
}

fn service_router(state: ServiceState) -> Router {
    let transcription_body_limit = usize::try_from(
        state
            .client_upload_limit_bytes
            .saturating_add(MULTIPART_OVERHEAD_BYTES),
    )
    .unwrap_or(usize::MAX);
    use tower_http::cors::{AllowOrigin, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([Method::POST, Method::GET])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let health_routes = get(health);
    let transcribe_routes = post(transcribe);
    let speech_routes = post(speech).layer(DefaultBodyLimit::max(SPEECH_BODY_LIMIT_BYTES));
    let web_speech_routes = post(web_speech).layer(DefaultBodyLimit::max(SPEECH_BODY_LIMIT_BYTES));
    let web_speech_job_routes =
        post(web_speech_job_create).layer(DefaultBodyLimit::max(SPEECH_BODY_LIMIT_BYTES));

    Router::new()
        .route("/healthz", health_routes.clone())
        .route("/v1/healthz", health_routes)
        .route("/web", get(web_app))
        .route("/web/config", get(web_config))
        .route("/web/manifest.webmanifest", get(web_manifest))
        .route("/web/manifest-light.webmanifest", get(web_manifest_light))
        .route("/web-sw.js", get(web_service_worker))
        .route("/web/icon-192.png", get(web_icon_192))
        .route("/web/icon-512.png", get(web_icon_512))
        .route("/web/icon-maskable-512.png", get(web_icon_maskable_512))
        .route("/web/apple-touch-icon.png", get(web_apple_touch_icon))
        .route("/web/speech", web_speech_routes)
        .route("/web/speech-jobs", web_speech_job_routes)
        .route("/web/speech-jobs/{id}", get(web_speech_job_status))
        .route("/audio/transcriptions", transcribe_routes.clone())
        .route("/v1/audio/transcriptions", transcribe_routes)
        .layer(DefaultBodyLimit::max(transcription_body_limit))
        .route("/audio/speech", speech_routes.clone())
        .route("/v1/audio/speech", speech_routes)
        .layer(cors)
        .with_state(state)
}

async fn health(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> Result<Json<Health>, ApiError> {
    authorize(&headers, &state.auth)?;
    let tts = state.tts.read().expect("TTS state lock");
    let capabilities = ServiceCapabilities {
        transcriptions: true,
        speech: tts.speech.is_some(),
    };
    Ok(Json(Health {
        ok: true,
        capabilities,
    }))
}

async fn transcribe(
    State(state): State<ServiceState>,
    request: Request,
) -> Result<Response, ApiError> {
    authorize(request.headers(), &state.auth)?;
    let multipart = Multipart::from_request(request, &state)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("length limit") || message.contains("Payload Too Large") {
                ApiError::payload_too_large(format!("request body exceeds size limit: {message}"))
            } else {
                ApiError::bad_request(format!("failed to read multipart form: {message}"))
            }
        })?;
    let upload = upload::read_upload(multipart, state.client_upload_limit_bytes).await?;
    let text = transcribe_upload(&state, &upload).await?;
    Ok(match upload.response_format {
        upload::ResponseFormat::Json => Json(TranscriptionResponse { text }).into_response(),
        upload::ResponseFormat::Text => {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
        }
    })
}

async fn web_app() -> Html<String> {
    Html(web_app_body())
}

fn web_app_body() -> String {
    WEB_APP_HTML
        .replace(
            "__WEB_MANIFEST_URL__",
            &versioned_web_asset("/web/manifest.webmanifest"),
        )
        .replace(
            "__WEB_MANIFEST_LIGHT_URL__",
            &versioned_web_asset("/web/manifest-light.webmanifest"),
        )
        .replace(
            "__WEB_ICON_192_URL__",
            &versioned_web_asset("/web/icon-192.png"),
        )
        .replace(
            "__WEB_ICON_512_URL__",
            &versioned_web_asset("/web/icon-512.png"),
        )
        .replace(
            "__WEB_APPLE_TOUCH_ICON_URL__",
            &versioned_web_asset("/web/apple-touch-icon.png"),
        )
}

async fn web_config(State(state): State<ServiceState>) -> Result<impl IntoResponse, ApiError> {
    let config = state
        .tts
        .read()
        .expect("TTS state lock")
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

fn web_build_version() -> String {
    format!("{}+{}", env!("CARGO_PKG_VERSION"), WEB_BUILD_REVISION)
}

fn web_cache_name() -> String {
    format!("codex-voice-web-{}", web_build_version())
}

fn versioned_web_asset(path: &str) -> String {
    format!("{path}?v={WEB_BUILD_REVISION}")
}

fn web_manifest_body(background_color: &str, theme_color: &str) -> String {
    serde_json::json!({
        "name": "Codex Voice",
        "short_name": "Voice",
        "description": "Quick text-to-speech for Codex Voice.",
        "id": "/web",
        "start_url": "/web",
        "scope": "/web",
        "display": "standalone",
        "background_color": background_color,
        "theme_color": theme_color,
        "version": web_build_version(),
        "build_revision": WEB_BUILD_REVISION,
        "icons": [
            {
                "src": versioned_web_asset("/web/icon-192.png"),
                "sizes": "192x192",
                "type": "image/png",
                "purpose": "any"
            },
            {
                "src": versioned_web_asset("/web/icon-512.png"),
                "sizes": "512x512",
                "type": "image/png",
                "purpose": "any"
            },
            {
                "src": versioned_web_asset("/web/icon-maskable-512.png"),
                "sizes": "512x512",
                "type": "image/png",
                "purpose": "maskable"
            }
        ]
    })
    .to_string()
}

fn web_service_worker_body() -> String {
    let cache_name = serde_json::to_string(&web_cache_name()).expect("cache name serializes");
    let build_revision =
        serde_json::to_string(WEB_BUILD_REVISION).expect("build revision serializes");
    format!("const CACHE_NAME = {cache_name};\nconst WEB_BUILD_REVISION = {build_revision};\n{WEB_SW_BODY_JS}")
}

async fn web_manifest() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        web_manifest_body("#17091f", "#17091f"),
    )
}

async fn web_manifest_light() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        web_manifest_body("#f3dff1", "#f3dff1"),
    )
}

async fn web_service_worker() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        web_service_worker_body(),
    )
}

fn web_png_response(bytes: &'static [u8]) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        Bytes::from_static(bytes),
    )
}

async fn web_icon_192() -> impl IntoResponse {
    web_png_response(WEB_ICON_192)
}

async fn web_icon_512() -> impl IntoResponse {
    web_png_response(WEB_ICON_512)
}

async fn web_icon_maskable_512() -> impl IntoResponse {
    web_png_response(WEB_ICON_MASKABLE_512)
}

async fn web_apple_touch_icon() -> impl IntoResponse {
    web_png_response(WEB_APPLE_TOUCH_ICON)
}

async fn transcribe_upload(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    if upload.bytes <= state.codex_upload_limit_bytes {
        return transcribe_direct(state, upload).await;
    }

    transcribe_chunked(state, upload).await
}

async fn transcribe_direct(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    client::transcribe_path(
        state.backend.as_ref(),
        upload.temp.path(),
        &upload.filename,
        &upload.content_type,
    )
    .await
    .map_err(|error| ApiError::backend(error.to_string()))
}

async fn transcribe_chunked(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    if !chunking::ffmpeg_available(&state.ffmpeg_binary).await {
        return Err(ApiError::payload_too_large(format!(
            "audio is {} bytes, above the Codex per-request limit of {} bytes; install ffmpeg or send smaller chunks",
            upload.bytes, state.codex_upload_limit_bytes
        )));
    }

    let chunk_seconds =
        chunking::effective_chunk_seconds(state.chunk_seconds, state.codex_upload_limit_bytes);
    let max_seconds_from_bytes = state.client_upload_limit_bytes / PCM_BYTES_PER_SECOND;
    let max_seconds_from_chunks = MAX_GENERATED_CHUNKS as u64 * chunk_seconds;
    let max_duration_seconds = max_seconds_from_bytes.min(max_seconds_from_chunks).max(1);

    match chunking::input_duration_seconds(
        &chunking::ffprobe_binary(&state.ffmpeg_binary),
        upload.temp.path(),
    )
    .await
    {
        Ok(Some(duration)) if duration > max_duration_seconds as f64 => {
            return Err(ApiError::payload_too_large(format!(
                "audio duration is {duration:.1}s, above the service limit of {max_duration_seconds}s; send smaller chunks"
            )));
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to probe audio duration, proceeding with chunk-count safety cap");
        }
    }

    let chunks = chunking::split_audio_with_ffmpeg(
        &state.ffmpeg_binary,
        upload.temp.path(),
        chunk_seconds,
        Some(max_duration_seconds),
    )
    .await
    .map_err(|error| ApiError::internal(format!("failed to split oversized audio: {error:#}")))?;
    chunking::validate_generated_chunks(
        &chunks.paths,
        state.client_upload_limit_bytes,
        state.codex_upload_limit_bytes,
    )
    .await
    .map_err(|error| match error {
        chunking::ChunkingError::TooManyChunks { count, limit } => ApiError::payload_too_large(
            format!(
                "audio produced {count} chunks, above the service limit of {limit}; send smaller chunks"
            ),
        ),
        chunking::ChunkingError::ChunkTooLarge { index, bytes, limit } => {
            ApiError::payload_too_large(format!(
                "generated chunk {index} is {bytes} bytes, above configured Codex limit of {limit} bytes"
            ))
        }
        chunking::ChunkingError::DecodedTooLarge { bytes, limit } => ApiError::payload_too_large(
            format!(
                "decoded audio is {bytes} bytes, above the service decoded-output limit of {limit} bytes; send smaller chunks"
            ),
        ),
        chunking::ChunkingError::Io { message } => ApiError::internal(message),
    })?;
    let mut transcripts = Vec::with_capacity(chunks.paths.len());
    for path in &chunks.paths {
        let filename = upload::filename_for_path(path);
        transcripts.push(
            client::transcribe_path(state.backend.as_ref(), path, &filename, "audio/wav")
                .await
                .map_err(|error| ApiError::backend(error.to_string()))?,
        );
    }
    Ok(upload::join_transcripts(&transcripts))
}

#[derive(Debug, Deserialize)]
struct OpenAiSpeechRequest {
    model: String,
    input: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(rename = "response_format", default)]
    response_format: Option<String>,
    #[serde(default)]
    speed: Option<f32>,
    #[serde(default)]
    rate: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct WebSpeechRequest {
    input: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WebSpeechResponse {
    input: String,
    input_changed: bool,
    audio_base64: String,
    mime_type: String,
    format: String,
}

#[derive(Debug, Serialize)]
struct WebSpeechJobCreateResponse {
    id: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct WebSpeechJobStatusResponse {
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

async fn web_speech(
    State(state): State<ServiceState>,
    Json(body): Json<WebSpeechRequest>,
) -> Result<Json<WebSpeechResponse>, ApiError> {
    let speech_client = web_speech_client(&state)?;
    synthesize_web_speech(speech_client, body.input)
        .await
        .map(Json)
}

async fn web_speech_job_create(
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
        .expect("web speech job store lock");
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
            .expect("web speech job store lock")
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

async fn web_speech_job_status(
    State(state): State<ServiceState>,
    Path(id): Path<String>,
) -> Result<Json<WebSpeechJobStatusResponse>, ApiError> {
    let job = {
        let mut jobs = state
            .web_speech_jobs
            .lock()
            .expect("web speech job store lock");
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

fn web_speech_client(state: &ServiceState) -> Result<Arc<dyn SpeechClient>, ApiError> {
    state
        .tts
        .read()
        .expect("TTS state lock")
        .speech
        .as_ref()
        .cloned()
        .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))
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

async fn speech(State(state): State<ServiceState>, request: Request) -> Result<Response, ApiError> {
    authorize(request.headers(), &state.auth)?;

    let speech_client = state
        .tts
        .read()
        .expect("TTS state lock")
        .speech
        .as_ref()
        .cloned()
        .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))?;

    let Json(body) = Json::<OpenAiSpeechRequest>::from_request(request, &state)
        .await
        .map_err(ApiError::json_rejection)?;

    let voice = body.voice.filter(|voice| !voice.trim().is_empty());

    if body.input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }
    if body.model.trim().is_empty() {
        return Err(ApiError::bad_request("model is required"));
    }

    let format = match body.response_format.as_deref() {
        None | Some("") => SpeechFormat::Mp3,
        Some(s) => SpeechFormat::from_openai(s)
            .ok_or_else(|| ApiError::bad_request(format!("unsupported response_format: {s:?}; supported values are mp3, opus, aac, flac, wav, pcm")))?,
    };

    let request = SpeechRequest {
        input: body.input,
        model_hint: body.model,
        voice_hint: voice,
        instructions: body.instructions,
        format,
        speed: body.speed.or(body.rate),
    };

    synthesize_response(speech_client.as_ref(), &request).await
}

async fn synthesize_response(
    speech_client: &dyn SpeechClient,
    request: &SpeechRequest,
) -> Result<Response, ApiError> {
    let synthesized = speech_client
        .synthesize(request)
        .await
        .map_err(ApiError::from_speech_error)?;

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, synthesized.mime_type.clone());

    response = response.header("X-Codex-Voice-Format", synthesized.format.to_openai());

    response
        .body(axum::body::Body::from(synthesized.bytes))
        .map_err(|error| ApiError::internal(format!("failed to build response: {error}")))
}

#[derive(Debug, Serialize)]
struct Health {
    ok: bool,
    capabilities: ServiceCapabilities,
}

#[derive(Debug, Serialize)]
struct TranscriptionResponse {
    text: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) kind: &'static str,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            kind: "bad_request",
            message: message.into(),
        }
    }

    pub(crate) fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            kind: "payload_too_large",
            message: message.into(),
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            kind: "unauthorized",
            message: "missing or invalid bearer token".into(),
        }
    }

    pub(crate) fn backend(message: impl Into<String>) -> Self {
        let message = message.into();
        let redacted = codex_voice_core::redact_diagnostics(&message);
        let message = if redacted.len() > 1500 {
            let mut t = redacted;
            t.truncate(1500);
            t.push_str("...");
            t
        } else {
            redacted
        };
        Self {
            status: StatusCode::BAD_GATEWAY,
            kind: "backend_error",
            message,
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal_error",
            message: message.into(),
        }
    }

    pub(crate) fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            kind: "service_unavailable",
            message: message.into(),
        }
    }

    pub(crate) fn from_speech_error(error: codex_voice_core::SpeechError) -> Self {
        match error {
            codex_voice_core::SpeechError::Unsupported(msg) => Self::bad_request(msg),
            codex_voice_core::SpeechError::Config(msg) => Self::bad_request(msg),
            codex_voice_core::SpeechError::Auth(msg) => Self::service_unavailable(msg),
            other => Self::backend(format!("{other}")),
        }
    }

    pub(crate) fn json_rejection(error: axum::extract::rejection::JsonRejection) -> Self {
        let status = error.status();
        let kind = match status {
            StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
            StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
            _ => "bad_request",
        };
        Self {
            status,
            kind,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "error": {
                "type": self.kind,
                "message": self.message,
            }
        }));
        (self.status, body).into_response()
    }
}

fn authorize(headers: &HeaderMap, auth: &ServiceAuth) -> Result<(), ApiError> {
    if auth.no_auth {
        return Ok(());
    }
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(ApiError::unauthorized());
    };
    let expected = format!("Bearer {}", auth.token);
    if constant_time_eq(value.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0_u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => Some(signal),
                Err(error) => {
                    tracing::warn!(%error, "failed to listen for SIGTERM");
                    None
                }
            };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    tracing::warn!(%error, "failed to listen for ctrl-c");
                }
            }
            _ = async {
                if let Some(terminate) = terminate.as_mut() {
                    terminate.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to listen for ctrl-c");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use axum::body;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[tokio::test]
    async fn cors_preflight_allows_browser_transcription_request() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method(axum::http::Method::OPTIONS)
                    .uri("/v1/audio/transcriptions")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "authorization,content-type",
                    )
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:5173")
        );
    }

    #[tokio::test]
    async fn cors_headers_are_present_on_unauthorized_response() {
        let app = service_router(test_state(1024));
        let mut request = multipart_request("/v1/audio/transcriptions", "tiny wav", None);
        request
            .headers_mut()
            .insert(header::ORIGIN, "http://localhost:5173".parse().unwrap());

        let response = app.oneshot(request).await.expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("http://localhost:5173")
        );
    }

    #[tokio::test]
    async fn web_app_returns_phone_tts_shell() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/web")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/html")),
            "web app should return text/html"
        );
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let html = std::str::from_utf8(&bytes).expect("html is utf-8");
        assert!(html.contains(
            r#"<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover">"#
        ));
        assert!(html.contains("<textarea id=\"text\""));
        assert!(html.contains(&format!(
            r#"<img class="app-icon" src="{}" alt="Codex Voice">"#,
            versioned_web_asset("/web/icon-192.png")
        )));
        assert!(!html.contains("<h1>Codex Voice</h1>"));
        assert!(html.contains("id=\"provider\""));
        assert!(html.contains("id=\"voice\""));
        assert!(html.contains("id=\"model\""));
        assert!(html.contains("id=\"theme\""));
        assert!(html.contains("<option value=\"auto\">Auto</option>"));
        assert!(html.contains("<option value=\"dark\">Dark</option>"));
        assert!(html.contains("<option value=\"light\">Light</option>"));
        assert!(html.contains("id=\"emotion\""));
        assert!(html.contains("id=\"summarize\""));
        assert!(html.contains("id=\"generate-on-paste\""));
        assert!(html.contains("Generate on paste"));
        assert!(html.contains("id=\"generate\""));
        assert!(html.contains("id=\"generate-label\""));
        assert!(html.contains("id=\"clear\""));
        assert!(html.contains("id=\"paste\""));
        assert!(html.contains("id=\"download\""));
        assert!(html.contains("id=\"settings-toggle\""));
        assert!(html.contains("id=\"error-banner\""));
        assert!(html.contains("id=\"waveform-slider\""));
        assert!(html.contains("role=\"slider\""));
        assert!(html.contains("aria-valuetext=\"0:00 of 0:00\""));
        assert!(html.contains("<canvas id=\"waveform\""));
        assert!(html.contains("class=\"waveform-marker\""));
        assert!(html.contains("class=\"waveform-thumb\""));
        assert!(html.contains(".waveform-slider.scrubbing .waveform-thumb"));
        assert!(html.contains("-webkit-tap-highlight-color: transparent;"));
        assert!(html.contains("min-height: 44px;"));
        assert!(html.contains("height: 34px;"));
        assert!(html.contains("opacity: 0;"));
        assert!(!html.contains("type=\"range\""));
        assert!(!html.contains("id=\"status\""));
        assert!(html.contains("codex-voice.web.config.v1"));
        assert!(html.contains("codex-voice.web.settings.v1"));
        assert!(html.contains("generateOnPaste: true"));
        assert!(html.contains("generateOnPaste.checked = settings.generateOnPaste !== false"));
        assert!(html
            .contains("const themeMedia = window.matchMedia?.('(prefers-color-scheme: light)')"));
        assert!(html.contains("function applyThemeSetting"));
        assert!(html.contains("themeSelect.addEventListener('change', saveSettings)"));
        assert!(html.contains("function handleThemeMediaChange()"));
        assert!(html.contains("themeMedia.addEventListener('change', handleThemeMediaChange)"));
        assert!(html.contains("themeMedia.addListener(handleThemeMediaChange)"));
        assert!(html.contains("emotionPreprocessing"));
        assert!(html.contains("summarization"));
        assert!(html.contains("theme: 'auto'"));
        assert!(html.contains("function providerCanGenerate"));
        assert!(html.contains("function firstPersonaForProvider"));
        assert!(html.contains("personaSupportsProvider(persona, providerSelect.value)"));
        assert!(html.contains("providerSelect.addEventListener('change', populateSettings)"));
        let text_idx = html
            .find("class=\"text-shell\"")
            .expect("text shell exists");
        let clear_idx = html.find("id=\"clear\"").expect("clear button exists");
        let scrubber_idx = html.find("class=\"scrubber\"").expect("scrubber exists");
        let buttons_idx = html.find("class=\"buttons\"").expect("buttons exist");
        let play_idx = html.find("id=\"play\"").expect("play button exists");
        let download_idx = html
            .find("id=\"download\"")
            .expect("download button exists");
        let settings_idx = html
            .find("id=\"settings-toggle\"")
            .expect("settings button exists");
        assert!(text_idx < scrubber_idx);
        assert!(text_idx < clear_idx);
        assert!(clear_idx < scrubber_idx);
        assert!(scrubber_idx < buttons_idx);
        assert!(buttons_idx < play_idx);
        assert!(play_idx < download_idx);
        assert!(download_idx < settings_idx);
        assert!(html.contains("modelSelect.addEventListener('change', saveSettings)"));
        assert!(html.contains("codex-voice-web-audio"));
        assert!(html.contains("codex-voice.web.generation.v1"));
        assert!(html.contains("function savePendingGeneration"));
        assert!(html.contains("function resumePendingGeneration"));
        assert!(html.contains("function createWebSpeechJob"));
        assert!(html.contains("function waitForWebSpeechJob"));
        assert!(html.contains("fetch('/web/speech-jobs'"));
        assert!(html.contains("`/web/speech-jobs/${encodeURIComponent(jobId)}`"));
        assert!(html.contains("savePendingGeneration(input, activeJobId)"));
        assert!(html.contains("runGeneration(pending.input, pending.jobId || null)"));
        assert!(html.contains("function runGeneration"));
        assert!(html.contains("function saveLastGeneratedAudio"));
        assert!(html.contains("function restoreLastGeneratedAudio"));
        assert!(html.contains("function currentDraftText"));
        assert!(html.contains("function shouldApplyGeneratedText"));
        assert!(html.contains("currentDraft === generationInput || currentDraft === generatedText"));
        assert!(html.contains("shouldApplyGeneratedText(pending.input, pending.input)"));
        assert!(html.contains("shouldApplyGeneratedText(input, result.input)"));
        assert!(html.contains("saveLastGeneratedAudio(result.blob, result.input"));
        assert!(html.contains("window.addEventListener('pagehide'"));
        assert!(html.contains("pendingWorkerReload"));
        assert!(html.contains("generationActive"));
        assert!(html.contains("function shouldDeferWorkerReload"));
        assert!(html.contains("return generationActive || Boolean(activeStreamPlayback);"));
        assert!(html.contains("function reloadForWorkerUpdateWhenIdle"));
        assert!(html.contains("reloadForWorkerUpdateWhenIdle();"));
        assert!(html.contains("lifecycleInterruptedGeneration"));
        assert!(html.contains("function shouldKeepPendingGeneration"));
        assert!(html.contains("const serverJobMaxPollMs = 10 * 60 * 1000;"));
        assert!(html.contains("function cancelActiveGeneration"));
        assert!(html.contains("activeGenerationController?.abort();"));
        assert!(html.contains("throwIfGenerationCancelled(controller.signal, runId)"));
        assert!(html.contains("if (!pending.jobId)"));
        assert!(html.contains("if (resumeJobId) savePendingGeneration(input, resumeJobId);"));
        assert!(html.contains("generateDirect(input, controller.signal, runId)"));
        assert!(html.contains("async function synthesizeProvider(config, provider, input, persona, prepCache, signal = null"));
        assert!(html.contains("clear.disabled = false;"));
        assert!(html.contains("TTS job stayed pending for too long"));
        assert!(html.contains("if (error?.status) return false;"));
        assert!(html.contains("showError(error.message || 'TTS failed.')"));
        assert!(html.contains("settings.provider !== 'auto'"));
        assert!(html.contains("function providerModelOptions"));
        assert!(html.contains("function selectedProviderModel"));
        assert!(html.contains("return selectedProviderModel('google', google.model);"));
        assert!(html.contains("model_id: resolveElevenLabsModel(elevenlabs)"));
        assert!(html.contains("prep.mode === 'shorten'"));
        assert!(html.contains("function prepareDecision"));
        assert!(html.contains("function speechPrepForStreaming"));
        assert!(html.contains("threshold: 0"));
        assert!(html.contains("minShortenOutputChars = 4000"));
        assert!(html.contains("function shortenPrepareFloor"));
        assert!(html.contains("function shortenMinOutputChars"));
        assert!(html.contains("function providerMaxTextLength"));
        assert!(html.contains("function speechPrepForProviderLimit"));
        assert!(html.contains("function shortenFitLimit"));
        assert!(html.contains("function extractiveShortenToFit"));
        assert!(html.contains("forceSummarization: true"));
        assert!(html.contains("prep.forceSummarization"));
        assert!(html.contains("function truncateToChars"));
        assert!(html.contains("performancePrep = await prepareForProvider"));
        assert!(html
            .contains("const forcePerformanceTags = canStreamProvider(config, provider, persona)"));
        assert!(html.contains("{ forcePerformanceTags, requireBrowserPrep: true }"));
        assert!(html.contains("Do not collapse prose into a short abstract"));
        assert!(html.contains("a fitted source excerpt was used"));
        assert!(html.contains("clamp(Math.floor(prep.maxLength / 3), 64, 4096)"));
        assert!(html.contains("function speechPrepStrategy"));
        assert!(html.contains("function googleSpeechPrepFallback"));
        assert!(html.contains("function browserSpeechPrepForDirect"));
        assert!(html.contains("browserFallback"));
        assert!(html.contains("function buildStyleInstructionPrompt"));
        assert!(html.contains("function styleInstructionIsValid"));
        assert!(html.contains("const prepCache = new Map()"));
        assert!(html.contains("Additional delivery hints:"));
        assert!(html.contains("function showError"));
        assert!(html.contains("function clearError"));
        assert!(html.contains(".generate-progress"));
        assert!(html.contains("left: 0;"));
        assert!(html.contains("right: 0;"));
        assert!(html.contains("bottom: 0;"));
        assert!(html.contains("--visual-viewport-height"));
        assert!(html.contains("--visual-viewport-offset-top"));
        assert!(html.contains("html.keyboard-open .text-shell"));
        assert!(html.contains("function updateVisualViewportLayout"));
        assert!(html.contains("window.visualViewport.addEventListener('resize'"));
        assert!(html.contains("document.documentElement.classList.toggle('keyboard-open'"));
        assert!(html.contains("function setGenerateProgress"));
        assert!(html.contains("function setGenerating"));
        assert!(html.contains("function playSvg"));
        assert!(html.contains("function resetWaveform"));
        assert!(html.contains("let waveformDecodeId = 0;"));
        assert!(html.contains("waveformDecodeId += 1;"));
        assert!(html.contains("function resetStreamingWaveform"));
        assert!(html.contains("function decodeWaveformBlob"));
        assert!(html.contains("function appendStreamingWaveformPcm"));
        assert!(html.contains("function samplePeaks"));
        assert!(html.contains("sumSquares += peak * peak"));
        assert!(
            html.contains("sampled.push(clamp((mean * 0.62) + (rms * 0.28) + (max * 0.1), 0, 1))")
        );
        assert!(html.contains("function peakContrastRange"));
        assert!(html.contains("* 0.12"));
        assert!(html.contains("* 0.9"));
        assert!(html.contains("function drawEmptyWaveform"));
        assert!(html.contains("function drawPeakWaveform"));
        assert!(html.contains("const maxBar = Math.max(12, height * 0.86);"));
        assert!(html.contains("const contrast = peakContrastRange(peaks);"));
        assert!(html.contains(
            "const relativePeak = clamp((peak - contrast.floor) / contrastRange, 0, 1);"
        ));
        assert!(
            html.contains("const visualPeak = clamp((Math.pow(relativePeak, 0.86) * 0.94) + (peak * 0.08), 0, 1);")
        );
        assert!(html.contains("function seekTimeFromClientX"));
        assert!(html.contains("function handleWaveformPointer"));
        assert!(html.contains("function showKeyboardScrubFeedback"));
        assert!(html.contains("seekSlider.classList.add('scrubbing')"));
        assert!(html.contains("seekSlider.classList.remove('scrubbing')"));
        assert!(html.contains("seekSlider.addEventListener('pointerdown'"));
        assert!(html.contains("seekSlider.addEventListener('keydown'"));
        assert!(html.contains("activeStreamPlayback.seekTo(target)"));
        assert!(html.contains("decodeWaveformBlob(blob)"));
        assert!(html.contains("function audioDownloadExtension"));
        assert!(html.contains("function downloadCurrentAudio"));
        assert!(html.contains("download.addEventListener('click', downloadCurrentAudio)"));
        assert!(html.contains("settingsToggle.addEventListener('click'"));
        assert!(html.contains("paste.addEventListener('click'"));
        assert!(html.contains("text.addEventListener('paste', generateAfterPaste)"));
        assert!(html.contains("generateOnPaste.addEventListener('change', saveSettings)"));
        assert!(html.contains("function generateCurrentText"));
        assert!(html.contains("function generateAfterPaste"));
        assert!(html.contains("event?.clipboardData?.getData('text')"));
        assert!(html.contains("const valueBeforePaste = text.value;"));
        assert!(html.contains("if (text.value === valueBeforePaste) return;"));
        assert!(
            html.contains("if (settings.generateOnPaste !== false) await generateCurrentText();")
        );
        assert!(html.contains("navigator.clipboard.readText()"));
        assert!(html.contains("text.value = '';"));
        assert!(html.contains("setGenerateProgress(0.64, 'Synthesizing')"));
        assert!(html.contains("setGenerateProgress(0.9, 'Saving')"));
        assert!(html.contains("setGenerateProgress(1, 'Done')"));
        assert!(html.contains("performanceTagsMaxOutputTokens = 384"));
        assert!(html.contains("performanceTagsAbsoluteMaxOutputTokens = 4096"));
        assert!(html.contains("prep?.capPerformanceTags ? performanceTagsMaxOutputTokens"));
        assert!(html.contains("function performanceTagsOutputTokens"));
        assert!(html.contains("defaultSpeechPrepAttemptTimeoutMs = 4000"));
        assert!(html.contains("function speechPrepModels"));
        assert!(html.contains("function speechPrepErrorIsRetryable"));
        assert!(html.contains("function fetchSpeechPrepAttempt"));
        assert!(html.contains("function fetchCodexPrepAttempt"));
        assert!(html.contains("function sanitizeBrowserConfig"));
        assert!(html.contains("delete config.speechPrep.codexAuth"));
        assert!(html.contains("function parseCodexSse"));
        assert!(html.contains("chatgpt-account-id"));
        assert!(html.contains("Codex direct emotion prep is blocked by the browser or network."));
        assert!(html.contains("thinkingLevel: 'MINIMAL'"));
        assert!(html.contains("function performanceTagsPreserveText"));
        assert!(html.contains("function repairBareLeadingPerformanceCue"));
        assert!(html.contains("function looksLikeBarePerformanceCue"));
        assert!(html.contains("function repairSentenceBoundaryBareCues"));
        assert!(html.contains("'smiles softly'"));
        assert!(html.contains("'smiles and lowers my voice'"));
        assert!(html.contains("'leans over and kisses your lips softly'"));
        assert!(html.contains("'kiss', 'kisses', 'kissing', 'lips'"));
        assert!(html.contains("function performanceTagsAreValid"));
        assert!(html.contains("Every performance cue you add must be enclosed in square brackets"));
        assert!(html.contains("prepared = repairBareLeadingPerformanceCue(input, prepared, prep)"));
        assert!(html.contains("function fallbackPerformanceTags"));
        assert!(html.contains("fetch('/web/config'"));
        assert!(html.contains("function nonRetryableError"));
        assert!(html.contains("error.retryable = false;"));
        assert!(html.contains("if (options.requireBrowserPrep) throw nonRetryableError(message);"));
        assert!(html.contains("if (error?.retryable === false) return false;"));
        assert!(html.contains("function splitTtsText"));
        assert!(html.contains("function concatUint8Arrays"));
        assert!(html.contains("ttsChunkBoundarySilenceMs = 180"));
        assert!(html.contains("function concatPcmChunksWithBoundarySilence"));
        assert!(html.contains("function concatWavChunksWithBoundarySilence"));
        assert!(html.contains("let activeStreamPlayback = null"));
        assert!(html.contains("function ttsStreamPcmGain"));
        assert!(html.contains("providers?.elevenlabs?.streamGain"));
        assert!(html.contains(
            "if (elevenlabs.languageCode) body.language_code = elevenlabs.languageCode;"
        ));
        assert!(html.contains("function applyPcm16Gain"));
        assert!(html.contains("function evenPcmBytes"));
        assert!(html.contains("const model = resolveElevenLabsModel(elevenlabs).toLowerCase();"));
        assert!(html.contains("class StreamingPlayback"));
        assert!(html.contains("if (this.stopped || activeStreamPlayback !== this) return;"));
        assert!(html.contains("this.seekSerial = 0;"));
        assert!(html.contains("const sourceContext = this.context;"));
        assert!(html.contains("const sourceSeekSerial = this.seekSerial;"));
        assert!(
            html.contains("this.context !== sourceContext || this.seekSerial !== sourceSeekSerial")
        );
        assert!(html.contains("const wasPlaying = this.playing;"));
        assert!(html.contains("const seekSerial = this.seekSerial + 1;"));
        assert!(html.contains("previousContext?.close?.().catch(() => {});"));
        assert!(html.contains("this.context.currentTime >= this.nextStartTime + 0.08"));
        assert!(html.contains("this.pendingSources = 0;"));
        assert!(html.contains("function createPcmStreamSink"));
        assert!(html.contains("function websocketBaseUrl"));
        assert!(html.contains("function resolveElevenLabsStreamingModel"));
        assert!(html.contains(
            "return elevenLabsWebSocketModelSupported(model) ? Boolean(window.WebSocket) : Boolean(window.ReadableStream);"
        ));
        assert!(html.contains("async function streamElevenLabs"));
        assert!(html.contains("return resolveElevenLabsModel(elevenlabs);"));
        assert!(html.contains("async function streamElevenLabsHttp"));
        assert!(html.contains("/v1/text-to-speech/${encodeURIComponent(voiceId)}/stream"));
        assert!(html.contains("model_id: modelId"));
        assert!(html.contains("/stream-input"));
        assert!(html.contains("text: ' '"));
        assert!(html.contains("xi_api_key: elevenlabs.apiKey"));
        assert!(html.contains("function googleInteractionsBaseUrl"));
        assert!(html.contains("async function readGoogleInteractionStream"));
        assert!(html.contains("async function streamGoogle"));
        assert!(html.contains("'Api-Revision': '2026-05-20'"));
        assert!(html.contains("stream: true"));
        assert!(html.contains("function tryStreamProvider"));
        assert!(html.contains("const streamed = await tryStreamProvider"));
        assert!(html.contains("const gained = applyPcm16Gain(pcm)"));
        assert!(html.contains("parts.push(gained)"));
        assert!(html.contains("appendPcm(bytes, sampleRate, channels = 1, waveformBytes = bytes)"));
        assert!(html.contains("appendStreamingWaveformPcm(waveformBytes, sampleRate, channels)"));
        assert!(html.contains("playback.appendPcm(gained, sampleRate, channels, pcm)"));
        assert!(html.contains("stopActiveStreamPlayback()"));
        assert!(html.contains("duration.textContent = 'Live'"));
        assert!(html.contains("activeStreamPlayback.toggle()"));
        assert!(html.contains("result.playback.setReplayBlob(result.blob)"));
        assert!(html.contains("function synthesizeGoogle"));
        assert!(html.contains("async function fetchGoogleAudio"));
        assert!(html.contains("function wavBlobFromPcm"));
        assert!(html.contains(
            "return concatWavChunksWithBoundarySilence(audios.map((audio) => audio.bytes));"
        ));
        assert!(html.contains("function synthesizeElevenLabs"));
        assert!(html.contains("async function synthesizeElevenLabsSingle"));
        assert!(html.contains("rawPcm = false"));
        assert!(html.contains("startsWith('pcm') && !rawPcm"));
        assert!(html.contains("const outputFormat = 'pcm_24000'"));
        assert!(html.contains(
            "synthesizeElevenLabsSingle(config, chunk, persona, outputFormat, true, signal, runId)"
        ));
        assert!(html.contains(
            "wavBlobFromPcm(concatPcmChunksWithBoundarySilence(parts, sampleRate), sampleRate)"
        ));
        assert!(html.contains("Emotion prep failed"));
        assert!(html.contains("function generateViaServer"));
        assert!(html.contains("function canGenerateDirectWithConfiguredPrep"));
        assert!(html.contains(
            "return Boolean(config?.providers?.google || config?.providers?.elevenlabs);"
        ));
        assert!(html.contains("function settingsMatchServerDefaults"));
        assert!(html.contains("settings.model === 'default'"));
        assert!(html.contains("settings.emotionPreprocessing === true"));
        assert!(html.contains("settingsMatchServerDefaults()"));
        assert!(html.contains(
            "} else if (directConfig && canGenerateDirectWithConfiguredPrep(directConfig)) {"
        ));
        assert!(html.contains("Configured emotion prep is server-only."));
        assert!(html.contains("'/web/speech-jobs'"));
        assert!(html.contains(&format!(
            r#"<link rel="manifest" href="{}" data-manifest-dark="{}" data-manifest-light="{}">"#,
            versioned_web_asset("/web/manifest.webmanifest"),
            versioned_web_asset("/web/manifest.webmanifest"),
            versioned_web_asset("/web/manifest-light.webmanifest")
        )));
        assert!(html.contains(r##"<meta name="theme-color" content="#17091f">"##));
        assert!(html.contains("setManifest(resolved)"));
        assert!(html.contains("const manifest = document.querySelector('link[rel=\"manifest\"]');"));
        assert!(html.contains("manifest.dataset.manifestLight || manifest.href"));
        assert!(html.contains("manifest.dataset.manifestDark || manifest.href"));
        assert!(html.contains(&format!(
            r#"<link rel="apple-touch-icon" href="{}">"#,
            versioned_web_asset("/web/apple-touch-icon.png")
        )));
        assert!(html.contains("navigator.serviceWorker.register('/web-sw.js'"));
        assert!(html.contains("updateViaCache: 'none'"));
        assert!(html.contains(r#":root[data-theme="light"]"#));
        assert!(html.contains("--bg: #f3dff1;"));
        assert!(html.contains("--panel: #fbf6fb;"));
        assert!(html.contains("--accent: #e53786;"));
        assert!(html.contains("--text-edge-pad: 8px;"));
        assert!(html.contains("--text-button-clearance: 126px;"));
        assert!(html.contains(
            "padding: var(--text-edge-pad) 16px calc(var(--text-button-clearance) + var(--text-edge-pad));"
        ));
        assert!(html.contains(
            "scroll-padding: var(--text-edge-pad) 16px calc(var(--text-button-clearance) + var(--text-edge-pad));"
        ));
        assert!(html.contains("--glass-button-sheen:"));
        assert!(html.contains("radial-gradient(ellipse at 82% 56%"));
        assert!(html.contains("backdrop-filter: var(--glass-button-filter);"));
        assert!(html.contains(".buttons .icon-button::before"));
    }

    #[tokio::test]
    async fn web_config_is_public_and_exports_browser_tts_config() {
        let app = service_router(test_state_with_web_tts_config(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/web/config")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert!(response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json")));

        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let config: serde_json::Value = serde_json::from_slice(&bytes).expect("json response");
        assert_eq!(config["version"], 1);
        assert_eq!(config["defaultProvider"], "google");
        assert_eq!(config["defaultPersona"], "sky");
        assert_eq!(config["speechPrep"]["model"], "google/gemini-3.5-flash");
        assert_eq!(config["speechPrep"]["browserSupported"], true);
        assert_eq!(config["speechPrep"]["strategies"]["google"], "inline-tags");
        assert_eq!(
            config["speechPrep"]["strategies"]["elevenlabs"],
            "inline-tags"
        );
        assert_eq!(config["speechPrep"]["tagPalette"][0], "tender");
        assert_eq!(config["speechPrep"]["capPerformanceTags"], false);
        assert!(config["speechPrep"]["fallbackModels"]
            .as_array()
            .is_some_and(Vec::is_empty));
        assert_eq!(config["speechPrep"]["attemptTimeoutMs"], 4000);
        assert_eq!(config["speechPrep"]["apiKey"], "google-prep-key");
        assert_eq!(config["providers"]["google"]["apiKey"], "google-tts-key");
        assert_eq!(
            config["providers"]["google"]["model"],
            "gemini-3.1-flash-tts-preview"
        );
        assert_eq!(
            config["providers"]["google"]["streaming"]["transport"],
            "interactions-stream"
        );
        assert_eq!(
            config["providers"]["google"]["streaming"]["supportedModels"][0],
            "gemini-3.1-flash-tts-preview"
        );
        assert_eq!(
            config["providers"]["google"]["streaming"]["outputFormat"],
            "pcm_24000"
        );
        assert_eq!(
            config["providers"]["google"]["streaming"]["sampleRate"],
            24000
        );
        assert_eq!(config["providers"]["google"]["streaming"]["channels"], 1);
        assert_eq!(config["providers"]["elevenlabs"]["apiKey"], "eleven-key");
        assert_eq!(
            config["providers"]["elevenlabs"]["streaming"]["transport"],
            "websocket"
        );
        assert_eq!(
            config["providers"]["elevenlabs"]["streaming"]["preferredModel"],
            "eleven_flash_v2_5"
        );
        assert_eq!(
            config["providers"]["elevenlabs"]["streaming"]["outputFormat"],
            "pcm_24000"
        );
        assert_eq!(
            config["providers"]["elevenlabs"]["streaming"]["sampleRate"],
            24000
        );
        assert_eq!(
            config["providers"]["elevenlabs"]["streaming"]["channels"],
            1
        );
        assert_eq!(
            config["providers"]["elevenlabs"]["streaming"]["chunkLengthSchedule"][0],
            120
        );
        assert_eq!(config["providers"]["elevenlabs"]["streamGain"], 2.0);
        assert!(config["providers"]["elevenlabs"]
            .get("languageCode")
            .is_none());
        assert_eq!(
            config["personas"]["sky"]["fallbackPolicy"],
            "preserve-persona"
        );
        assert_eq!(
            config["personas"]["sky"]["elevenlabs"]["voiceId"],
            "eleven-voice"
        );
    }

    #[test]
    fn browser_config_exports_codex_speech_prep_with_cached_auth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let auth_file = temp.path().join("auth.json");
        std::fs::write(
            &auth_file,
            r#"{"tokens":{"access_token":"access-token","refresh_token":"refresh-token","account_id":"account-id"}}"#,
        )
        .expect("auth written");
        let mut config = sample_tts_config();
        let prep = config.speech_prep.as_mut().expect("speech prep exists");
        prep.provider = SpeechPrepProviderKind::Codex;
        prep.api_key = None;
        prep.auth_file = Some(auth_file);
        prep.base_url = "https://chatgpt.com/backend-api/codex".to_string();
        prep.model = "gpt-5.3-codex-spark".to_string();
        prep.fallback_models = Vec::new();
        prep.reasoning_effort = Some("medium".to_string());

        let browser_config = BrowserTtsConfig::from_resolved(&config);
        let json = serde_json::to_value(browser_config).expect("serializes");

        assert_eq!(json["speechPrep"]["provider"], "codex");
        assert_eq!(json["speechPrep"]["browserSupported"], false);
        assert_eq!(json["speechPrep"]["browserFallback"]["provider"], "google");
        assert_eq!(
            json["speechPrep"]["browserFallback"]["apiKey"],
            "google-tts-key"
        );
        assert_eq!(
            json["speechPrep"]["browserFallback"]["baseUrl"],
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(
            json["speechPrep"]["browserFallback"]["model"],
            "google/gemini-3.5-flash"
        );
        assert_eq!(json["speechPrep"]["model"], "gpt-5.3-codex-spark");
        assert!(json["speechPrep"]["fallbackModels"]
            .as_array()
            .is_some_and(Vec::is_empty));
        assert_eq!(json["speechPrep"]["reasoningEffort"], "medium");
        assert!(json["speechPrep"].get("codexAuth").is_none());
        assert!(json["speechPrep"].get("apiKey").is_none());
    }

    #[tokio::test]
    async fn web_config_returns_503_without_tts_config() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/web/config")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    fn write_reload_test_config(path: &FsPath, env_name: &str, voice: &str) {
        std::fs::write(
            path,
            format!(
                r#"{{
                    "messages": {{
                        "tts": {{
                            "provider": "google",
                            "providers": {{
                                "google": {{
                                    "apiKey": {{ "source": "env", "id": "{env_name}" }},
                                    "voice": "{voice}",
                                    "model": "gemini-2.5-flash-preview-tts"
                                }}
                            }}
                        }}
                    }}
                }}"#
            ),
        )
        .expect("config written");
    }

    #[tokio::test]
    async fn tts_config_reload_updates_swappable_service_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("read-aloud-defaults.json");
        std::env::set_var("TEST_TTS_RELOAD_KEY", "test-google-key");
        write_reload_test_config(&path, "TEST_TTS_RELOAD_KEY", "Sulafat");
        let tts = Arc::new(RwLock::new(TtsServiceState::from_parts(None, None)));

        reload_tts_config_once(&tts, &path)
            .await
            .expect("config reload succeeds");

        let state = tts.read().expect("TTS state lock");
        assert!(state.speech.is_some());
        let web_config = state.web_tts_config.clone().expect("web config loaded");
        let json = serde_json::to_value(web_config).expect("serializes");
        assert_eq!(json["providers"]["google"]["voice"], "Sulafat");
    }

    #[tokio::test]
    async fn tts_config_reload_keeps_previous_state_when_new_config_is_invalid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("read-aloud-defaults.json");
        std::env::set_var("TEST_TTS_RELOAD_KEEP_KEY", "test-google-key");
        write_reload_test_config(&path, "TEST_TTS_RELOAD_KEEP_KEY", "Sulafat");
        let tts = Arc::new(RwLock::new(TtsServiceState::from_parts(None, None)));
        reload_tts_config_once(&tts, &path)
            .await
            .expect("initial config reload succeeds");
        let before = serde_json::to_value(
            tts.read()
                .expect("TTS state lock")
                .web_tts_config
                .clone()
                .expect("web config loaded"),
        )
        .expect("serializes");

        std::fs::write(&path, "{not valid json").expect("invalid config written");
        let error = reload_tts_config_once(&tts, &path)
            .await
            .expect_err("invalid config should fail");
        assert!(error
            .to_string()
            .contains("failed to load read-aloud config"));

        let after = serde_json::to_value(
            tts.read()
                .expect("TTS state lock")
                .web_tts_config
                .clone()
                .expect("web config remains loaded"),
        )
        .expect("serializes");
        assert_eq!(after, before);
    }

    #[test]
    fn browser_config_export_omits_absent_providers() {
        let mut config = sample_tts_config();
        config.elevenlabs = None;
        let exported = BrowserTtsConfig::from_resolved(&config);
        let json = serde_json::to_value(exported).expect("serializes");

        assert_eq!(json["providers"]["google"]["apiKey"], "google-tts-key");
        assert!(json["providers"].get("elevenlabs").is_none());
        assert_eq!(json["personas"]["sky"]["google"]["voiceName"], "Sulafat");
    }

    #[tokio::test]
    async fn web_manifest_returns_install_metadata() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/web/manifest.webmanifest")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/manifest+json"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let manifest: serde_json::Value =
            serde_json::from_slice(&bytes).expect("manifest is valid json");

        assert_eq!(manifest["name"], "Codex Voice");
        assert_eq!(manifest["short_name"], "Voice");
        assert_eq!(manifest["start_url"], "/web");
        assert_eq!(manifest["scope"], "/web");
        assert_eq!(manifest["display"], "standalone");
        assert_eq!(manifest["theme_color"], "#17091f");
        assert_eq!(manifest["background_color"], "#17091f");
        assert_eq!(manifest["version"], web_build_version());
        assert_eq!(manifest["build_revision"], WEB_BUILD_REVISION);
        let icons = manifest["icons"].as_array().expect("icons array");
        assert!(icons.iter().any(|icon| {
            icon["src"] == versioned_web_asset("/web/icon-192.png")
                && icon["sizes"] == "192x192"
                && icon["type"] == "image/png"
        }));
        assert!(icons.iter().any(|icon| {
            icon["src"] == versioned_web_asset("/web/icon-512.png")
                && icon["sizes"] == "512x512"
                && icon["purpose"] == "any"
        }));
        assert!(icons.iter().any(|icon| {
            icon["src"] == versioned_web_asset("/web/icon-maskable-512.png")
                && icon["sizes"] == "512x512"
                && icon["purpose"] == "maskable"
        }));

        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/web/manifest-light.webmanifest")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let manifest: serde_json::Value =
            serde_json::from_slice(&bytes).expect("manifest is valid json");
        assert_eq!(manifest["theme_color"], "#f3dff1");
        assert_eq!(manifest["background_color"], "#f3dff1");
    }

    #[tokio::test]
    async fn web_service_worker_returns_install_and_fetch_handlers() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/web-sw.js")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let script = std::str::from_utf8(&bytes).expect("script is utf-8");
        assert!(script.contains("self.addEventListener('install'"));
        assert!(script.contains("self.addEventListener('fetch'"));
        assert!(script.contains("request.method !== 'GET'"));
        assert!(script.contains(&format!(
            "const CACHE_NAME = {};",
            serde_json::to_string(&web_cache_name()).expect("cache name serializes")
        )));
        assert!(script.contains(&format!(
            "const WEB_BUILD_REVISION = {};",
            serde_json::to_string(WEB_BUILD_REVISION).expect("revision serializes")
        )));
        assert!(script.contains("if (response.ok)"));
        assert!(script.contains("if (cached) return cached;"));
        assert!(script.contains("NETWORK_FIRST_ASSETS"));
        assert!(script.contains("'/web/manifest.webmanifest'"));
        assert!(script.contains("'/web/manifest-light.webmanifest'"));
        assert!(script.contains("`/web/manifest-light.webmanifest?v=${WEB_BUILD_REVISION}`"));
        assert!(script.contains("`/web/icon-192.png?v=${WEB_BUILD_REVISION}`"));
        assert!(script.contains("`/web/apple-touch-icon.png?v=${WEB_BUILD_REVISION}`"));
        assert!(script.contains("networkFirst(request, url.pathname)"));
        assert!(script.contains("`/web/icon-maskable-512.png?v=${WEB_BUILD_REVISION}`"));
    }

    #[tokio::test]
    async fn web_icon_routes_return_png_assets() {
        for path in [
            "/web/icon-192.png",
            "/web/icon-512.png",
            "/web/icon-maskable-512.png",
            "/web/apple-touch-icon.png",
        ] {
            let app = service_router(test_state_with_speech(1024));
            let response = app
                .oneshot(
                    axum::http::Request::builder()
                        .uri(path)
                        .body(body::Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("request succeeds");

            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE).unwrap(),
                "image/png",
                "{path}"
            );
            let bytes = body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body reads");
            assert!(
                bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "{path} should return a PNG"
            );
        }
    }

    #[tokio::test]
    async fn web_speech_is_public_and_uses_service_defaults() {
        let speech = Arc::new(FakeSpeechBackend::default());
        let app = service_router(test_state_with_speech_backend(1024, Some(speech.clone())));

        let response = app
            .oneshot(speech_request(
                "/web/speech",
                r#"{"input":"hello from phone"}"#,
                None,
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json response");
        assert_eq!(json["input"], "hello from phone");
        assert_eq!(json["input_changed"], false);
        assert_eq!(json["mime_type"], "audio/wav");
        assert_eq!(json["format"], "wav");
        assert_eq!(json["audio_base64"], "ZmFrZSBhdWRpbyBieXRlcw==");
        let seen = speech.seen.lock().expect("fake speech lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].input, "hello from phone");
        assert_eq!(seen[0].model_hint, "gpt-4o-mini-tts");
        assert_eq!(seen[0].voice_hint, None);
        assert_eq!(seen[0].instructions, None);
        assert_eq!(seen[0].format, SpeechFormat::Wav);
        assert_eq!(seen[0].speed, None);
    }

    #[tokio::test]
    async fn web_speech_jobs_complete_after_create() {
        let speech = Arc::new(FakeSpeechBackend::default());
        let app = service_router(test_state_with_speech_backend(1024, Some(speech.clone())));

        let response = app
            .clone()
            .oneshot(speech_request(
                "/web/speech-jobs",
                r#"{"input":"hello from background"}"#,
                None,
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json response");
        let id = json["id"].as_str().expect("job id").to_string();
        assert_eq!(json["status"], "pending");

        let mut completed = None;
        for _ in 0..20 {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri(format!("/web/speech-jobs/{id}"))
                        .body(body::Body::empty())
                        .expect("request builds"),
                )
                .await
                .expect("poll succeeds");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body reads");
            let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json response");
            if json["status"] == "complete" {
                completed = Some(json);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let json = completed.expect("job completes");
        assert_eq!(json["id"], id);
        assert_eq!(json["result"]["input"], "hello from background");
        assert_eq!(json["result"]["audio_base64"], "ZmFrZSBhdWRpbyBieXRlcw==");
        let seen = speech.seen.lock().expect("fake speech lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].input, "hello from background");
    }

    #[test]
    fn web_speech_job_pruning_removes_expired_audio_results() {
        let mut jobs = HashMap::new();
        jobs.insert(
            "old".to_string(),
            WebSpeechJobRecord {
                state: WebSpeechJobState::Complete(WebSpeechResponse {
                    input: "old".to_string(),
                    input_changed: false,
                    audio_base64: "audio".to_string(),
                    mime_type: "audio/wav".to_string(),
                    format: "wav".to_string(),
                }),
                updated_at: Instant::now() - WEB_SPEECH_JOB_TTL - Duration::from_secs(1),
            },
        );
        jobs.insert(
            "fresh".to_string(),
            WebSpeechJobRecord::new(WebSpeechJobState::Pending),
        );

        prune_web_speech_jobs(&mut jobs);

        assert!(!jobs.contains_key("old"));
        assert!(jobs.contains_key("fresh"));
    }

    #[tokio::test]
    async fn web_speech_returns_prepared_input_for_visible_tag_edits() {
        let speech = Arc::new(FakeSpeechBackend {
            prepared_input: Some("[softly] hello from phone".to_string()),
            ..Default::default()
        });
        let app = service_router(test_state_with_speech_backend(1024, Some(speech)));

        let response = app
            .oneshot(speech_request(
                "/web/speech",
                r#"{"input":"hello from phone"}"#,
                None,
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json response");
        assert_eq!(json["input"], "[softly] hello from phone");
        assert_eq!(json["input_changed"], true);
        assert_eq!(json["audio_base64"], "ZmFrZSBhdWRpbyBieXRlcw==");
    }

    #[tokio::test]
    async fn web_speech_public_access_does_not_change_api_auth() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","input":"hello"}"#,
                None,
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn web_speech_rejects_empty_input() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(speech_request("/web/speech", r#"{"input":"   "}"#, None))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn web_speech_returns_503_when_tts_not_configured() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(speech_request("/web/speech", r#"{"input":"hello"}"#, None))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn route_aliases_return_openai_json() {
        for path in ["/audio/transcriptions", "/v1/audio/transcriptions"] {
            let app = service_router(test_state(1024));
            let response = app
                .oneshot(multipart_request(path, "tiny wav", Some("test-token")))
                .await
                .expect("request succeeds");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body reads");
            let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
            assert_eq!(value["text"], "hello from service");
        }
    }

    #[tokio::test]
    async fn speech_route_aliases_return_audio_bytes() {
        for path in ["/audio/speech", "/v1/audio/speech"] {
            let app = service_router(test_state_with_speech(1024));
            let response = app
                .oneshot(speech_request(
                    path,
                    r#"{"model":"gpt-4o-mini-tts","voice":"sky","input":"hello","response_format":"wav"}"#,
                    Some("test-token"),
                ))
                .await
                .expect("request succeeds");
            assert_eq!(response.status(), StatusCode::OK);
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .cloned()
                .expect("content-type header present");
            let bytes = body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body reads");
            assert_eq!(&bytes[..], b"fake audio bytes");
            assert_eq!(content_type, "audio/wav");
        }
    }

    #[tokio::test]
    async fn speech_route_accepts_openchamber_rate_alias() {
        let speech = Arc::new(FakeSpeechBackend::default());
        let app = service_router(test_state_with_speech_backend(1024, Some(speech.clone())));

        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","voice":"sky","input":"hello","rate":1.2}"#,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let seen = speech.seen.lock().expect("fake speech lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].speed, Some(1.2_f32));
    }

    #[tokio::test]
    async fn speech_route_prefers_speed_over_rate_alias() {
        let speech = Arc::new(FakeSpeechBackend::default());
        let app = service_router(test_state_with_speech_backend(1024, Some(speech.clone())));

        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","voice":"sky","input":"hello","speed":0.9,"rate":1.2}"#,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        let seen = speech.seen.lock().expect("fake speech lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].speed, Some(0.9_f32));
    }

    #[tokio::test]
    async fn speech_route_allows_omitted_voice() {
        let speech = Arc::new(FakeSpeechBackend::default());
        let app = service_router(test_state_with_speech_backend(1024, Some(speech.clone())));
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","input":"hello"}"#,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        let seen = speech.seen.lock().expect("fake speech lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].voice_hint, None);
    }

    #[tokio::test]
    async fn speech_route_defaults_response_format_to_mp3() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","voice":"sky","input":"hello"}"#,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("X-Codex-Voice-Format")
                .expect("format header"),
            "mp3"
        );
    }

    #[tokio::test]
    async fn speech_route_preserves_payload_too_large_status() {
        let app = service_router(test_state_with_speech(1024));
        let body = format!(
            r#"{{"model":"gpt-4o-mini-tts","voice":"sky","input":"{}"}}"#,
            "a".repeat(SPEECH_BODY_LIMIT_BYTES)
        );
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                &body,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn speech_route_rejects_missing_auth() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","input":"hello"}"#,
                None,
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn speech_route_returns_503_when_tts_not_configured() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","input":"hello"}"#,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn health_includes_capabilities() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["ok"], true);
        assert_eq!(value["capabilities"]["transcriptions"], true);
        assert_eq!(value["capabilities"]["speech"], true);
    }

    #[tokio::test]
    async fn health_shows_speech_false_when_no_tts() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["capabilities"]["speech"], false);
    }

    #[tokio::test]
    async fn rejects_missing_auth() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(multipart_request(
                "/v1/audio/transcriptions",
                "tiny wav",
                None,
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_runs_before_multipart_validation() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/audio/transcriptions")
                    .body(body::Body::from("not multipart"))
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_requires_auth() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(body::Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn response_format_text_returns_plain_text() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(multipart_request_with_response_format(
                "/v1/audio/transcriptions",
                "text",
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        assert_eq!(&bytes[..], b"hello from service");
    }

    #[tokio::test]
    async fn unsupported_response_format_returns_400() {
        let app = service_router(test_state(1024));
        let response = app
            .oneshot(multipart_request_with_response_format(
                "/v1/audio/transcriptions",
                "verbose_json",
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn oversized_upload_without_ffmpeg_returns_413() {
        let app = service_router(test_state(4));
        let response = app
            .oneshot(multipart_request(
                "/v1/audio/transcriptions",
                "this is larger than four bytes",
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn constant_time_comparison_rejects_mismatched_lengths() {
        assert!(!constant_time_eq(b"short", b"longer string"));
    }

    #[test]
    fn constant_time_comparison_rejects_single_byte_diff() {
        assert!(!constant_time_eq(b"test-token", b"test-tookn"));
    }

    #[test]
    fn constant_time_comparison_accepts_exact_match() {
        assert!(constant_time_eq(b"exact-match", b"exact-match"));
    }
}
