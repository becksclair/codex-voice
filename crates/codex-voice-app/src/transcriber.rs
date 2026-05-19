use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::{
    extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
use codex_voice_core::{
    RecordedAudio, SpeechClient, SpeechFormat, SpeechRequest, TranscriptionClient,
    TranscriptionError, TranscriptionResult,
};
use codex_voice_tts::{ConfiguredSpeechClient, ReadAloudConfigLoader};
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tempfile::{NamedTempFile, TempDir};
use tokio::{net::TcpListener, process::Command as TokioCommand, time};

const DEFAULT_SERVICE_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_RUNTIME_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_GENERATED_CHUNKS: usize = 512;
const MULTIPART_OVERHEAD_BYTES: u64 = 64 * 1024;
const PCM_BYTES_PER_SECOND: u64 = 16_000_u64 * 2;
const TOKEN_ENV: &str = "CODEX_VOICE_TRANSCRIBER_TOKEN";
const URL_ENV: &str = "CODEX_VOICE_TRANSCRIBER_URL";

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub bind: SocketAddr,
    pub codex_upload_limit_bytes: u64,
    pub client_upload_limit_bytes: u64,
    pub chunk_seconds: u64,
    pub token_env: String,
    pub ffmpeg_binary: String,
    pub no_auth: bool,
}

#[derive(Debug, Clone)]
pub struct ProbeLimitsConfig {
    pub file: PathBuf,
    pub codex_upload_limit_bytes: u64,
    pub chunk_seconds: u64,
    pub max_chunks: usize,
    pub include_oversized: bool,
    pub ffmpeg_binary: String,
}

pub struct ResolvedTranscriptionBackend {
    pub label: &'static str,
    pub client: RuntimeTranscriptionClient,
}

#[derive(Clone)]
pub enum RuntimeTranscriptionClient {
    Local {
        local: LocalTranscriberClient,
        fallback: Option<CodexTranscriptionClient>,
    },
    Direct(CodexTranscriptionClient),
}

#[async_trait]
impl TranscriptionClient for RuntimeTranscriptionClient {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
        match self {
            Self::Local { local, fallback } => match local.transcribe(recording).await {
                Ok(text) => Ok(text),
                Err(TranscriptionError::Service { status, message }) => {
                    Err(TranscriptionError::Service { status, message })
                }
                Err(error) => {
                    tracing::warn!(%error, "local transcriber failed, attempting direct Codex fallback");
                    if let Some(direct) = fallback {
                        direct.transcribe(recording).await
                    } else {
                        Err(error)
                    }
                }
            },
            Self::Direct(client) => client.transcribe(recording).await,
        }
    }
}

pub async fn resolve_transcription_backend() -> Result<ResolvedTranscriptionBackend> {
    if let Some(local) =
        LocalTranscriberClient::discover(DEFAULT_PROBE_TIMEOUT, DEFAULT_RUNTIME_TIMEOUT).await
    {
        let fallback = CodexAuthService::new()
            .and_then(|auth| CodexTranscriptionClient::with_timeout(auth, DEFAULT_RUNTIME_TIMEOUT))
            .map_err(|error| {
                tracing::warn!(%error, "failed to create direct fallback client; local-only mode");
            })
            .ok();
        return Ok(ResolvedTranscriptionBackend {
            label: "local-service",
            client: RuntimeTranscriptionClient::Local { local, fallback },
        });
    }

    Ok(ResolvedTranscriptionBackend {
        label: "direct-codex",
        client: RuntimeTranscriptionClient::Direct(CodexTranscriptionClient::with_timeout(
            CodexAuthService::new()?,
            DEFAULT_RUNTIME_TIMEOUT,
        )?),
    })
}

const SPEECH_BODY_LIMIT_BYTES: usize = 64 * 1024;

pub async fn serve(config: ServeConfig) -> Result<()> {
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind audio service on {}", config.bind))?;
    let local_addr = listener.local_addr()?;
    let backend = Arc::new(CodexTranscriptionClient::with_timeout(
        CodexAuthService::new()?,
        DEFAULT_SERVICE_TIMEOUT,
    )?);
    let root_url = service_root_url(local_addr);
    let token = resolve_or_generate_token(&config.token_env);

    let speech = match load_speech_client().await {
        Ok(client) => {
            tracing::info!("TTS client loaded successfully");
            Some(client)
        }
        Err(error) => {
            tracing::warn!(%error, "TTS client not available; speech endpoint will return 503");
            None
        }
    };

    let capabilities = ServiceCapabilities {
        transcriptions: true,
        speech: speech.is_some(),
    };
    let discovery = TranscriberDiscoveryFile::new(root_url, token, capabilities.clone());
    write_discovery_file(&discovery)?;

    let app = service_router(ServiceState {
        backend,
        speech,
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

async fn load_speech_client() -> anyhow::Result<Arc<dyn SpeechClient>> {
    let path = ReadAloudConfigLoader::default_path()
        .map_err(|e| anyhow::anyhow!("failed to resolve config path: {e}"))?;
    let loader = ReadAloudConfigLoader::new(path);
    let config = loader
        .load()
        .map_err(|e| anyhow::anyhow!("failed to load read-aloud config: {e}"))?;
    let client = ConfiguredSpeechClient::try_new(config)
        .map_err(|e| anyhow::anyhow!("failed to create TTS client: {e}"))?;
    if !client.has_any_provider() {
        return Err(anyhow::anyhow!(
            "TTS config parsed but no usable provider is configured (no Google or ElevenLabs client could be created)"
        ));
    }
    Ok(Arc::new(client))
}

pub async fn probe_limits(config: ProbeLimitsConfig) -> Result<()> {
    let source_size = fs::metadata(&config.file)
        .with_context(|| format!("failed to stat {}", config.file.display()))?
        .len();
    println!("file: {}", config.file.display());
    println!("source_bytes: {source_size}");
    println!(
        "codex_upload_limit_bytes: {}",
        config.codex_upload_limit_bytes
    );

    let backend =
        CodexTranscriptionClient::with_timeout(CodexAuthService::new()?, DEFAULT_SERVICE_TIMEOUT)?;

    if source_size <= config.codex_upload_limit_bytes || config.include_oversized {
        probe_one(
            &backend,
            "source",
            &config.file,
            source_size,
            source_content_type(&config.file),
        )
        .await;
    } else {
        println!("attempt=source status=skipped reason=exceeds_configured_limit");
    }

    if source_size <= config.codex_upload_limit_bytes {
        return Ok(());
    }

    if !ffmpeg_available(&config.ffmpeg_binary).await {
        println!("attempt=chunks status=skipped reason=ffmpeg_missing");
        return Ok(());
    }

    if config.max_chunks == 0 {
        println!("attempt=chunks status=skipped reason=max_chunks_zero");
        return Ok(());
    }

    let chunk_seconds =
        effective_chunk_seconds(config.chunk_seconds, config.codex_upload_limit_bytes);
    let chunks = split_audio_with_ffmpeg(&config.ffmpeg_binary, &config.file, chunk_seconds, None)
        .await
        .context("failed to split audio for limit probe")?;
    let limit = config.max_chunks.min(chunks.paths.len());
    for (index, path) in chunks.paths.iter().take(limit).enumerate() {
        let bytes = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let label = format!("chunk-{index}");
        probe_one(&backend, &label, path, bytes, "audio/wav").await;
    }
    if chunks.paths.len() > limit {
        println!(
            "attempt=chunks status=partial tested={} total={}",
            limit,
            chunks.paths.len()
        );
    }
    Ok(())
}

async fn probe_one(
    backend: &CodexTranscriptionClient,
    label: &str,
    path: &Path,
    bytes: u64,
    content_type: &str,
) {
    let recording = RecordedAudio {
        path: path.to_path_buf(),
        content_type: content_type.to_string(),
        filename: filename_for_path(path),
        duration: Duration::default(),
    };
    match backend.transcribe(&recording).await {
        Ok(transcript) => {
            println!(
                "attempt={label} bytes={bytes} status=ok transcript_chars={}",
                transcript.chars().count()
            );
        }
        Err(error) => {
            println!(
                "attempt={label} bytes={bytes} status=error error={}",
                redact_error(&error.to_string())
            );
        }
    }
}

#[derive(Clone)]
pub struct LocalTranscriberClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl LocalTranscriberClient {
    fn new(
        base_url: String,
        token: String,
        timeout: Duration,
    ) -> TranscriptionResult<LocalTranscriberClient> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| {
                TranscriptionError::Request(format!(
                    "failed to build local transcriber HTTP client: {error}"
                ))
            })?;
        Ok(Self {
            base_url,
            token,
            http,
        })
    }

    async fn discover(
        probe_timeout: Duration,
        runtime_timeout: Duration,
    ) -> Option<LocalTranscriberClient> {
        let candidate = resolve_discovery_candidate()?;
        let probe = Self::new(
            candidate.base_url.clone(),
            candidate.token.clone(),
            probe_timeout,
        )
        .map_err(|error| {
            tracing::debug!(%error, "failed to create local transcriber probe client");
        })
        .ok()?;
        if let Err(error) = probe.health_check().await {
            tracing::debug!(%error, "local transcriber probe failed");
            return None;
        }
        Self::new(candidate.base_url, candidate.token, runtime_timeout).ok()
    }

    async fn health_check(&self) -> TranscriptionResult<()> {
        let response = self
            .http
            .get(health_url(&self.base_url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| {
                TranscriptionError::Request(format!("local transcriber health failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(TranscriptionError::Request(format!(
                "local transcriber health returned HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl TranscriptionClient for LocalTranscriberClient {
    async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
        let file_part = multipart::Part::file(&recording.path)
            .await
            .map_err(|error| {
                TranscriptionError::Request(format!(
                    "failed to open {}: {error}",
                    recording.path.display()
                ))
            })?;
        let file_part = file_part
            .file_name(recording.filename.clone())
            .mime_str(&recording.content_type)
            .map_err(|error| {
                TranscriptionError::Request(format!("invalid multipart mime: {error}"))
            })?;
        let form = multipart::Form::new()
            .part("file", file_part)
            .text("model", "whisper-1");
        let response = self
            .http
            .post(transcription_url(&self.base_url))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .map_err(|error| TranscriptionError::Request(error.to_string()))?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            TranscriptionError::Request(format!(
                "failed to read local transcriber response: {error}"
            ))
        })?;
        if !status.is_success() {
            return Err(TranscriptionError::Service {
                status: status.as_u16(),
                message: redact_error(&body),
            });
        }
        parse_openai_transcription_response(&body)
    }
}

#[derive(Clone)]
struct ServiceState {
    backend: Arc<dyn TranscriptionClient>,
    speech: Option<Arc<dyn SpeechClient>>,
    auth: ServiceAuth,
    codex_upload_limit_bytes: u64,
    client_upload_limit_bytes: u64,
    chunk_seconds: u64,
    ffmpeg_binary: String,
}

#[derive(Clone)]
struct ServiceAuth {
    token: String,
    no_auth: bool,
}

fn service_router(state: ServiceState) -> Router {
    let transcription_body_limit = usize::try_from(
        state
            .client_upload_limit_bytes
            .saturating_add(MULTIPART_OVERHEAD_BYTES),
    )
    .unwrap_or(usize::MAX);
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/healthz", get(health))
        .route("/audio/transcriptions", post(transcribe))
        .route("/v1/audio/transcriptions", post(transcribe))
        .layer(DefaultBodyLimit::max(transcription_body_limit))
        .route(
            "/audio/speech",
            post(speech).layer(DefaultBodyLimit::max(SPEECH_BODY_LIMIT_BYTES)),
        )
        .route(
            "/v1/audio/speech",
            post(speech).layer(DefaultBodyLimit::max(SPEECH_BODY_LIMIT_BYTES)),
        )
        .with_state(state)
}

async fn health(
    State(state): State<ServiceState>,
    headers: HeaderMap,
) -> Result<Json<Health>, ApiError> {
    authorize(&headers, &state.auth)?;
    let capabilities = ServiceCapabilities {
        transcriptions: true,
        speech: state.speech.is_some(),
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
    let upload = read_upload(multipart, state.client_upload_limit_bytes).await?;
    let text = transcribe_upload(&state, &upload).await?;
    Ok(match upload.response_format {
        ResponseFormat::Json => Json(TranscriptionResponse { text }).into_response(),
        ResponseFormat::Text => {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
        }
    })
}

async fn read_upload(
    mut multipart: Multipart,
    client_upload_limit_bytes: u64,
) -> Result<Upload, ApiError> {
    let mut upload = None;
    let mut response_format = ResponseFormat::Json;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("failed to read multipart form: {error}")))?
    {
        match field.name() {
            Some("file") => {
                if upload.is_some() {
                    return Err(ApiError::bad_request(
                        "multipart form included more than one file field",
                    ));
                }
                upload = Some(read_file_field(field, client_upload_limit_bytes).await?);
            }
            Some("response_format") => {
                response_format = read_response_format_field(&mut field).await?;
            }
            _ => {}
        }
    }

    let mut upload = upload
        .ok_or_else(|| ApiError::bad_request("multipart form did not include a file field"))?;
    upload.response_format = response_format;
    Ok(upload)
}

async fn read_file_field(
    mut field: axum::extract::multipart::Field<'_>,
    client_upload_limit_bytes: u64,
) -> Result<Upload, ApiError> {
    let filename = field
        .file_name()
        .map(sanitize_filename)
        .unwrap_or_else(|| "audio.wav".to_string());
    let content_type = field
        .content_type()
        .map(ToString::to_string)
        .unwrap_or_else(|| source_content_type(Path::new(&filename)).to_string());
    let mut temp = NamedTempFile::new()
        .map_err(|error| ApiError::internal(format!("failed to create temp upload: {error}")))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
    let write_task = tokio::task::spawn_blocking(move || {
        while let Some(chunk) = rx.blocking_recv() {
            temp.write_all(&chunk).map_err(|error| {
                ApiError::internal(format!("failed to write temp upload: {error}"))
            })?;
        }
        Ok::<_, ApiError>(temp)
    });
    let mut bytes = 0_u64;
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        let message = error.to_string();
        if message.contains("length limit") || message.contains("Payload Too Large") {
            ApiError::payload_too_large(format!("failed to read upload chunk: {message}"))
        } else {
            ApiError::bad_request(format!("failed to read upload chunk: {message}"))
        }
    })? {
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > client_upload_limit_bytes {
            drop(tx);
            let _ = write_task.await;
            return Err(ApiError::payload_too_large(format!(
                "upload exceeds client limit of {client_upload_limit_bytes} bytes"
            )));
        }
        tx.send(chunk.to_vec())
            .await
            .map_err(|error| ApiError::internal(format!("temp write channel closed: {error}")))?;
    }
    drop(tx);
    let temp = write_task
        .await
        .map_err(|error| ApiError::internal(format!("temp write task panicked: {error}")))??;
    Ok(Upload {
        temp,
        filename,
        content_type,
        bytes,
        response_format: ResponseFormat::Json,
    })
}

async fn read_response_format_field(
    field: &mut axum::extract::multipart::Field<'_>,
) -> Result<ResponseFormat, ApiError> {
    const MAX_RESPONSE_FORMAT_BYTES: usize = 64;

    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        ApiError::bad_request(format!("failed to read response_format field: {error}"))
    })? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_FORMAT_BYTES {
            return Err(ApiError::bad_request("response_format field is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let value = String::from_utf8(bytes)
        .map_err(|error| ApiError::bad_request(format!("response_format is not UTF-8: {error}")))?;
    parse_response_format(&value)
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
}

async fn speech(State(state): State<ServiceState>, request: Request) -> Result<Response, ApiError> {
    authorize(request.headers(), &state.auth)?;

    let speech_client = state
        .speech
        .as_ref()
        .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))?;

    let Json(body) = Json::<OpenAiSpeechRequest>::from_request(request, &state)
        .await
        .map_err(ApiError::json_rejection)?;

    let voice = body
        .voice
        .filter(|voice| !voice.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("voice is required"))?;

    let format = match body.response_format.as_deref() {
        None | Some("") => SpeechFormat::Mp3,
        Some(s) => SpeechFormat::from_openai(s)
            .ok_or_else(|| ApiError::bad_request(format!("unsupported response_format: {s:?}; supported values are mp3, opus, aac, flac, wav, pcm")))?,
    };

    let request = SpeechRequest {
        input: body.input,
        model_hint: body.model,
        voice_hint: Some(voice),
        instructions: body.instructions,
        format,
        speed: body.speed,
    };

    let synthesized = speech_client
        .synthesize(&request)
        .await
        .map_err(|error| match &error {
            codex_voice_core::SpeechError::Unsupported(msg) => ApiError::bad_request(msg.clone()),
            codex_voice_core::SpeechError::Config(msg) => ApiError::bad_request(msg.clone()),
            codex_voice_core::SpeechError::Auth(msg) => ApiError::service_unavailable(msg.clone()),
            _ => ApiError::backend(format!("{error}")),
        })?;

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, synthesized.mime_type.clone());

    // Optional informational headers
    response = response.header("X-Codex-Voice-Format", synthesized.format.to_openai());

    response
        .body(axum::body::Body::from(synthesized.bytes))
        .map_err(|error| ApiError::internal(format!("failed to build response: {error}")))
}

async fn transcribe_upload(state: &ServiceState, upload: &Upload) -> Result<String, ApiError> {
    if upload.bytes <= state.codex_upload_limit_bytes {
        return transcribe_path(
            state.backend.as_ref(),
            upload.temp.path(),
            &upload.filename,
            &upload.content_type,
        )
        .await;
    }

    if !ffmpeg_available(&state.ffmpeg_binary).await {
        return Err(ApiError::payload_too_large(format!(
            "audio is {} bytes, above the Codex per-request limit of {} bytes; install ffmpeg or send smaller chunks",
            upload.bytes, state.codex_upload_limit_bytes
        )));
    }

    let chunk_seconds =
        effective_chunk_seconds(state.chunk_seconds, state.codex_upload_limit_bytes);
    let max_seconds_from_bytes = state.client_upload_limit_bytes / PCM_BYTES_PER_SECOND;
    let max_seconds_from_chunks = MAX_GENERATED_CHUNKS as u64 * chunk_seconds;
    let max_duration_seconds = max_seconds_from_bytes.min(max_seconds_from_chunks).max(1);

    match input_duration_seconds(&ffprobe_binary(&state.ffmpeg_binary), upload.temp.path()).await {
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

    let chunks = split_audio_with_ffmpeg(
        &state.ffmpeg_binary,
        upload.temp.path(),
        chunk_seconds,
        None,
    )
    .await
    .map_err(|error| ApiError::internal(format!("failed to split oversized audio: {error:#}")))?;
    validate_generated_chunks(
        &chunks.paths,
        state.client_upload_limit_bytes,
        state.codex_upload_limit_bytes,
    )?;
    let mut transcripts = Vec::with_capacity(chunks.paths.len());
    for path in &chunks.paths {
        let filename = filename_for_path(path);
        transcripts
            .push(transcribe_path(state.backend.as_ref(), path, &filename, "audio/wav").await?);
    }
    Ok(join_transcripts(&transcripts))
}

async fn transcribe_path(
    backend: &dyn TranscriptionClient,
    path: &Path,
    filename: &str,
    content_type: &str,
) -> Result<String, ApiError> {
    let recording = RecordedAudio {
        path: path.to_path_buf(),
        content_type: content_type.to_string(),
        filename: filename.to_string(),
        duration: Duration::default(),
    };
    backend
        .transcribe(&recording)
        .await
        .map_err(|error| ApiError::backend(error.to_string()))
}

struct Upload {
    temp: NamedTempFile,
    filename: String,
    content_type: String,
    bytes: u64,
    response_format: ResponseFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseFormat {
    Json,
    Text,
}

struct ChunkedAudio {
    _dir: TempDir,
    paths: Vec<PathBuf>,
}

/// Derives the ffprobe path from the ffmpeg path by replacing "ffmpeg" with "ffprobe".
/// This assumes ffmpeg and ffprobe are co-located with matching names (the common case).
/// Custom ffmpeg binary names that do not contain the literal substring "ffmpeg" will not
/// auto-resolve and should be paired with an explicit ffprobe path if needed.
fn ffprobe_binary(ffmpeg_binary: &str) -> String {
    ffmpeg_binary.replace("ffmpeg", "ffprobe")
}

async fn ffmpeg_available(binary: &str) -> bool {
    let mut command = TokioCommand::new(binary);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    match time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            false
        }
    }
}

async fn input_duration_seconds(binary: &str, input: &Path) -> Result<Option<f64>> {
    let mut command = TokioCommand::new(binary);
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {binary}"))?;
    let status = match time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => anyhow::bail!("failed to wait for ffprobe: {error}"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("ffprobe timed out after 5s");
        }
    };
    if !status.success() {
        return Ok(None);
    }
    let stdout = child.stdout.take().context("ffprobe stdout unavailable")?;
    let mut buffer = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::BufReader::new(stdout), &mut buffer)
        .await
        .context("failed to read ffprobe output")?;
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<f64>()
        .map(Some)
        .context("ffprobe output is not a valid duration")
}

async fn split_audio_with_ffmpeg(
    binary: &str,
    input: &Path,
    chunk_seconds: u64,
    max_duration_seconds: Option<u64>,
) -> Result<ChunkedAudio> {
    let dir = TempDir::new().context("failed to create chunk temp dir")?;
    let pattern = dir.path().join("chunk-%04d.wav");
    let segment_time = chunk_seconds.max(1).to_string();
    let duration_limit = max_duration_seconds.map(|seconds| seconds.max(1).to_string());

    let mut command = TokioCommand::new(binary);
    command
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input);
    if let Some(duration_limit) = duration_limit.as_deref() {
        command.args(["-t", duration_limit]);
    }
    command
        .args([
            "-vn",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            "-f",
            "segment",
            "-segment_time",
            &segment_time,
            "-reset_timestamps",
            "1",
        ])
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {binary}"))?;
    let status = match time::timeout(FFMPEG_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => anyhow::bail!("failed to wait for ffmpeg: {error}"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("ffmpeg timed out after {}s", FFMPEG_TIMEOUT.as_secs());
        }
    };
    if !status.success() {
        anyhow::bail!("ffmpeg failed with status {status}");
    }
    let mut paths = fs::read_dir(dir.path())
        .context("failed to list generated chunks")?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("wav"))
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        anyhow::bail!("ffmpeg did not produce any audio chunks");
    }
    Ok(ChunkedAudio { _dir: dir, paths })
}

fn validate_generated_chunks(
    paths: &[PathBuf],
    max_decoded_bytes: u64,
    codex_upload_limit_bytes: u64,
) -> Result<(), ApiError> {
    if paths.len() > MAX_GENERATED_CHUNKS {
        return Err(ApiError::payload_too_large(format!(
            "audio produced {} chunks, above the service limit of {}; send smaller chunks",
            paths.len(),
            MAX_GENERATED_CHUNKS
        )));
    }

    let mut total_bytes = 0_u64;
    for (index, path) in paths.iter().enumerate() {
        let bytes = fs::metadata(path)
            .map(|metadata| metadata.len())
            .map_err(|error| {
                ApiError::internal(format!("failed to stat generated chunk {index}: {error}"))
            })?;
        if bytes > codex_upload_limit_bytes {
            return Err(ApiError::payload_too_large(format!(
                "generated chunk {index} is {bytes} bytes, above configured Codex limit of {codex_upload_limit_bytes} bytes"
            )));
        }
        total_bytes = total_bytes.saturating_add(bytes);
        if total_bytes > max_decoded_bytes {
            return Err(ApiError::payload_too_large(format!(
                "decoded audio is {total_bytes} bytes, above the service decoded-output limit of {max_decoded_bytes} bytes; send smaller chunks"
            )));
        }
    }
    Ok(())
}

fn join_transcripts(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn effective_chunk_seconds(requested_seconds: u64, upload_limit_bytes: u64) -> u64 {
    let pcm_bytes_per_second = 16_000_u64 * 2;
    let reserve = 1_024 * 1_024;
    let usable = upload_limit_bytes
        .saturating_sub(reserve)
        .max(pcm_bytes_per_second);
    let max_seconds = (usable / pcm_bytes_per_second).max(1);
    requested_seconds.max(1).min(max_seconds)
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
struct ApiError {
    status: StatusCode,
    kind: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            kind: "bad_request",
            message: message.into(),
        }
    }

    fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            kind: "payload_too_large",
            message: message.into(),
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            kind: "unauthorized",
            message: "missing or invalid bearer token".into(),
        }
    }

    fn backend(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            kind: "backend_error",
            message: redact_error(&message.into()),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal_error",
            message: message.into(),
        }
    }

    fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            kind: "service_unavailable",
            message: message.into(),
        }
    }

    fn json_rejection(error: axum::extract::rejection::JsonRejection) -> Self {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriberDiscoveryFile {
    pub url: String,
    pub openai_base_url: String,
    pub token: String,
    pub pid: u32,
    #[serde(default)]
    pub capabilities: ServiceCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceCapabilities {
    pub transcriptions: bool,
    pub speech: bool,
}

impl TranscriberDiscoveryFile {
    fn new(root_url: String, token: String, capabilities: ServiceCapabilities) -> Self {
        Self {
            openai_base_url: format!("{}/v1", root_url.trim_end_matches('/')),
            url: root_url,
            token,
            pid: std::process::id(),
            capabilities,
        }
    }
}

#[derive(Debug, Clone)]
struct DiscoveryCandidate {
    base_url: String,
    token: String,
}

fn resolve_discovery_candidate() -> Option<DiscoveryCandidate> {
    discovery_candidate_from_parts(
        env::var(URL_ENV).ok(),
        env::var(TOKEN_ENV).ok(),
        read_discovery_file(),
        pid_is_running,
    )
}

fn discovery_candidate_from_parts(
    env_url: Option<String>,
    env_token: Option<String>,
    discovery: Option<TranscriberDiscoveryFile>,
    pid_alive: impl Fn(u32) -> bool,
) -> Option<DiscoveryCandidate> {
    if let Some(url) = env_url
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
    {
        let token = env_token.and_then(normalize_token).or_else(|| {
            let file = discovery.as_ref()?;
            if discovery_url_matches(&url, file) {
                normalize_token(file.token.clone())
            } else {
                None
            }
        })?;
        return Some(DiscoveryCandidate {
            base_url: url,
            token,
        });
    }

    let discovery = discovery?;
    if !pid_alive(discovery.pid) {
        return None;
    }
    let token = normalize_token(discovery.token)?;
    Some(DiscoveryCandidate {
        base_url: discovery.url,
        token,
    })
}

fn discovery_url_matches(url: &str, discovery: &TranscriberDiscoveryFile) -> bool {
    normalize_loopback(root_url(url)) == normalize_loopback(root_url(&discovery.url))
        || normalize_loopback(root_url(url))
            == normalize_loopback(root_url(&discovery.openai_base_url))
}

fn normalize_loopback(url: String) -> String {
    url.to_lowercase()
        .replace("localhost", "127.0.0.1")
        .replace("[::1]", "127.0.0.1")
}

pub fn discovery_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(env::temp_dir)
        .join("codex-voice")
        .join("transcriber.json")
}

fn read_discovery_file() -> Option<TranscriberDiscoveryFile> {
    let text = fs::read_to_string(discovery_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_discovery_file(discovery: &TranscriberDiscoveryFile) -> Result<()> {
    let path = discovery_path();
    let parent = path
        .parent()
        .context("transcriber discovery path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    set_owner_only_directory_permissions(parent)
        .with_context(|| format!("failed to restrict {}", parent.display()))?;
    let tmp_path = path.with_extension(format!(
        "json.{}.tmp",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let text = serde_json::to_string_pretty(discovery)?;
    if let Err(error) = write_private_file(&tmp_path, text.as_bytes()) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error).with_context(|| format!("failed to write {}", tmp_path.display()));
    }
    if let Err(error) = fs::rename(&tmp_path, &path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error).with_context(|| {
            format!(
                "failed to move {} to {}",
                tmp_path.display(),
                path.display()
            )
        });
    }
    set_owner_only_file_permissions(&path)
        .with_context(|| format!("failed to restrict {}", path.display()))?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        set_owner_only_file_permissions(path)?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        fs::write(path, bytes)?;
        Ok(())
    }
}

fn set_owner_only_directory_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }

    let _ = path;
    Ok(())
}

fn set_owner_only_file_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    let _ = path;
    Ok(())
}

fn remove_discovery_file_if_current(discovery: &TranscriberDiscoveryFile) {
    let Some(current) = read_discovery_file() else {
        return;
    };
    if current.pid == discovery.pid && current.token == discovery.token {
        let _ = fs::remove_file(discovery_path());
    }
}

fn pid_is_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

fn resolve_or_generate_token(env_key: &str) -> String {
    env::var(env_key)
        .ok()
        .and_then(normalize_token)
        .unwrap_or_else(|| hex::encode(rand::random::<[u8; 32]>()))
}

fn normalize_token(token: String) -> Option<String> {
    let token = token.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

fn service_root_url(addr: SocketAddr) -> String {
    let host = match addr.ip() {
        IpAddr::V4(ip) if ip == Ipv4Addr::UNSPECIFIED => "127.0.0.1".to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
        IpAddr::V4(ip) => ip.to_string(),
    };
    format!("http://{host}:{}", addr.port())
}

fn root_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    for suffix in [
        "/v1/audio/transcriptions",
        "/audio/transcriptions",
        "/v1/audio/speech",
        "/audio/speech",
        "/v1",
    ] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return stripped.trim_end_matches('/').to_string();
        }
    }
    trimmed.to_string()
}

fn health_url(base_url: &str) -> String {
    format!("{}/healthz", root_url(base_url))
}

fn transcription_url(base_url: &str) -> String {
    format!("{}/v1/audio/transcriptions", root_url(base_url))
}

#[allow(dead_code)]
fn speech_url(base_url: &str) -> String {
    format!("{}/v1/audio/speech", root_url(base_url))
}

fn parse_openai_transcription_response(body: &str) -> TranscriptionResult<String> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|error| TranscriptionError::Request(format!("invalid JSON response: {error}")))?;
    let text = value
        .get("text")
        .and_then(|value| value.as_str())
        .ok_or_else(|| TranscriptionError::Request("response JSON did not include text".into()))?;
    Ok(text.to_string())
}

fn parse_response_format(value: &str) -> Result<ResponseFormat, ApiError> {
    match value.trim() {
        "" | "json" => Ok(ResponseFormat::Json),
        "text" => Ok(ResponseFormat::Text),
        other => Err(ApiError::bad_request(format!(
            "unsupported response_format {other:?}; supported values are json and text"
        ))),
    }
}

fn sanitize_filename(name: &str) -> String {
    let name = name.replace(['/', '\\'], "_");
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "audio.wav".to_string()
    } else {
        trimmed.to_string()
    }
}

fn filename_for_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_filename)
        .unwrap_or_else(|| "audio.wav".to_string())
}

fn source_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("mp3") || ext.eq_ignore_ascii_case("mpga") => {
            "audio/mpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("m4a") || ext.eq_ignore_ascii_case("mp4") => {
            "audio/mp4"
        }
        Some(ext) if ext.eq_ignore_ascii_case("webm") => "audio/webm",
        Some(ext) if ext.eq_ignore_ascii_case("ogg") || ext.eq_ignore_ascii_case("oga") => {
            "audio/ogg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(ext) if ext.eq_ignore_ascii_case("wav") || ext.eq_ignore_ascii_case("wave") => {
            "audio/wav"
        }
        _ => "application/octet-stream",
    }
}

fn redact_error(error: &str) -> String {
    let single_line = error.replace(['\n', '\r'], " ");
    let mut result = single_line
        .replace("access_token", "access_token(redacted)")
        .replace("Authorization", "Authorization(redacted)");
    result = codex_voice_core::redact_bearer_tokens(&result);
    result = codex_voice_core::redact_jwts(&result);
    if result.chars().count() > 500 {
        format!("{}...", result.chars().take(500).collect::<String>())
    } else {
        result
    }
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
    use axum::body;
    use codex_voice_core::TranscriptionResult;
    use std::sync::Mutex;
    use tower::ServiceExt;

    #[derive(Default)]
    struct FakeBackend {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TranscriptionClient for FakeBackend {
        async fn transcribe(&self, recording: &RecordedAudio) -> TranscriptionResult<String> {
            self.seen
                .lock()
                .expect("fake backend lock")
                .push(recording.filename.clone());
            Ok("hello from service".into())
        }
    }

    #[derive(Default)]
    struct FakeSpeechBackend {
        seen: Mutex<Vec<codex_voice_core::SpeechRequest>>,
    }

    #[async_trait]
    impl SpeechClient for FakeSpeechBackend {
        async fn synthesize(
            &self,
            request: &codex_voice_core::SpeechRequest,
        ) -> codex_voice_core::SpeechResult<codex_voice_core::SynthesizedSpeech> {
            self.seen
                .lock()
                .expect("fake speech lock")
                .push(request.clone());
            Ok(codex_voice_core::SynthesizedSpeech {
                bytes: b"fake audio bytes".to_vec(),
                format: request.format,
                mime_type: request.format.mime_type().to_string(),
            })
        }
    }

    fn test_state(codex_upload_limit_bytes: u64) -> ServiceState {
        test_state_with_speech_backend(codex_upload_limit_bytes, None)
    }

    fn test_state_with_speech(codex_upload_limit_bytes: u64) -> ServiceState {
        test_state_with_speech_backend(
            codex_upload_limit_bytes,
            Some(Arc::new(FakeSpeechBackend::default())),
        )
    }

    fn test_state_with_speech_backend(
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

    fn speech_request(
        path: &str,
        body: &str,
        token: Option<&str>,
    ) -> axum::http::Request<body::Body> {
        let mut builder = axum::http::Request::builder()
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

    fn multipart_request(
        path: &str,
        body: &str,
        token: Option<&str>,
    ) -> axum::http::Request<body::Body> {
        let boundary = "codex-voice-test";
        let payload = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"input.wav\"\r\nContent-Type: audio/wav\r\n\r\n{body}\r\n--{boundary}--\r\n"
        );
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header(
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

    fn multipart_request_with_response_format(
        path: &str,
        response_format: &str,
        token: Option<&str>,
    ) -> axum::http::Request<body::Body> {
        let boundary = "codex-voice-test";
        let payload = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"input.wav\"\r\nContent-Type: audio/wav\r\n\r\ntiny wav\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\n{response_format}\r\n--{boundary}--\r\n"
        );
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header(
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
    async fn speech_route_requires_voice() {
        let app = service_router(test_state_with_speech(1024));
        let response = app
            .oneshot(speech_request(
                "/v1/audio/speech",
                r#"{"model":"gpt-4o-mini-tts","input":"hello"}"#,
                Some("test-token"),
            ))
            .await
            .expect("request succeeds");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
    fn joins_transcripts_in_order() {
        let joined = join_transcripts(&[
            " first ".to_string(),
            "".to_string(),
            "second".to_string(),
            " third\n".to_string(),
        ]);
        assert_eq!(joined, "first\n\nsecond\n\nthird");
    }

    #[test]
    fn chunk_seconds_respect_upload_limit() {
        assert_eq!(effective_chunk_seconds(600, 24 * 1024 * 1024), 600);
        assert!(effective_chunk_seconds(600, 2 * 1024 * 1024) < 600);
        assert_eq!(effective_chunk_seconds(0, 24 * 1024 * 1024), 1);
    }

    #[test]
    fn normalizes_service_urls() {
        assert_eq!(
            health_url("http://127.0.0.1:3845/v1"),
            "http://127.0.0.1:3845/healthz"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:3845"),
            "http://127.0.0.1:3845/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:3845/v1"),
            "http://127.0.0.1:3845/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_url("http://127.0.0.1:3845/v1/audio/transcriptions"),
            "http://127.0.0.1:3845/v1/audio/transcriptions"
        );
        assert_eq!(
            speech_url("http://127.0.0.1:3845"),
            "http://127.0.0.1:3845/v1/audio/speech"
        );
        assert_eq!(
            speech_url("http://127.0.0.1:3845/v1/audio/speech"),
            "http://127.0.0.1:3845/v1/audio/speech"
        );
        assert_eq!(
            root_url("http://127.0.0.1:3845/v1/audio/transcriptions"),
            "http://127.0.0.1:3845"
        );
    }

    #[test]
    fn stale_discovery_is_ignored_without_env_override() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_candidate_from_parts(None, None, Some(discovery), |_| false).is_none());
    }

    #[test]
    fn env_url_can_reuse_discovery_token_for_matching_service() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        let candidate = discovery_candidate_from_parts(
            Some("http://127.0.0.1:3845/v1".into()),
            None,
            Some(discovery),
            |_| false,
        )
        .expect("env URL with file token resolves");
        assert_eq!(candidate.base_url, "http://127.0.0.1:3845/v1");
        assert_eq!(candidate.token, "from-file");
    }

    #[test]
    fn env_url_requires_explicit_token_for_different_service() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_candidate_from_parts(
            Some("http://127.0.0.1:9999/v1".into()),
            None,
            Some(discovery),
            |_| true,
        )
        .is_none());
    }

    #[test]
    fn env_url_uses_explicit_token_for_different_service() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        let candidate = discovery_candidate_from_parts(
            Some("http://127.0.0.1:9999/v1".into()),
            Some("from-env".into()),
            Some(discovery),
            |_| true,
        )
        .expect("explicit token resolves");
        assert_eq!(candidate.token, "from-env");
    }

    #[test]
    fn discovery_tokens_are_trimmed() {
        assert_eq!(
            normalize_token("  test-token\n".to_string()).as_deref(),
            Some("test-token")
        );
        assert!(normalize_token(" \n\t ".to_string()).is_none());
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

    #[cfg(unix)]
    #[test]
    fn private_file_writes_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("secret.json");

        write_private_file(&path, b"secret").expect("private file write");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn generated_chunk_limits_reject_decoded_growth() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = dir.path().join("chunk-0000.wav");
        let second = dir.path().join("chunk-0001.wav");
        fs::write(&first, [0_u8; 8]).expect("first chunk");
        fs::write(&second, [0_u8; 8]).expect("second chunk");

        let error = validate_generated_chunks(&[first, second], 12, 16).expect_err("limit rejects");

        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error.kind, "payload_too_large");
    }

    #[test]
    fn generated_chunk_limits_reject_many_chunks() {
        let paths = (0..=MAX_GENERATED_CHUNKS)
            .map(|index| PathBuf::from(format!("chunk-{index:04}.wav")))
            .collect::<Vec<_>>();

        let error = validate_generated_chunks(&paths, 1024, 1024).expect_err("chunk count rejects");

        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error.kind, "payload_too_large");
    }

    #[test]
    fn localhost_matches_discovery_127_0_0_1() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_url_matches("http://localhost:3845", &discovery));
    }

    #[test]
    fn ipv6_loopback_matches_discovery_127_0_0_1() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_url_matches("http://[::1]:3845", &discovery));
    }

    #[test]
    fn mixed_case_localhost_matches_discovery() {
        let discovery = TranscriberDiscoveryFile {
            url: "http://127.0.0.1:3845".into(),
            openai_base_url: "http://127.0.0.1:3845/v1".into(),
            token: "from-file".into(),
            pid: 42,
            capabilities: ServiceCapabilities::default(),
        };
        assert!(discovery_url_matches("http://LOCALHOST:3845", &discovery));
        assert!(discovery_url_matches("http://Localhost:3845", &discovery));
    }
}
