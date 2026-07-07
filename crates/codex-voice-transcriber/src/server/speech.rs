use super::web::BrowserTtsConfig;
use super::{authorize, ApiError, ServiceState};

use anyhow::{Context, Result};
use axum::{
    extract::{FromRequest, Request, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest};
use codex_voice_tts::{config::ResolvedTtsConfig, ConfiguredSpeechClient, ReadAloudConfigLoader};
use serde::Deserialize;
use std::{
    path::{Path as FsPath, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};

const TTS_CONFIG_WATCH_INTERVAL: Duration = Duration::from_secs(2);
const TTS_CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfigFingerprint {
    modified: SystemTime,
    len: u64,
}

pub(crate) async fn watch_tts_config(tts: Arc<RwLock<TtsServiceState>>, path: PathBuf) {
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

pub(crate) async fn reload_tts_config_once(
    tts: &Arc<RwLock<TtsServiceState>>,
    path: &FsPath,
) -> Result<()> {
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

pub(crate) fn web_speech_client(state: &ServiceState) -> Result<Arc<dyn SpeechClient>, ApiError> {
    state
        .tts
        .read()
        .expect("TTS state lock")
        .speech
        .as_ref()
        .cloned()
        .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))
}

pub(crate) async fn speech(
    State(state): State<ServiceState>,
    request: Request,
) -> Result<Response, ApiError> {
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
