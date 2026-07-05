use anyhow::{Context, Result};
use axum::{
    extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use bytes::Bytes;
use codex_voice_codex::{CodexAuthService, CodexTranscriptionClient};
use codex_voice_core::{SpeechClient, SpeechFormat, SpeechRequest, TranscriptionClient};

use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
const WEB_MANIFEST: &str = r##"{
  "name": "Codex Voice",
  "short_name": "Voice",
  "description": "Quick text-to-speech for Codex Voice.",
  "id": "/web",
  "start_url": "/web",
  "scope": "/web",
  "display": "standalone",
  "background_color": "#101214",
  "theme_color": "#101214",
  "icons": [
    {
      "src": "/web/icon-192.png",
      "sizes": "192x192",
      "type": "image/png",
      "purpose": "any"
    },
    {
      "src": "/web/icon-512.png",
      "sizes": "512x512",
      "type": "image/png",
      "purpose": "any"
    },
    {
      "src": "/web/icon-maskable-512.png",
      "sizes": "512x512",
      "type": "image/png",
      "purpose": "maskable"
    }
  ]
}"##;
const WEB_SW_JS: &str = r#"const CACHE_NAME = 'codex-voice-web-v2';
const SHELL_ASSETS = [
  '/web',
  '/web/manifest.webmanifest',
  '/web/icon-192.png',
  '/web/icon-512.png',
  '/web/icon-maskable-512.png',
  '/web/apple-touch-icon.png'
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME)
      .then((cache) => cache.addAll(SHELL_ASSETS))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys()
      .then((names) => Promise.all(
        names.filter((name) => name !== CACHE_NAME).map((name) => caches.delete(name))
      ))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  if (request.mode === 'navigate' && url.pathname === '/web') {
    event.respondWith(
      fetch(request)
        .then((response) => {
          const copy = response.clone();
          caches.open(CACHE_NAME).then((cache) => cache.put('/web', copy));
          return response;
        })
        .catch(() => caches.match('/web'))
    );
    return;
  }

  if (SHELL_ASSETS.includes(url.pathname)) {
    event.respondWith(
      caches.match(request)
        .then((cached) => cached || fetch(request))
    );
  }
});
"#;
const WEB_APP_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
  <meta name="theme-color" content="#101214">
  <meta name="mobile-web-app-capable" content="yes">
  <meta name="apple-mobile-web-app-capable" content="yes">
  <meta name="apple-mobile-web-app-title" content="Codex Voice">
  <meta name="apple-mobile-web-app-status-bar-style" content="black-translucent">
  <link rel="manifest" href="/web/manifest.webmanifest">
  <link rel="icon" type="image/png" sizes="192x192" href="/web/icon-192.png">
  <link rel="icon" type="image/png" sizes="512x512" href="/web/icon-512.png">
  <link rel="apple-touch-icon" href="/web/apple-touch-icon.png">
  <title>Codex Voice</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #101214;
      --panel: #191d21;
      --text: #f2f5f7;
      --muted: #a8b0b8;
      --line: #30363d;
      --accent: #5dc7b7;
      --accent-strong: #78e0d0;
      --danger: #ff8f8f;
    }
    * { box-sizing: border-box; }
    html, body { min-height: 100%; }
    body {
      margin: 0;
      background: var(--bg);
      color: var(--text);
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      letter-spacing: 0;
    }
    main {
      min-height: 100dvh;
      display: grid;
      grid-template-rows: auto minmax(280px, 1fr) auto;
      gap: 14px;
      padding: max(18px, env(safe-area-inset-top)) 16px max(18px, env(safe-area-inset-bottom));
      max-width: 760px;
      margin: 0 auto;
    }
    header {
      display: flex;
      align-items: end;
      justify-content: space-between;
      gap: 14px;
    }
    h1 {
      margin: 0;
      font-size: 1.35rem;
      font-weight: 700;
    }
    #count {
      color: var(--muted);
      font-size: 0.92rem;
      white-space: nowrap;
    }
    textarea {
      width: 100%;
      min-height: 0;
      resize: none;
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 16px;
      background: var(--panel);
      color: var(--text);
      font: inherit;
      font-size: 1.08rem;
      line-height: 1.45;
      outline: none;
    }
    textarea:focus {
      border-color: var(--accent);
      box-shadow: 0 0 0 3px rgba(93, 199, 183, 0.18);
    }
    .controls {
      display: grid;
      gap: 14px;
    }
    .buttons {
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 10px;
    }
    button {
      min-height: 54px;
      border: 0;
      border-radius: 8px;
      color: #081110;
      background: var(--accent);
      font: inherit;
      font-weight: 700;
      cursor: pointer;
      touch-action: manipulation;
    }
    button.secondary {
      color: var(--text);
      background: #252b31;
      border: 1px solid var(--line);
    }
    button:disabled {
      cursor: not-allowed;
      opacity: 0.55;
    }
    input[type="range"] {
      width: 100%;
      accent-color: var(--accent-strong);
    }
    .time {
      display: flex;
      justify-content: space-between;
      color: var(--muted);
      font-variant-numeric: tabular-nums;
      font-size: 0.95rem;
    }
    #status {
      min-height: 1.4em;
      color: var(--muted);
      font-size: 0.98rem;
    }
    #status.error { color: var(--danger); }
    @media (max-width: 420px) {
      main { padding-left: 12px; padding-right: 12px; }
      .buttons { grid-template-columns: 1fr; }
    }
  </style>
</head>
<body>
  <main>
    <header>
      <h1>Codex Voice</h1>
      <span id="count">0 chars</span>
    </header>
    <textarea id="text" autocomplete="off" autocapitalize="sentences" spellcheck="true" placeholder="Type something to hear it spoken..."></textarea>
    <section class="controls">
      <div class="buttons">
        <button id="generate" type="button">Generate</button>
        <button id="play" type="button" class="secondary" disabled>Play</button>
      </div>
      <input id="seek" type="range" min="0" max="1000" value="0" disabled aria-label="Audio position">
      <div class="time">
        <span id="elapsed">0:00</span>
        <span id="duration">0:00</span>
      </div>
      <div id="status" role="status" aria-live="polite">Ready</div>
    </section>
  </main>
  <script>
    const text = document.getElementById('text');
    const generate = document.getElementById('generate');
    const play = document.getElementById('play');
    const seek = document.getElementById('seek');
    const elapsed = document.getElementById('elapsed');
    const duration = document.getElementById('duration');
    const status = document.getElementById('status');
    const count = document.getElementById('count');
    const storageKey = 'codex-voice.web.text';
    let audio = new Audio();
    let objectUrl = null;
    let seeking = false;

    if ('serviceWorker' in navigator) {
      window.addEventListener('load', () => {
        navigator.serviceWorker.register('/web-sw.js', { scope: '/web' }).catch(() => {});
      });
    }

    text.value = localStorage.getItem(storageKey) || '';
    updateCount();

    function setStatus(message, isError = false) {
      status.textContent = message;
      status.classList.toggle('error', isError);
    }

    function formatTime(seconds) {
      if (!Number.isFinite(seconds) || seconds <= 0) return '0:00';
      const whole = Math.floor(seconds);
      const minutes = Math.floor(whole / 60);
      return `${minutes}:${String(whole % 60).padStart(2, '0')}`;
    }

    function updateCount() {
      const chars = Array.from(text.value).length;
      count.textContent = `${chars} ${chars === 1 ? 'char' : 'chars'}`;
    }

    function updatePosition() {
      const total = audio.duration || 0;
      if (!seeking && total > 0) {
        seek.value = Math.round((audio.currentTime / total) * 1000);
      }
      elapsed.textContent = formatTime(audio.currentTime);
      duration.textContent = formatTime(total);
    }

    function resetAudio() {
      audio.pause();
      audio.removeAttribute('src');
      audio.load();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
      objectUrl = null;
      play.disabled = true;
      play.textContent = 'Play';
      seek.disabled = true;
      seek.value = 0;
      elapsed.textContent = '0:00';
      duration.textContent = '0:00';
    }

    function audioBlobFromBase64(base64Audio, mimeType) {
      const binary = atob(base64Audio);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) {
        bytes[i] = binary.charCodeAt(i);
      }
      return new Blob([bytes], { type: mimeType || 'audio/wav' });
    }

    text.addEventListener('input', () => {
      localStorage.setItem(storageKey, text.value);
      updateCount();
    });

    generate.addEventListener('click', async () => {
      const input = text.value.trim();
      if (!input) {
        setStatus('Enter some text first.', true);
        return;
      }
      generate.disabled = true;
      play.disabled = true;
      setStatus('Generating...');
      try {
        const response = await fetch('/web/speech', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ input })
        });
        if (!response.ok) {
          let message = `TTS failed (${response.status})`;
          try {
            const json = await response.json();
            message = json?.error?.message || message;
          } catch (_) {}
          throw new Error(message);
        }
        resetAudio();
        const result = await response.json();
        if (typeof result.input === 'string' && result.input !== text.value) {
          text.value = result.input;
          localStorage.setItem(storageKey, text.value);
          updateCount();
        }
        const blob = audioBlobFromBase64(result.audio_base64, result.mime_type);
        objectUrl = URL.createObjectURL(blob);
        audio.src = objectUrl;
        audio.load();
        play.disabled = false;
        seek.disabled = false;
        setStatus(result.input_changed ? 'Ready to play. Tags added.' : 'Ready to play.');
      } catch (error) {
        resetAudio();
        setStatus(error.message || 'TTS failed.', true);
      } finally {
        generate.disabled = false;
      }
    });

    play.addEventListener('click', async () => {
      if (!audio.src) return;
      if (audio.paused) {
        try {
          await audio.play();
        } catch (error) {
          setStatus(error.message || 'Playback failed.', true);
        }
      } else {
        audio.pause();
      }
    });

    seek.addEventListener('input', () => {
      seeking = true;
      const total = audio.duration || 0;
      if (total > 0) {
        audio.currentTime = (Number(seek.value) / 1000) * total;
        updatePosition();
      }
      seeking = false;
    });

    audio.addEventListener('loadedmetadata', updatePosition);
    audio.addEventListener('timeupdate', updatePosition);
    audio.addEventListener('play', () => {
      play.textContent = 'Pause';
      setStatus('Playing.');
    });
    audio.addEventListener('pause', () => {
      play.textContent = 'Play';
      if (audio.src) setStatus('Paused.');
    });
    audio.addEventListener('ended', () => {
      play.textContent = 'Play';
      setStatus('Done.');
      updatePosition();
    });
  </script>
</body>
</html>"##;

#[derive(Clone)]
pub(crate) struct ServiceState {
    pub(crate) backend: Arc<dyn TranscriptionClient>,
    pub(crate) speech: Option<Arc<dyn SpeechClient>>,
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

    Router::new()
        .route("/healthz", health_routes.clone())
        .route("/v1/healthz", health_routes)
        .route("/web", get(web_app))
        .route("/web/manifest.webmanifest", get(web_manifest))
        .route("/web-sw.js", get(web_service_worker))
        .route("/web/icon-192.png", get(web_icon_192))
        .route("/web/icon-512.png", get(web_icon_512))
        .route("/web/icon-maskable-512.png", get(web_icon_maskable_512))
        .route("/web/apple-touch-icon.png", get(web_apple_touch_icon))
        .route("/web/speech", web_speech_routes)
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
    let upload = upload::read_upload(multipart, state.client_upload_limit_bytes).await?;
    let text = transcribe_upload(&state, &upload).await?;
    Ok(match upload.response_format {
        upload::ResponseFormat::Json => Json(TranscriptionResponse { text }).into_response(),
        upload::ResponseFormat::Text => {
            ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
        }
    })
}

async fn web_app() -> Html<&'static str> {
    Html(WEB_APP_HTML)
}

async fn web_manifest() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        WEB_MANIFEST,
    )
}

async fn web_service_worker() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        WEB_SW_JS,
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

#[derive(Debug, Serialize)]
struct WebSpeechResponse {
    input: String,
    input_changed: bool,
    audio_base64: String,
    mime_type: String,
    format: String,
}

async fn web_speech(
    State(state): State<ServiceState>,
    Json(body): Json<WebSpeechRequest>,
) -> Result<Json<WebSpeechResponse>, ApiError> {
    let speech_client = state
        .speech
        .as_ref()
        .ok_or_else(|| ApiError::service_unavailable("TTS service is not configured"))?;

    if body.input.trim().is_empty() {
        return Err(ApiError::bad_request("input is required"));
    }

    let request = SpeechRequest {
        input: body.input,
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

    Ok(Json(WebSpeechResponse {
        input,
        input_changed,
        audio_base64: base64::engine::general_purpose::STANDARD.encode(&synthesized.bytes),
        mime_type: synthesized.mime_type,
        format: synthesized.format.to_openai().to_string(),
    }))
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
        assert!(html.contains("<textarea id=\"text\""));
        assert!(html.contains("id=\"generate\""));
        assert!(html.contains("id=\"seek\""));
        assert!(html.contains("'/web/speech'"));
        assert!(html.contains(r#"<link rel="manifest" href="/web/manifest.webmanifest">"#));
        assert!(html.contains(r##"<meta name="theme-color" content="#101214">"##));
        assert!(html.contains(r#"<link rel="apple-touch-icon" href="/web/apple-touch-icon.png">"#));
        assert!(html.contains("navigator.serviceWorker.register('/web-sw.js'"));
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
        assert_eq!(manifest["theme_color"], "#101214");
        assert_eq!(manifest["background_color"], "#101214");
        let icons = manifest["icons"].as_array().expect("icons array");
        assert!(icons.iter().any(|icon| {
            icon["src"] == "/web/icon-192.png"
                && icon["sizes"] == "192x192"
                && icon["type"] == "image/png"
        }));
        assert!(icons.iter().any(|icon| {
            icon["src"] == "/web/icon-512.png"
                && icon["sizes"] == "512x512"
                && icon["purpose"] == "any"
        }));
        assert!(icons.iter().any(|icon| {
            icon["src"] == "/web/icon-maskable-512.png"
                && icon["sizes"] == "512x512"
                && icon["purpose"] == "maskable"
        }));
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
        assert!(script.contains("'/web/icon-maskable-512.png'"));
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
