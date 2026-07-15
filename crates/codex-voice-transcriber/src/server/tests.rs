use super::speech::{reload_tts_config_once, TtsServiceState};
use super::transcribe::transcribe_chunk_paths;
use super::web::{
    prune_web_speech_jobs_at, BrowserTtsConfig, WebSpeechJobRecord, WebSpeechJobState,
    WebSpeechResponse, WEB_SPEECH_JOB_TTL,
};
use super::web_assets::web_dist_is_stub;
use super::*;
use crate::test_support::*;
use axum::body;
use axum::http::{header, StatusCode};
use codex_voice_core::{SpeechFormat, TranscriptionClient};
use codex_voice_tts::config::SpeechPrepProviderKind;
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
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
async fn web_app_sets_no_cache_and_html_content_type() {
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
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html")));
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
}

#[tokio::test]
async fn desktop_intent_requires_token_and_is_consumed_once() {
    let app = service_router(test_state_with_speech(1024));
    let unauthenticated = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/web/desktop-intents")
                .header(header::CONTENT_TYPE, "application/json")
                .body(body::Body::from(r#"{"text":"Привет мир"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let created = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/web/desktop-intents")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(body::Body::from(r#"{"text":"Привет мир"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_body = body::to_bytes(created.into_body(), usize::MAX)
        .await
        .unwrap();
    let created_json: serde_json::Value = serde_json::from_slice(&created_body).unwrap();
    let id = created_json["id"].as_str().unwrap();
    assert_eq!(id.len(), 32);

    let consume = || {
        axum::http::Request::builder()
            .uri(format!("/web/desktop-intents/{id}"))
            .body(body::Body::empty())
            .unwrap()
    };
    let consumed = app.clone().oneshot(consume()).await.unwrap();
    assert_eq!(consumed.status(), StatusCode::OK);
    assert_eq!(consumed.headers()[header::CACHE_CONTROL], "no-store");
    let consumed_body = body::to_bytes(consumed.into_body(), usize::MAX)
        .await
        .unwrap();
    let consumed_json: serde_json::Value = serde_json::from_slice(&consumed_body).unwrap();
    assert_eq!(consumed_json["text"], "Привет мир");

    let second = app.oneshot(consume()).await.unwrap();
    assert_eq!(second.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn desktop_intent_honors_explicit_no_auth_mode() {
    let mut state = test_state_with_speech(1024);
    state.auth.no_auth = true;
    let app = service_router(state);
    let created = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/web/desktop-intents")
                .header(header::CONTENT_TYPE, "application/json")
                .body(body::Body::from(r#"{"text":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn desktop_intent_delete_reclaims_an_unconsumed_intent() {
    let app = service_router(test_state_with_speech(1024));
    let created = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/web/desktop-intents")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(body::Body::from(r#"{"text":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body::to_bytes(created.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = json["id"].as_str().unwrap();

    let deleted = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/web/desktop-intents/{id}"))
                .body(body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let consumed = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/web/desktop-intents/{id}"))
                .body(body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(consumed.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn web_app_serves_gzip_when_requested() {
    let identity_len = {
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
        body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads")
            .len()
    };

    let app = service_router(test_state_with_speech(1024));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/web")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
    let gzip_len = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads")
        .len();
    assert!(
        gzip_len < identity_len,
        "gzip body ({gzip_len}) should be smaller than identity body ({identity_len})"
    );
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

    let state = tts
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
async fn legacy_service_worker_self_destructs() {
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
    assert!(script.contains("self.registration"));
    assert!(script.contains("unregister"));
}

#[tokio::test]
async fn web_hashed_assets_are_immutable() {
    if web_dist_is_stub() {
        eprintln!("skipped: stub web dist");
        return;
    }
    let asset = super::web_assets::first_embedded_asset_under("assets/")
        .expect("real web dist should contain a hashed asset under assets/");

    let app = service_router(test_state_with_speech(1024));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/web/{asset}"))
                .body(body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK, "{asset}");
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable",
        "{asset}"
    );
}

#[tokio::test]
async fn web_unhashed_dist_files_are_not_immutable() {
    // index.html lives at the dist root (not under assets/) and is never
    // content-hashed, so it must revalidate rather than being cached forever.
    let app = service_router(test_state_with_speech(1024));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/web/index.html")
                .body(body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html")));
}

#[tokio::test]
async fn web_spa_fallback_serves_index_for_extensionless_paths() {
    let app = service_router(test_state_with_speech(1024));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/web/settings/profile")
                .body(body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html")));
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
}

#[tokio::test]
async fn web_asset_traversal_is_rejected() {
    for path in ["/web/../Cargo.toml", "/web/..%2fCargo.toml"] {
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

        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        assert!(
            !bytes.windows(7).any(|window| window == b"package"),
            "{path} must not leak Cargo.toml contents"
        );
    }
}

#[tokio::test]
async fn web_dist_override_takes_precedence() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><title>OVERRIDE-MARKER</title>",
    )
    .expect("index written");
    std::fs::create_dir_all(dir.path().join("assets")).expect("assets dir");
    std::fs::write(
        dir.path().join("assets").join("app.abc123.js"),
        "console.log('override');\n",
    )
    .expect("asset written");

    let mut state = test_state_with_speech(1024);
    state.web_dist_override = Some(dir.path().to_path_buf());
    let app = service_router(state);

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/web")
                .body(body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    let bytes = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let html = std::str::from_utf8(&bytes).expect("html is utf-8");
    assert!(
        html.contains("OVERRIDE-MARKER"),
        "override index.html should be served"
    );

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/web/assets/app.abc123.js")
                .body(body::Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );
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
            r#"{"input":"hello from background","provider":"elevenlabs","voice":"sky","model":"eleven_v3","speechPrepEnabled":false}"#,
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
    assert_eq!(seen[0].provider_hint.as_deref(), Some("elevenlabs"));
    assert_eq!(seen[0].voice_hint.as_deref(), Some("sky"));
    assert_eq!(seen[0].model_hint, "eleven_v3");
    assert_eq!(seen[0].speech_prep_enabled, Some(false));
}

#[tokio::test]
async fn web_speech_prep_returns_enriched_text_without_synthesis() {
    let speech = Arc::new(FakeSpeechBackend {
        prepared_input: Some("[fearful] hello from prep".to_string()),
        ..Default::default()
    });
    let app = service_router(test_state_with_speech_backend(1024, Some(speech.clone())));

    let response = app
        .oneshot(speech_request(
            "/web/speech-prep",
            r#"{"input":"hello from prep","provider":"google","speechPrepEnabled":true}"#,
            None,
        ))
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json response");
    assert_eq!(json["input"], "[fearful] hello from prep");
    assert_eq!(json["input_changed"], true);
    let seen = speech.seen.lock().expect("fake speech lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].input, "hello from prep");
    assert_eq!(seen[0].provider_hint.as_deref(), Some("google"));
    assert_eq!(seen[0].speech_prep_enabled, Some(true));
}

#[test]
fn web_speech_job_pruning_removes_expired_audio_results() {
    let mut jobs = HashMap::new();
    let old_updated_at = Instant::now();
    // Prune at a point strictly after the TTL of the "old" record. Advancing
    // "now" forward avoids subtracting from `Instant::now()`, which underflows
    // on hosts with low uptime (fresh CI runners, Windows VMs) and aborts the
    // test process.
    let prune_at = old_updated_at + WEB_SPEECH_JOB_TTL + Duration::from_secs(1);
    jobs.insert(
        "old".to_string(),
        WebSpeechJobRecord {
            state: WebSpeechJobState::Complete(Arc::new(WebSpeechResponse {
                input: "old".to_string(),
                input_changed: false,
                audio_base64: "audio".to_string(),
                mime_type: "audio/wav".to_string(),
                format: "wav".to_string(),
            })),
            updated_at: old_updated_at,
            abort: None,
        },
    );
    jobs.insert(
        "fresh".to_string(),
        WebSpeechJobRecord {
            state: WebSpeechJobState::Pending { phase: "queued" },
            updated_at: prune_at,
            abort: None,
        },
    );

    prune_web_speech_jobs_at(prune_at, &mut jobs);

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
    assert_eq!(value["capabilities"]["desktop"], true);
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
async fn health_shows_desktop_false_when_web_ui_is_unavailable() {
    let mut state = test_state_with_speech(1024);
    let override_dir = tempfile::tempdir().expect("temp override");
    state.web_dist_override = Some(override_dir.path().to_path_buf());
    let app = service_router(state);
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
    let bytes = body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["capabilities"]["speech"], true);
    assert_eq!(value["capabilities"]["desktop"], false);
}

#[tokio::test]
async fn health_recomputes_override_readiness_and_checks_referenced_assets() {
    let override_dir = tempfile::tempdir().expect("temp override");
    let assets = override_dir.path().join("assets");
    std::fs::create_dir_all(&assets).expect("assets dir");
    std::fs::write(
        override_dir.path().join("index.html"),
        r#"<link href="/web/assets/app.css"><script src="/web/assets/app.js"></script>"#,
    )
    .expect("index");
    std::fs::write(assets.join("app.css"), "body{}").expect("css");
    std::fs::write(assets.join("app.js"), "export{}").expect("js");

    let mut state = test_state_with_speech(1024);
    state.web_dist_override = Some(override_dir.path().to_path_buf());
    let app = service_router(state);
    let health_request = || {
        axum::http::Request::builder()
            .uri("/healthz")
            .header(header::AUTHORIZATION, "Bearer test-token")
            .body(body::Body::empty())
            .expect("request builds")
    };

    let ready = app.clone().oneshot(health_request()).await.expect("health");
    let body = body::to_bytes(ready.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["capabilities"]["desktop"], true);

    std::fs::remove_file(assets.join("app.js")).expect("remove referenced asset");
    let stale = app.oneshot(health_request()).await.expect("health");
    let body = body::to_bytes(stale.into_body(), usize::MAX)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["capabilities"]["desktop"], false);
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

#[tokio::test]
async fn chunked_transcripts_join_in_order_under_concurrency() {
    // The first chunk is deliberately the slowest. If the concurrent stream
    // reordered results (e.g. by using buffer_unordered instead of
    // buffered), the faster later chunks would finish first and the joined
    // transcript would come out scrambled.
    const CHUNK_COUNT: u64 = 4;
    let delays: Vec<Duration> = (0..CHUNK_COUNT)
        .map(|index| Duration::from_millis((CHUNK_COUNT - index) * 10))
        .collect();
    let backend = Arc::new(DelayedFakeBackend::new(delays)) as Arc<dyn TranscriptionClient>;
    let paths: Vec<PathBuf> = (0..CHUNK_COUNT)
        .map(|index| PathBuf::from(format!("chunk-{index}.wav")))
        .collect();

    let joined = transcribe_chunk_paths(&backend, &paths)
        .await
        .expect("chunk transcription succeeds");

    assert_eq!(joined, "part-0\n\npart-1\n\npart-2\n\npart-3");
}

#[tokio::test]
async fn chunked_transcription_runs_concurrently() {
    const CHUNK_COUNT: usize = 4;
    let delays = vec![Duration::from_millis(50); CHUNK_COUNT];
    let fake = Arc::new(DelayedFakeBackend::new(delays));
    let backend = Arc::clone(&fake) as Arc<dyn TranscriptionClient>;
    let paths: Vec<PathBuf> = (0..CHUNK_COUNT)
        .map(|index| PathBuf::from(format!("chunk-{index}.wav")))
        .collect();

    let started = Instant::now();
    transcribe_chunk_paths(&backend, &paths)
        .await
        .expect("chunk transcription succeeds");
    let elapsed = started.elapsed();

    assert!(
        fake.max_active() >= 2,
        "expected overlapping in-flight transcriptions, saw max_active={}",
        fake.max_active()
    );
    assert!(
        elapsed < Duration::from_millis(150),
        "expected concurrent execution well under serial time (~200ms), took {elapsed:?}"
    );
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
