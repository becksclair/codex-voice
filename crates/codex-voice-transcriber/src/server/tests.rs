use super::speech::{reload_tts_config_once, TtsServiceState};
use super::web::{
    prune_web_speech_jobs, versioned_web_asset, web_build_version, web_cache_name,
    BrowserTtsConfig, WebSpeechJobRecord, WebSpeechJobState, WebSpeechResponse,
    WEB_BUILD_REVISION, WEB_SPEECH_JOB_TTL,
};
use super::*;
use crate::test_support::*;
use axum::body;
use axum::http::{header, StatusCode};
use codex_voice_core::SpeechFormat;
use codex_voice_tts::config::SpeechPrepProviderKind;
use std::collections::HashMap;
use std::path::Path as FsPath;
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

    assert!(
        !html.contains("__WEB_"),
        "all __WEB_*_URL__ placeholders should be substituted"
    );

    // Every DOM id the client-side JS binds via getElementById must be present
    // in the served markup (derived from
    // `grep -o "getElementById([^)]*)" assets/web/app.html`).
    for id in [
        "clear",
        "count",
        "download",
        "duration",
        "elapsed",
        "emotion",
        "error-banner",
        "generate",
        "generate-label",
        "generate-on-paste",
        "model",
        "paste",
        "play",
        "play-icon",
        "provider",
        "settings-panel",
        "settings-toggle",
        "summarize",
        "text",
        "theme",
        "voice",
        "waveform",
        "waveform-slider",
    ] {
        assert!(
            html.contains(&format!("id=\"{id}\"")),
            "served HTML should contain id=\"{id}\" for a getElementById binding"
        );
    }
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
    assert!(script.contains(&format!(
        "const CACHE_NAME = {};",
        serde_json::to_string(&web_cache_name()).expect("cache name serializes")
    )));
    assert!(script.contains(&format!(
        "const WEB_BUILD_REVISION = {};",
        serde_json::to_string(WEB_BUILD_REVISION).expect("revision serializes")
    )));
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
