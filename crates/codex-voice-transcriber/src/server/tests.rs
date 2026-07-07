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
