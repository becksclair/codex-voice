use anyhow::{Context, Result};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
use codex_voice_core::{SpeechClient, TranscriptionClient};
use codex_voice_tts::config::ResolvedTtsConfig;
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
};
use tokio::net::TcpListener;

use super::discovery::{
    discovery_path, remove_discovery_file_if_current, resolve_or_generate_token, service_root_url,
    write_discovery_file, ServiceCapabilities, TranscriberDiscoveryFile,
};

mod speech;
#[cfg(test)]
mod tests;
mod transcribe;
mod web;

pub(crate) use speech::TtsServiceState;
use speech::{speech, watch_tts_config};
use transcribe::transcribe;
use web::{
    web_app, web_apple_touch_icon, web_config, web_icon_192, web_icon_512, web_icon_maskable_512,
    web_manifest, web_manifest_light, web_service_worker, web_speech, web_speech_job_create,
    web_speech_job_status, WebSpeechJobStore,
};

const SPEECH_BODY_LIMIT_BYTES: usize = 64 * 1024;
const MULTIPART_OVERHEAD_BYTES: u64 = 64 * 1024;

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
pub(crate) struct ServiceAuth {
    pub(crate) token: String,
    pub(crate) no_auth: bool,
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
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(state)
}

async fn health(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> Result<Json<Health>, ApiError> {
    authorize(&headers, &state.auth)?;
    // Recover from a poisoned lock instead of propagating the panic: the guarded
    // state is a plain snapshot (config / job map), not a partially-mutated
    // invariant, so a panic while another thread held the lock leaves the data
    // valid. Recovering keeps one failure from turning into a persistent panic
    // loop on this endpoint. Applied uniformly at every lock site below.
    let tts = state
        .tts
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let capabilities = ServiceCapabilities {
        transcriptions: true,
        speech: tts.speech.is_some(),
    };
    Ok(Json(Health {
        ok: true,
        capabilities,
    }))
}

#[derive(Debug, Serialize)]
struct Health {
    ok: bool,
    capabilities: ServiceCapabilities,
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
