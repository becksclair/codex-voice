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
use codex_voice_tts::config::{
    ElevenLabsPersonaConfig, FallbackPolicy, GooglePersonaConfig, ProviderKind, ResolvedPersona,
    ResolvedTtsConfig, SpeechPrepMode, SpeechPrepProviderKind, SpeechPrepStrategies,
    SpeechPrepStrategy,
};

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
const WEB_SW_BODY_JS: &str = r#"const SHELL_ASSETS = [
  '/web',
  '/web/manifest.webmanifest',
  '/web/icon-192.png',
  '/web/icon-512.png',
  '/web/icon-maskable-512.png',
  '/web/apple-touch-icon.png'
];
const NETWORK_FIRST_ASSETS = new Set([
  '/web',
  '/web/manifest.webmanifest'
]);
const CACHE_FIRST_ASSETS = new Set([
  '/web/icon-192.png',
  '/web/icon-512.png',
  '/web/icon-maskable-512.png',
  '/web/apple-touch-icon.png'
]);

async function networkFirst(request, cacheKey) {
  const cache = await caches.open(CACHE_NAME);
  try {
    const response = await fetch(request);
    if (response.ok) {
      await cache.put(cacheKey, response.clone());
      return response;
    }
    const cached = await cache.match(cacheKey);
    if (cached) return cached;
    return response;
  } catch (_) {
    const cached = await cache.match(cacheKey);
    if (cached) return cached;
    throw _;
  }
}

async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) return cached;
  const response = await fetch(request);
  if (response.ok) {
    const cache = await caches.open(CACHE_NAME);
    await cache.put(request, response.clone());
  }
  return response;
}

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
    event.respondWith(networkFirst(request, '/web'));
    return;
  }

  if (NETWORK_FIRST_ASSETS.has(url.pathname)) {
    event.respondWith(networkFirst(request, url.pathname));
    return;
  }

  if (CACHE_FIRST_ASSETS.has(url.pathname)) {
    event.respondWith(cacheFirst(request));
  }
});
"#;
const WEB_APP_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover">
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
    html, body {
      min-height: 100%;
      height: 100%;
      overflow: hidden;
    }
    body {
      margin: 0;
      background: var(--bg);
      color: var(--text);
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      letter-spacing: 0;
    }
    main {
      height: var(--visual-viewport-height, 100dvh);
      min-height: 0;
      display: flex;
      flex-direction: column;
      gap: 14px;
      padding: max(18px, env(safe-area-inset-top)) 16px max(18px, env(safe-area-inset-bottom));
      max-width: 760px;
      margin: 0 auto;
      overflow: hidden;
      transform: translateY(var(--visual-viewport-offset-top, 0px));
    }
    header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 14px;
    }
    .app-icon {
      width: 44px;
      height: 44px;
      display: block;
      border-radius: 8px;
    }
    #count {
      color: var(--muted);
      font-size: 0.92rem;
      white-space: nowrap;
    }
    .header-actions {
      display: flex;
      align-items: center;
      gap: 8px;
    }
    .icon-button {
      width: 44px;
      min-width: 44px;
      min-height: 44px;
      padding: 0;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      color: var(--text);
      background: #252b31;
      border: 1px solid var(--line);
    }
    .icon-button svg,
    button svg {
      width: 20px;
      height: 20px;
      stroke: currentColor;
      stroke-width: 2;
      stroke-linecap: round;
      stroke-linejoin: round;
      fill: none;
      pointer-events: none;
    }
    .error-banner {
      display: none;
      min-height: 44px;
      align-items: center;
      padding: 10px 12px;
      border: 1px solid rgba(255, 143, 143, 0.45);
      border-radius: 8px;
      color: #ffd4d4;
      background: rgba(120, 38, 38, 0.34);
      font-size: 0.95rem;
    }
    .error-banner.visible { display: flex; }
    .text-shell {
      position: relative;
      flex: 1 1 auto;
      display: flex;
      min-height: 260px;
    }
    textarea {
      flex: 1 1 auto;
      width: 100%;
      height: auto;
      min-height: 0;
      resize: none;
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 16px 58px 58px 16px;
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
    #paste {
      position: absolute;
      top: 10px;
      right: 10px;
      background: rgba(37, 43, 49, 0.72);
      backdrop-filter: blur(8px);
    }
    #clear {
      position: absolute;
      right: 10px;
      bottom: 10px;
      background: rgba(37, 43, 49, 0.72);
      backdrop-filter: blur(8px);
    }
    .controls {
      flex: 0 0 auto;
      display: grid;
      gap: 14px;
    }
    .buttons {
      display: grid;
      grid-template-columns: minmax(0, 1fr) 52px;
      align-items: center;
      gap: 8px;
    }
    button {
      min-height: 44px;
      padding: 0 10px;
      border: 0;
      border-radius: 8px;
      color: #081110;
      background: var(--accent);
      font: inherit;
      font-size: 0.98rem;
      font-weight: 700;
      cursor: pointer;
      touch-action: manipulation;
    }
    .buttons button {
      height: 44px;
    }
    button.secondary {
      color: var(--text);
      background: #252b31;
      border: 1px solid var(--line);
    }
    #generate {
      position: relative;
      overflow: hidden;
      display: flex;
      align-items: center;
      justify-content: center;
    }
    .generate-main {
      position: relative;
      z-index: 1;
      min-width: 0;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      gap: 8px;
    }
    .spinner {
      display: none;
      width: 18px;
      height: 18px;
      border: 2px solid rgba(8, 17, 16, 0.28);
      border-top-color: #081110;
      border-radius: 999px;
      animation: spin 0.8s linear infinite;
    }
    #generate.generating .spinner { display: inline-block; }
    .generate-progress {
      position: absolute;
      left: 0;
      right: 0;
      bottom: 0;
      height: 3px;
      background: rgba(8, 17, 16, 0.22);
      overflow: hidden;
    }
    .generate-progress span {
      display: block;
      width: calc(var(--generate-progress, 0) * 100%);
      height: 100%;
      background: #081110;
      transition: width 180ms ease;
    }
    @keyframes spin { to { transform: rotate(360deg); } }
    button:disabled {
      cursor: not-allowed;
      opacity: 0.55;
    }
    input[type="range"] {
      width: 100%;
      accent-color: var(--accent-strong);
    }
    .scrubber {
      display: grid;
      grid-template-columns: auto minmax(0, 1fr) auto;
      align-items: center;
      gap: 10px;
      color: var(--muted);
      font-variant-numeric: tabular-nums;
      font-size: 0.95rem;
    }
    .settings {
      border: 1px solid var(--line);
      border-radius: 8px;
      background: rgba(25, 29, 33, 0.72);
    }
    .settings[hidden] {
      display: none;
    }
    .settings-grid {
      display: grid;
      gap: 12px;
      padding: 14px;
    }
    .field {
      display: grid;
      gap: 6px;
      color: var(--muted);
      font-size: 0.9rem;
    }
    select {
      min-height: 42px;
      width: 100%;
      border: 1px solid var(--line);
      border-radius: 8px;
      padding: 0 10px;
      background: #252b31;
      color: var(--text);
      font: inherit;
    }
    .toggles {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 8px;
    }
    .toggle {
      min-height: 42px;
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 0 10px;
      border: 1px solid var(--line);
      border-radius: 8px;
      color: var(--text);
      background: #252b31;
      font-weight: 650;
    }
    .toggle input {
      width: 18px;
      height: 18px;
      accent-color: var(--accent-strong);
    }
    html.keyboard-open main {
      padding-bottom: max(10px, env(safe-area-inset-bottom));
    }
    html.keyboard-open .text-shell {
      min-height: 120px;
    }
    @media (max-width: 420px) {
      main { padding-left: 12px; padding-right: 12px; }
    }
  </style>
</head>
<body>
  <main>
    <header>
      <img class="app-icon" src="/web/icon-192.png" alt="Codex Voice">
      <div class="header-actions">
        <span id="count">0 chars</span>
        <button id="settings-toggle" type="button" class="icon-button" aria-label="Toggle settings" aria-expanded="false">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"/><path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.04 1.56V21a2 2 0 1 1-4 0v-.08a1.7 1.7 0 0 0-1.04-1.56 1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.56-1.04H3a2 2 0 1 1 0-4h.08A1.7 1.7 0 0 0 4.64 8.9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-1.56V3a2 2 0 1 1 4 0v.08a1.7 1.7 0 0 0 1.04 1.56 1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 9c.1.38.4.7.76.86.25.1.52.15.8.14H21a2 2 0 1 1 0 4h-.08A1.7 1.7 0 0 0 19.4 15Z"/></svg>
        </button>
      </div>
    </header>
    <div id="error-banner" class="error-banner" role="alert"></div>
    <div class="text-shell">
      <textarea id="text" autocomplete="off" autocapitalize="sentences" spellcheck="true" placeholder="Type something to hear it spoken..."></textarea>
      <button id="paste" type="button" class="icon-button" aria-label="Paste clipboard contents">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 4h8"/><path d="M9 2h6a1 1 0 0 1 1 1v2H8V3a1 1 0 0 1 1-1Z"/><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/></svg>
      </button>
      <button id="clear" type="button" class="secondary icon-button" aria-label="Clear text">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/><path d="M10 11v5"/><path d="M14 11v5"/></svg>
      </button>
    </div>
    <section class="controls">
      <div class="scrubber">
        <span id="elapsed">0:00</span>
        <input id="seek" type="range" min="0" max="1000" value="0" disabled aria-label="Audio position">
        <span id="duration">0:00</span>
      </div>
      <div class="buttons">
        <button id="generate" type="button">
          <span class="generate-main"><span class="spinner" aria-hidden="true"></span><span id="generate-label">Generate</span></span>
          <span class="generate-progress" aria-hidden="true"><span></span></span>
        </button>
        <button id="play" type="button" class="secondary icon-button" disabled aria-label="Play">
          <svg id="play-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5v14l11-7Z"/></svg>
        </button>
      </div>
      <div class="settings" id="settings-panel" hidden>
        <div class="settings-grid">
          <label class="field">
            Provider
            <select id="provider"></select>
          </label>
          <label class="field">
            Voice
            <select id="voice"></select>
          </label>
          <label class="field">
            Model
            <select id="model"></select>
          </label>
          <div class="toggles">
            <label class="toggle">
              <input id="emotion" type="checkbox">
              Emotion
            </label>
            <label class="toggle">
              <input id="summarize" type="checkbox">
              Summarize
            </label>
          </div>
        </div>
      </div>
    </section>
  </main>
  <script>
    const text = document.getElementById('text');
    const generate = document.getElementById('generate');
    const generateLabel = document.getElementById('generate-label');
    const play = document.getElementById('play');
    const playIcon = document.getElementById('play-icon');
    const clear = document.getElementById('clear');
    const paste = document.getElementById('paste');
    const settingsToggle = document.getElementById('settings-toggle');
    const settingsPanel = document.getElementById('settings-panel');
    const providerSelect = document.getElementById('provider');
    const voiceSelect = document.getElementById('voice');
    const modelSelect = document.getElementById('model');
    const emotion = document.getElementById('emotion');
    const summarize = document.getElementById('summarize');
    const seek = document.getElementById('seek');
    const elapsed = document.getElementById('elapsed');
    const duration = document.getElementById('duration');
    const errorBanner = document.getElementById('error-banner');
    const count = document.getElementById('count');
    const textStorageKey = 'codex-voice.web.text';
    const configStorageKey = 'codex-voice.web.config.v1';
    const settingsStorageKey = 'codex-voice.web.settings.v1';
    const generationStateStorageKey = 'codex-voice.web.generation.v1';
    const generatedAudioDbName = 'codex-voice-web-audio';
    const generatedAudioStore = 'generated';
    const lastGeneratedAudioKey = 'last';
    const maxPendingGenerationAgeMs = 6 * 60 * 60 * 1000;
    const defaultSpeechPrepAttemptTimeoutMs = 4000;
    const performanceTagsMaxOutputTokens = 384;
    const performanceTagsAbsoluteMaxOutputTokens = 4096;
    const minShortenOutputChars = 4000;
    const speechModelHint = 'gpt-4o-mini-tts';
    let audio = new Audio();
    let objectUrl = null;
    let seeking = false;
    let directConfig = loadCachedConfig();
    let settings = loadSettings();
    let serviceWorkerRefreshing = false;
    let pendingWorkerReload = false;
    let generationActive = false;
    let lifecycleInterruptedGeneration = false;

    if ('serviceWorker' in navigator) {
      window.addEventListener('load', () => {
        navigator.serviceWorker.register('/web-sw.js', { scope: '/web', updateViaCache: 'none' })
          .then((registration) => registration.update().catch(() => {}))
          .catch(() => {});
      });
      navigator.serviceWorker.addEventListener('controllerchange', () => {
        if (serviceWorkerRefreshing) return;
        if (generationActive) {
          pendingWorkerReload = true;
          return;
        }
        serviceWorkerRefreshing = true;
        window.location.reload();
      });
    }

    text.value = localStorage.getItem(textStorageKey) || '';
    updateVisualViewportLayout();
    updateCount();
    applySettingsToForm();
    populateSettings();
    refreshConfig();
    initializeStoredState();

    function updateVisualViewportLayout() {
      const viewport = window.visualViewport;
      const height = viewport?.height || window.innerHeight || document.documentElement.clientHeight;
      const offsetTop = viewport?.offsetTop || 0;
      const keyboardInset = Math.max(0, (window.innerHeight || height) - height - offsetTop);
      document.documentElement.style.setProperty('--visual-viewport-height', `${height}px`);
      document.documentElement.style.setProperty('--visual-viewport-offset-top', `${offsetTop}px`);
      document.documentElement.classList.toggle('keyboard-open', keyboardInset > 80);
    }

    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', updateVisualViewportLayout);
      window.visualViewport.addEventListener('scroll', updateVisualViewportLayout);
    }
    window.addEventListener('resize', updateVisualViewportLayout);
    window.addEventListener('orientationchange', updateVisualViewportLayout);
    text.addEventListener('focus', updateVisualViewportLayout);
    text.addEventListener('blur', () => setTimeout(updateVisualViewportLayout, 120));

    function showError(message) {
      errorBanner.textContent = message || 'Something went wrong.';
      errorBanner.classList.add('visible');
    }

    function clearError() {
      errorBanner.textContent = '';
      errorBanner.classList.remove('visible');
    }

    function setGenerateProgress(value, label = 'Generate') {
      const progress = Math.max(0, Math.min(1, Number(value) || 0));
      generate.style.setProperty('--generate-progress', String(progress));
      generateLabel.textContent = label;
    }

    function setGenerating(active, label = 'Generate', progress = 0) {
      generate.classList.toggle('generating', active);
      setGenerateProgress(progress, label);
    }

    function playSvg(paused) {
      playIcon.innerHTML = paused
        ? '<path d="M8 5v14l11-7Z"/>'
        : '<path d="M8 5v14"/><path d="M16 5v14"/>';
      play.setAttribute('aria-label', paused ? 'Play' : 'Pause');
    }

    function sanitizeBrowserConfig(config) {
      if (config?.speechPrep?.codexAuth) {
        delete config.speechPrep.codexAuth;
      }
      return config;
    }

    function loadCachedConfig() {
      try {
        const raw = localStorage.getItem(configStorageKey);
        const config = raw ? sanitizeBrowserConfig(JSON.parse(raw)) : null;
        if (config) localStorage.setItem(configStorageKey, JSON.stringify(config));
        return config;
      } catch (_) {
        return null;
      }
    }

    function loadSettings() {
      const defaults = {
        provider: 'auto',
        voice: 'default',
        model: 'default',
        emotionPreprocessing: true,
        summarization: false
      };
      try {
        return { ...defaults, ...(JSON.parse(localStorage.getItem(settingsStorageKey) || '{}')) };
      } catch (_) {
        return defaults;
      }
    }

    function saveSettings() {
      settings = {
        provider: providerSelect.value || 'auto',
        voice: voiceSelect.value || 'default',
        model: modelSelect.value || 'default',
        emotionPreprocessing: emotion.checked,
        summarization: summarize.checked
      };
      localStorage.setItem(settingsStorageKey, JSON.stringify(settings));
    }

    function option(value, label) {
      const node = document.createElement('option');
      node.value = value;
      node.textContent = label;
      return node;
    }

    function applySettingsToForm() {
      providerSelect.value = settings.provider;
      voiceSelect.value = settings.voice;
      modelSelect.value = settings.model;
      emotion.checked = settings.emotionPreprocessing;
      summarize.checked = settings.summarization;
    }

    function personaEntries(config) {
      return Object.entries(config?.personas || {});
    }

    function personaSupportsProvider(persona, provider) {
      if (provider === 'elevenlabs') return Boolean(persona?.elevenlabs?.voiceId);
      return true;
    }

    function firstPersonaForProvider(config, provider) {
      const found = personaEntries(config).find(([, persona]) => personaSupportsProvider(persona, provider));
      return found ? found[0] : null;
    }

    function providerCanGenerate(config, provider) {
      if (provider === 'google') return Boolean(config?.providers?.google);
      if (provider === 'elevenlabs') {
        return Boolean(config?.providers?.elevenlabs && firstPersonaForProvider(config, 'elevenlabs'));
      }
      return false;
    }

    function providerModelOptions(config, provider) {
      const seen = new Set();
      const options = [];
      const pushModel = (value) => {
        if (!value || seen.has(value)) return;
        seen.add(value);
        options.push(value);
      };
      if (provider === 'google') {
        const google = config?.providers?.google;
        pushModel(google?.model);
        for (const model of google?.fallbackModels || []) pushModel(model);
      } else if (provider === 'elevenlabs') {
        pushModel(config?.providers?.elevenlabs?.modelId);
      }
      return options.map((model) => option(`${provider}:${model}`, model));
    }

    function populateSettings() {
      const priorProvider = providerSelect.value || settings.provider;
      const priorVoice = voiceSelect.value || settings.voice;
      const priorModel = modelSelect.value || settings.model;
      providerSelect.replaceChildren(option('auto', 'Auto'));
      if (providerCanGenerate(directConfig, 'google')) providerSelect.append(option('google', 'Google'));
      if (providerCanGenerate(directConfig, 'elevenlabs')) providerSelect.append(option('elevenlabs', 'ElevenLabs'));
      providerSelect.value = [...providerSelect.options].some((item) => item.value === priorProvider) ? priorProvider : 'auto';

      voiceSelect.replaceChildren(option('default', 'Default'));
      if (providerSelect.value !== 'elevenlabs') voiceSelect.append(option('provider-default', 'Provider default'));
      for (const [name, persona] of personaEntries(directConfig)) {
        if (!personaSupportsProvider(persona, providerSelect.value)) continue;
        voiceSelect.append(option(`persona:${name}`, persona.label || name));
      }
      voiceSelect.value = [...voiceSelect.options].some((item) => item.value === priorVoice) ? priorVoice : 'default';

      modelSelect.replaceChildren(option('default', 'Default'));
      if (providerSelect.value !== 'auto') {
        for (const modelOption of providerModelOptions(directConfig, providerSelect.value)) {
          modelSelect.append(modelOption);
        }
      }
      modelSelect.value = [...modelSelect.options].some((item) => item.value === priorModel) ? priorModel : 'default';
      saveSettings();
    }

    async function refreshConfig() {
      try {
        const response = await fetch('/web/config', { cache: 'no-store' });
        if (!response.ok) return;
        const config = sanitizeBrowserConfig(await response.json());
        if (config?.version !== 1 || !config.providers) return;
        directConfig = config;
        localStorage.setItem(configStorageKey, JSON.stringify(config));
        populateSettings();
      } catch (_) {}
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
      playSvg(true);
      seek.disabled = true;
      seek.value = 0;
      elapsed.textContent = '0:00';
      duration.textContent = '0:00';
    }

    function loadAudioBlob(blob) {
      resetAudio();
      objectUrl = URL.createObjectURL(blob);
      audio.src = objectUrl;
      audio.load();
      play.disabled = false;
      seek.disabled = false;
    }

    function openGeneratedAudioDb() {
      return new Promise((resolve, reject) => {
        if (!('indexedDB' in window)) {
          reject(new Error('IndexedDB is not available.'));
          return;
        }
        const request = indexedDB.open(generatedAudioDbName, 1);
        request.onupgradeneeded = () => {
          request.result.createObjectStore(generatedAudioStore, { keyPath: 'id' });
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error || new Error('Could not open audio storage.'));
      });
    }

    async function withGeneratedAudioStore(mode, callback) {
      const db = await openGeneratedAudioDb();
      try {
        return await new Promise((resolve, reject) => {
          const transaction = db.transaction(generatedAudioStore, mode);
          const store = transaction.objectStore(generatedAudioStore);
          const request = callback(store);
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error || new Error('Audio storage request failed.'));
          transaction.onerror = () => reject(transaction.error || new Error('Audio storage transaction failed.'));
        });
      } finally {
        db.close();
      }
    }

    async function saveLastGeneratedAudio(blob, generatedText, inputChanged) {
      try {
        await withGeneratedAudioStore('readwrite', (store) => store.put({
          id: lastGeneratedAudioKey,
          text: generatedText,
          blob,
          mimeType: blob.type || 'audio/wav',
          inputChanged: Boolean(inputChanged),
          createdAt: new Date().toISOString()
        }));
      } catch (_) {}
    }

    async function deleteLastGeneratedAudio() {
      try {
        await withGeneratedAudioStore('readwrite', (store) => store.delete(lastGeneratedAudioKey));
      } catch (_) {}
    }

    function savePendingGeneration(input, jobId = null) {
      localStorage.setItem(generationStateStorageKey, JSON.stringify({
        input,
        jobId,
        startedAt: Date.now()
      }));
    }

    function loadPendingGeneration() {
      try {
        const pending = JSON.parse(localStorage.getItem(generationStateStorageKey) || 'null');
        if (!pending?.input || !pending?.startedAt) return null;
        if (Date.now() - pending.startedAt > maxPendingGenerationAgeMs) {
          localStorage.removeItem(generationStateStorageKey);
          return null;
        }
        return pending;
      } catch (_) {
        localStorage.removeItem(generationStateStorageKey);
        return null;
      }
    }

    function clearPendingGeneration() {
      localStorage.removeItem(generationStateStorageKey);
    }

    function shouldKeepPendingGeneration(error) {
      if (error?.status) return false;
      return error?.name === 'AbortError' || (lifecycleInterruptedGeneration && error?.name === 'TypeError');
    }

    function currentDraftText() {
      return text.value || localStorage.getItem(textStorageKey) || '';
    }

    function shouldApplyGeneratedText(generationInput, generatedText) {
      const currentDraft = currentDraftText();
      return !currentDraft || currentDraft === generationInput || currentDraft === generatedText;
    }

    async function restoreLastGeneratedAudio() {
      try {
        const record = await withGeneratedAudioStore('readonly', (store) => store.get(lastGeneratedAudioKey));
        if (!record?.blob) return;
        if (typeof record.text === 'string') {
          if (shouldApplyGeneratedText(record.text, record.text)) {
            text.value = record.text;
            localStorage.setItem(textStorageKey, text.value);
            updateCount();
          }
        }
        loadAudioBlob(record.blob);
        clearError();
      } catch (_) {}
    }

    async function resumePendingGeneration() {
      const pending = loadPendingGeneration();
      if (!pending || generationActive) return;
      if (shouldApplyGeneratedText(pending.input, pending.input)) {
        text.value = pending.input;
        localStorage.setItem(textStorageKey, text.value);
        updateCount();
      }
      clearError();
      await runGeneration(pending.input, pending.jobId || null);
    }

    async function initializeStoredState() {
      await restoreLastGeneratedAudio();
      await resumePendingGeneration();
    }

    function bytesFromBase64(base64Audio) {
      const binary = atob(base64Audio);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) {
        bytes[i] = binary.charCodeAt(i);
      }
      return bytes;
    }

    function audioBlobFromBase64(base64Audio, mimeType) {
      return new Blob([bytesFromBase64(base64Audio)], { type: mimeType || 'audio/wav' });
    }

    function normalizeGoogleModelName(model) {
      return String(model || '').replace(/^google\//, '');
    }

    function clamp(value, min, max) {
      return Math.min(max, Math.max(min, value));
    }

    function selectedPersonaName(config, provider) {
      if (settings.voice === 'provider-default') return null;
      if (settings.voice?.startsWith('persona:')) return settings.voice.slice('persona:'.length);
      if (provider === 'elevenlabs') {
        const defaultPersona = config?.defaultPersona ? config.personas?.[config.defaultPersona] : null;
        return personaSupportsProvider(defaultPersona, 'elevenlabs')
          ? config.defaultPersona
          : firstPersonaForProvider(config, 'elevenlabs');
      }
      return config?.defaultPersona || null;
    }

    function resolvePersona(config, provider) {
      const name = selectedPersonaName(config, provider);
      return name && config.personas ? config.personas[name] || null : null;
    }

    function resolveProvider(config, persona) {
      if (settings.provider !== 'auto') return settings.provider;
      return persona?.provider || config.defaultProvider;
    }

    function fallbackProvider(provider) {
      return provider === 'google' ? 'elevenlabs' : 'google';
    }

    function providerMaxTextLength(config, provider) {
      const providerConfig = config?.providers?.[provider];
      return Number(providerConfig?.maxTextLength) || Number(config?.maxTextLength) || Infinity;
    }

    function selectedProviderModel(provider, defaultModel) {
      const prefix = `${provider}:`;
      return settings.model?.startsWith(prefix) ? settings.model.slice(prefix.length) : defaultModel;
    }

    function resolveGoogleModel(google) {
      if (!google) return '';
      return selectedProviderModel('google', google.model);
    }

    function resolveElevenLabsModel(elevenlabs) {
      if (!elevenlabs) return '';
      return selectedProviderModel('elevenlabs', elevenlabs.modelId);
    }

    function providerSupportsInlineAudioTags(config, provider) {
      if (provider === 'google') {
        const google = config.providers?.google;
        if (!google) return false;
        if (typeof google.inlineAudioTags === 'boolean') return google.inlineAudioTags;
        const model = resolveGoogleModel(google).toLowerCase();
        return model.includes('gemini-3.1') && model.includes('tts');
      }
      if (provider === 'elevenlabs') {
        const elevenlabs = config.providers?.elevenlabs;
        if (!elevenlabs) return false;
        if (typeof elevenlabs.inlineAudioTags === 'boolean') return elevenlabs.inlineAudioTags;
        const model = String(elevenlabs.modelId || '').toLowerCase();
        return model === 'eleven_v3' || model.startsWith('eleven_v3_');
      }
      return false;
    }

    function googleSupportsStyleInstruction(config) {
      const model = resolveGoogleModel(config.providers?.google).toLowerCase();
      return model.includes('gemini') && model.includes('tts');
    }

    function speechPrepStrategy(config, provider) {
      const prep = config?.speechPrep;
      if (!prep || prep.mode === 'shorten') return 'shorten';
      const configured = provider === 'google'
        ? prep.strategies?.google
        : provider === 'elevenlabs'
          ? prep.strategies?.elevenlabs
          : prep.strategies?.default;
      const strategy = configured && configured !== 'off' ? configured : prep.strategies?.default || 'off';
      if (strategy === 'inline-tags') {
        return providerSupportsInlineAudioTags(config, provider) ? 'inline-tags' : 'off';
      }
      if (strategy === 'style-instruction') {
        return provider === 'google' && googleSupportsStyleInstruction(config) ? 'style-instruction' : 'off';
      }
      return 'off';
    }

    function googleSpeechPrepFallback(prep) {
      const fallback = prep?.browserFallback;
      if (fallback?.provider !== 'google' || !fallback.apiKey || !fallback.baseUrl || !fallback.model) {
        return null;
      }
      return {
        ...prep,
        provider: 'google',
        browserSupported: true,
        apiKey: fallback.apiKey,
        codexAuth: null,
        baseUrl: fallback.baseUrl,
        model: fallback.model,
        fallbackModels: fallback.fallbackModels || [],
        reasoningEffort: null
      };
    }

    function browserSpeechPrepForDirect(config) {
      const prep = config?.speechPrep;
      if (!prep || prep.browserSupported !== false) return prep || null;
      return googleSpeechPrepFallback(prep) || prep;
    }

    function speechPrepForProviderLimit(prep, maxLength) {
      if (!prep || !Number.isFinite(maxLength)) return prep;
      const targetLength = shortenFitLimit(maxLength);
      return {
        ...prep,
        mode: 'shorten',
        maxLength: targetLength,
        threshold: Math.min(targetLength, minShortenOutputChars),
        forceSummarization: true
      };
    }

    function shortenFitLimit(providerMaxLength) {
      if (!Number.isFinite(providerMaxLength) || providerMaxLength <= minShortenOutputChars) {
        return providerMaxLength;
      }
      return minShortenOutputChars;
    }

    function truncateToChars(value, maxLength) {
      if (!Number.isFinite(maxLength)) return value;
      const chars = Array.from(value);
      return chars.length <= maxLength ? value : chars.slice(0, maxLength).join('');
    }

    function extractiveShortenToFit(value, maxLength) {
      return truncateToChars(value, maxLength);
    }

    function prepareDecision(input, prep, strategy) {
      if (!prep) return { shouldPrepare: false, reason: 'No speech prep config.' };
      if (prep.mode === 'performance-tags' && !settings.emotionPreprocessing) {
        return { shouldPrepare: false, reason: 'Emotion prep is off.' };
      }
      if (prep.mode === 'shorten' && !settings.summarization && !prep.forceSummarization) {
        return { shouldPrepare: false, reason: 'Summarization is off.' };
      }
      if (prep.mode !== 'performance-tags' && prep.mode !== 'shorten') {
        return { shouldPrepare: false, reason: 'Unsupported speech prep mode.' };
      }
      if (prep.mode === 'performance-tags' && strategy === 'off') {
        return { shouldPrepare: false, reason: 'Speech model does not support configured emotion prep.' };
      }
      const chars = Array.from(input).length;
      if (chars < prep.threshold) return { shouldPrepare: false, reason: 'Text is below the prep threshold.' };
      if (chars > prep.maxInputLength && !prep.forceSummarization) return { shouldPrepare: false, reason: 'Text is too long for prep.' };
      if (prep.mode === 'shorten' && chars <= shortenPrepareFloor(prep)) return { shouldPrepare: false, reason: 'Text already fits without summarization.' };
      if (prep.mode === 'shorten' && chars <= prep.maxLength) return { shouldPrepare: false, reason: 'Text already fits the speech limit.' };
      return { shouldPrepare: true, reason: '' };
    }

    function shouldPrepare(input, prep, supportsInlineAudioTags) {
      return prepareDecision(input, prep, supportsInlineAudioTags ? 'inline-tags' : 'off').shouldPrepare;
    }

    function elapsedMs(startedAt) {
      return Math.max(0, Math.round(performance.now() - startedAt));
    }

    function formatDurationMs(ms) {
      if (!Number.isFinite(ms) || ms <= 0) return '0.0s';
      return `${(ms / 1000).toFixed(1)}s`;
    }

    function performanceTagsOutputTokens(input, prep) {
      const inputChars = Array.from(input).length;
      const defaultCap = clamp(Math.floor(prep.maxLength / 2), 128, performanceTagsMaxOutputTokens);
      const preserveCap = clamp(Math.floor(inputChars / 3), 128, performanceTagsAbsoluteMaxOutputTokens);
      return Math.max(defaultCap, preserveCap);
    }

    function buildShortenPrompt(input, prep) {
      const minLength = shortenMinOutputChars(input, prep);
      return `Prepare this text for text-to-speech playback. Preserve the user's meaning, key facts, decisions, and the full requested message. Shorten only when necessary to stay under ${prep.maxLength} characters. Keep the prepared text at least ${minLength} characters unless the source text itself is shorter. Do not collapse prose into a short abstract. Remove repetition, code blocks, URLs, file paths, and formatting noise. Return only natural speakable prose, no markdown, no preamble, no labels.\n\nText:\n"""${input}"""`;
    }

    function shortenPrepareFloor(prep) {
      return Math.max(Number(prep.threshold) || 0, Math.min(minShortenOutputChars, Number(prep.maxLength) || minShortenOutputChars));
    }

    function shortenMinOutputChars(input, prep) {
      const inputChars = Array.from(input).length;
      return Math.min(inputChars, Number(prep.maxLength) || inputChars, minShortenOutputChars);
    }

    function buildPerformanceTagsPrompt(input, prep, persona) {
      let prompt = 'You are a TTS performance tagger. Do not rewrite the text. Do not summarize. Insert concise emotion/performance tags only where they improve delivery. Use tags sparingly. Keep tags local to the phrase or paragraph they affect. Prefer natural performance: warm, amused, teasing, soft, relieved, sleepy, serious, whispering, laughing, affectionate. Never add tags that contradict the text. Return only the tagged text. Every performance cue you add must be enclosed in square brackets, like [softly] or [sigh of relief]. If no cue improves delivery, return the original text unchanged.\n';
      const palette = (prep.tagPalette || ['excited', 'delighted', 'playful', 'brightly', 'nervous', 'uneasy', 'fearful', 'frustrated', 'angry', 'stern', 'sorrowful', 'wistful', 'choked up', 'calm', 'reassuring', 'tender', 'vulnerable', 'affectionate', 'proud', 'determined', 'amused', 'dryly', 'deadpan', 'relieved', 'sleepy', 'serious', 'urgent', 'teasing', 'warmly', 'softly', 'flatly', 'breathless', 'sigh', 'laughs', 'laughing', 'gasps', 'whispers', 'exhales', 'shaky breath', 'light chuckle', 'snorts', 'scoffs', 'sigh of relief', 'hesitates', 'pause', 'long pause', 'voice breaks', 'swallows', 'leans closer', 'under breath', 'smiling', 'moan'])
        .map((tag) => `[${tag}]`)
        .join(', ');
      prompt += `Use inline bracketed audio tags from this palette when they fit: ${palette}. Closely related performable cues are allowed when the palette does not fit, but they must also be square-bracketed. Keep the result under `;
      prompt += `${prep.maxLength} characters.\n\n`;
      if (persona) {
        prompt += 'Delivery context:\n';
        prompt += `- persona: ${persona.label} - ${persona.description}\n`;
        if (persona.promptScene) prompt += `- scene: ${persona.promptScene}\n`;
        if (persona.promptStyle) prompt += `- style: ${persona.promptStyle}\n`;
        if (persona.promptPacing) prompt += `- pace: ${persona.promptPacing}\n`;
        for (const constraint of persona.promptConstraints || []) {
          prompt += `- constraint: ${constraint}\n`;
        }
        prompt += '\n';
      }
      prompt += `Text:\n"""${input}"""`;
      return prompt;
    }

    function buildStyleInstructionPrompt(input, prep, persona) {
      let prompt = 'You are a TTS delivery director for Google Gemini speech synthesis. Do not rewrite, summarize, quote, or repeat the text. Return only a 1-3 sentence natural-language delivery instruction for how the voice should perform this exact message: emotional state, pacing, intimacy, tension, hesitation, warmth, and release. Keep it concrete and speakable as direction, not content. Never include bracket tags. Keep the instruction under 300 characters.\n\n';
      if (persona) {
        prompt += 'Delivery context:\n';
        prompt += `- persona: ${persona.label} - ${persona.description}\n`;
        if (persona.promptScene) prompt += `- scene: ${persona.promptScene}\n`;
        if (persona.promptStyle) prompt += `- style: ${persona.promptStyle}\n`;
        if (persona.promptPacing) prompt += `- pace: ${persona.promptPacing}\n`;
        for (const constraint of persona.promptConstraints || []) {
          prompt += `- constraint: ${constraint}\n`;
        }
        prompt += '\n';
      }
      prompt += `Text to direct, not rewrite:\n"""${input}"""`;
      return prompt;
    }

    function textWords(value) {
      return String(value || '')
        .replace(/\[[^\]]{1,80}\]/g, ' ')
        .toLowerCase()
        .match(/[a-z0-9']+/g) || [];
    }

    function textWordSpans(value) {
      const text = String(value || '');
      const spans = [];
      let inTag = false;
      let current = '';
      let start = 0;
      for (let index = 0; index < text.length; index += 1) {
        const ch = text[index];
        if (ch === '[' && !current) {
          inTag = true;
          continue;
        }
        if (ch === ']' && inTag) {
          inTag = false;
          continue;
        }
        if (inTag) continue;
        if (/[a-z0-9']/i.test(ch)) {
          if (!current) start = index;
          current += ch.toLowerCase();
          continue;
        }
        if (current) {
          spans.push({ word: current, start, end: index });
          current = '';
        }
      }
      if (current) spans.push({ word: current, start, end: text.length });
      return spans;
    }

    function performanceTagsPreserveText(input, prepared) {
      const original = textWords(input);
      if (!original.length) return true;
      const tagged = textWords(prepared);
      let found = 0;
      let taggedIndex = 0;
      for (const word of original) {
        while (taggedIndex < tagged.length && tagged[taggedIndex] !== word) taggedIndex += 1;
        if (taggedIndex >= tagged.length) continue;
        found += 1;
        taggedIndex += 1;
      }
      const ratio = found / original.length;
      const tailPreserved = original.length < 3 || tagged.includes(original[original.length - 1]);
      return ratio >= 0.97 && tailPreserved;
    }

    function bracketTags(value) {
      return String(value || '').match(/\[[^\]\n]{1,80}\]/g) || [];
    }

    function stripPrefixIgnoreCase(value, prefix) {
      return value.toLowerCase().startsWith(prefix.toLowerCase())
        ? value.slice(prefix.length)
        : null;
    }

    function isBareCueDelimiter(ch) {
      return /[:;,.\-!?\s]/.test(ch || '');
    }

    function cleanBareCue(value) {
      let cue = String(value || '');
      while (cue && isBareCueDelimiter(cue[0])) cue = cue.slice(1);
      while (cue && isBareCueDelimiter(cue[cue.length - 1])) cue = cue.slice(0, -1);
      return cue.trim();
    }

    function looksLikeBarePerformanceCue(cue, prep) {
      const lower = cleanBareCue(cue).toLowerCase();
      const words = textWords(lower);
      if (!words.length || words.length > 5) return false;
      const palette = new Set((prep?.tagPalette || []).map((tag) => String(tag).toLowerCase()));
      if (palette.has(lower)) return true;
      const cueWords = new Set([
        'affectionate', 'amused', 'angry', 'breathless', 'calm', 'chuckle', 'chuckles',
        'deadpan', 'dryly', 'exhale', 'exhales', 'fearful', 'flatly', 'frustrated',
        'gasp', 'gasps', 'hesitates', 'laugh', 'laughing', 'laughs', 'leans',
        'lowers', 'kiss', 'kisses', 'kissing', 'lips', 'moan', 'moans', 'nervous', 'pause', 'proud', 'relieved', 'reassuring',
        'scoffs', 'serious', 'shaky', 'sigh', 'sighs', 'sleepy', 'smile',
        'smiles', 'smiling', 'softly', 'sorrowful', 'swallows', 'tender',
        'teasing', 'urgent', 'vulnerable', 'warmly', 'whisper', 'whispers', 'wistful'
      ]);
      return words.some((word) => cueWords.has(word));
    }

    function barePerformanceCuePhrases(prep) {
      const phrases = [
        'smiles softly', 'smiles and lowers my voice', 'smiles and lowers her voice',
        'smiles and lowers his voice', 'smiles and lowers their voice',
        'lowers my voice', 'lowers her voice', 'lowers his voice', 'lowers their voice',
        'leans over and kisses your lips softly', 'leans over and kisses her lips softly',
        'leans over and kisses his lips softly', 'leans over and kisses their lips softly',
        'leans over and kisses you softly', 'leans over and kisses her softly',
        'leans over and kisses him softly', 'leans over and kisses them softly',
        'laughs softly', 'chuckles softly', 'sighs softly',
        'whispers softly', 'smiles', 'smiling', 'laughs', 'laughing', 'chuckles',
        'sighs', 'sigh', 'whispers', 'gasps', 'exhales', 'moans', 'hesitates',
        'swallows', 'voice breaks', 'leans closer', 'under breath', 'softly',
        'warmly', 'dryly', 'flatly'
      ];
      for (const tag of prep?.tagPalette || []) {
        if (looksLikeBarePerformanceCue(tag, prep)) phrases.push(String(tag).toLowerCase());
      }
      return [...new Set(phrases)].sort((a, b) => b.length - a.length);
    }

    function preservedTextStart(input, prepared) {
      const original = textWords(input).slice(0, 3);
      if (!original.length) return null;
      const preparedWords = textWordSpans(prepared);
      for (let index = 0; index < preparedWords.length; index += 1) {
        let matched = true;
        for (let offset = 0; offset < original.length; offset += 1) {
          if (preparedWords[index + offset]?.word !== original[offset]) {
            matched = false;
            break;
          }
        }
        if (matched) return preparedWords[index].start;
      }
      return null;
    }

    function repairLeadingBareCue(input, prepared, prep) {
      const value = String(prepared || '');
      const leading = value.match(/^\s*/)?.[0] || '';
      const trimmed = value.slice(leading.length);
      const sourceStart = preservedTextStart(input, trimmed);
      if (!sourceStart) return prepared;
      const cue = cleanBareCue(trimmed.slice(0, sourceStart));
      if (!cue || !looksLikeBarePerformanceCue(cue, prep)) return prepared;
      const body = trimmed.slice(sourceStart).trimStart();
      if (!body) return prepared;
      const repaired = `${leading}[${cue}] ${body}`;
      return performanceTagsPreserveText(input, repaired) ? repaired : prepared;
    }

    function isSentenceBoundary(value, index) {
      if (index === 0) return true;
      const prefix = value.slice(0, index);
      let sawNewline = false;
      let cursor = prefix.length - 1;
      while (cursor >= 0 && /\s/.test(prefix[cursor])) {
        sawNewline = sawNewline || prefix[cursor] === '\n';
        cursor -= 1;
      }
      if (sawNewline) return true;
      return /[.!?]/.test(prefix[cursor] || '');
    }

    function isInsideBracketTag(value, index) {
      const prefix = value.slice(0, index);
      const open = prefix.lastIndexOf('[');
      return open >= 0 && prefix.slice(open).indexOf(']') < 0;
    }

    function cueTrailingDelimiterLength(value) {
      let length = 0;
      let sawSeparator = false;
      for (let index = 0; index < value.length; index += 1) {
        const ch = value[index];
        if (/[:,.\-!?\s]/.test(ch)) {
          length = index + 1;
          sawSeparator = true;
          continue;
        }
        break;
      }
      return sawSeparator ? length : null;
    }

    function repairSentenceBoundaryBareCues(input, prepared, prep) {
      const originalLower = String(input || '').toLowerCase();
      const phrases = barePerformanceCuePhrases(prep);
      let repaired = String(prepared || '');
      for (let attempt = 0; attempt < 8; attempt += 1) {
        let changed = false;
        outer:
        for (let index = 0; index < repaired.length; index += 1) {
          if (!isSentenceBoundary(repaired, index) || isInsideBracketTag(repaired, index)) continue;
          const rest = repaired.slice(index);
          for (const phrase of phrases) {
            if (originalLower.includes(phrase)) continue;
            const after = stripPrefixIgnoreCase(rest, phrase);
            if (after === null) continue;
            const afterLength = cueTrailingDelimiterLength(after);
            if (afterLength === null) continue;
            const candidate = `${repaired.slice(0, index)}[${phrase}] ${repaired.slice(index + phrase.length + afterLength).trimStart()}`;
            if (!performanceTagsPreserveText(input, candidate)) continue;
            repaired = candidate;
            changed = true;
            break outer;
          }
        }
        if (!changed) break;
      }
      return repaired;
    }

    function repairBareLeadingPerformanceCue(input, prepared, prep) {
      if (String(input || '').trim() === String(prepared || '').trim()) {
        return prepared;
      }
      return repairSentenceBoundaryBareCues(input, repairLeadingBareCue(input, prepared, prep), prep);
    }

    function performanceTagsAreValid(input, prepared) {
      if (!bracketTags(prepared).length && String(input || '').trim() !== String(prepared || '').trim()) {
        return false;
      }
      return performanceTagsPreserveText(input, prepared);
    }

    function fallbackPerformanceTag(input, prep) {
      const palette = new Set((prep?.tagPalette || []).map((tag) => String(tag).toLowerCase()));
      const lower = String(input || '').toLowerCase();
      const candidates = [
        ['whispers', ['whisper', 'hushed', 'under her breath', 'under his breath']],
        ['sigh of relief', ['relief', 'relieved', 'finally breathe', 'safe at last']],
        ['laughs', ['laugh', 'laughed', 'laughing']],
        ['light chuckle', ['smile', 'smiled', 'grin', 'amused']],
        ['fearful', ['fear', 'afraid', 'terrified', 'dread', 'panic']],
        ['nervous', ['tremor', 'trembling', 'anxious', 'nervous']],
        ['angry', ['angry', 'furious', 'rage', 'outraged']],
        ['sorrowful', ['sorrow', 'grief', 'tears', 'wept', 'crying', 'mourning']],
        ['wistful', ['remembered', 'memory', 'longed', 'missed', 'nostalgia']],
        ['frustrated', ['frustrated', 'irritated', 'annoyed', 'stuck']],
        ['reassuring', ['safe', 'steady', 'promise', 'trust', 'breathe']],
        ['tender', ['tender', 'gentle', 'soft', 'carefully', 'held', 'kiss', 'kisses', 'kissing', 'lips', 'leans over']],
        ['urgent', ['hurry', 'urgent', 'quickly', 'now', 'immediately']],
        ['breathless', ['breathless', 'gasped', 'panting', 'ran']],
        ['proud', ['proud', 'triumph', 'victory', 'accomplished']],
        ['excited', ['excited', 'thrilled', 'delighted', 'eager']]
      ];
      return candidates.find(([tag, needles]) => palette.has(tag) && needles.some((needle) => lower.includes(needle)))?.[0] || null;
    }

    function fallbackPerformanceTags(input, prep, strategy) {
      if (prep?.mode !== 'performance-tags' || strategy !== 'inline-tags') return null;
      if (bracketTags(input).length) return null;
      const tag = fallbackPerformanceTag(input, prep);
      if (!tag) return null;
      const tagged = `[${tag}] ${String(input || '').trimStart()}`;
      if (Array.from(tagged).length > prep.maxLength) return null;
      if (!performanceTagsAreValid(input, tagged)) return null;
      return tagged;
    }

    function styleInstructionIsValid(input, instruction) {
      const trimmed = String(instruction || '').trim();
      if (Array.from(trimmed).length > 300) return false;
      if (trimmed.includes('[') || trimmed.includes(']') || trimmed.includes('```')) return false;
      if (/^(delivery instruction:|instruction:|here)/i.test(trimmed)) return false;
      if (textWords(trimmed).length < 3) return false;
      if (textWords(input).length >= 8 && preservationRatio(input, trimmed) > 0.45) return false;
      return true;
    }

    function preservationRatio(input, prepared) {
      const original = textWords(input);
      if (!original.length) return 1;
      const output = textWords(prepared);
      let found = 0;
      let outputIndex = 0;
      for (const word of original) {
        while (outputIndex < output.length && output[outputIndex] !== word) outputIndex += 1;
        if (outputIndex >= output.length) continue;
        found += 1;
        outputIndex += 1;
      }
      return found / original.length;
    }

    async function providerError(response, fallback) {
      let text = '';
      try {
        text = await response.text();
      } catch (_) {}
      const error = new Error(text ? `${fallback}: ${text}` : `${fallback} (${response.status})`);
      error.status = response.status;
      return error;
    }

    function speechPrepModels(prep) {
      const seen = new Set();
      const models = [];
      for (const model of [prep.model, ...(prep.fallbackModels || [])]) {
        const normalized = prep.provider === 'google' ? normalizeGoogleModelName(model) : String(model || '').replace(/^codex\//, '');
        if (!normalized || seen.has(normalized)) continue;
        seen.add(normalized);
        models.push(model);
      }
      return models;
    }

    function speechPrepAttemptTimeoutMs(prep) {
      const configured = Number(prep.attemptTimeoutMs) || defaultSpeechPrepAttemptTimeoutMs;
      const overall = Number(prep.timeoutMs) || 30000;
      return Math.max(250, Math.min(configured, overall));
    }

    function speechPrepErrorIsRetryable(error) {
      if (error?.status) return error.status === 429 || error.status >= 500;
      return error?.name === 'AbortError' || error?.name === 'TypeError';
    }

    function base64UrlJson(segment) {
      const normalized = String(segment || '').replace(/-/g, '+').replace(/_/g, '/');
      const padded = normalized + '='.repeat((4 - normalized.length % 4) % 4);
      return JSON.parse(atob(padded));
    }

    function codexAccessTokenNeedsRefresh(auth) {
      try {
        const payload = base64UrlJson(String(auth?.accessToken || '').split('.')[1]);
        return !Number.isFinite(payload.exp) || payload.exp <= Math.floor(Date.now() / 1000) + 300;
      } catch (_) {
        return true;
      }
    }

    async function refreshCodexAuth(prep) {
      const auth = prep.codexAuth;
      if (!auth?.refreshToken) throw new Error('Codex auth is missing a refresh token.');
      const response = await fetch(auth.tokenUrl || 'https://auth.openai.com/oauth/token', {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: new URLSearchParams({
          grant_type: 'refresh_token',
          refresh_token: auth.refreshToken,
          client_id: auth.clientId || 'app_EMoamEEZ73f0CkXaXp7hrann'
        })
      });
      if (!response.ok) throw await providerError(response, 'Codex auth refresh failed');
      const json = await response.json();
      prep.codexAuth = {
        ...auth,
        accessToken: json.access_token || auth.accessToken,
        refreshToken: json.refresh_token || auth.refreshToken,
        accountId: json.account_id || auth.accountId
      };
      if (directConfig?.speechPrep === prep) {
        directConfig.speechPrep = prep;
        localStorage.setItem(configStorageKey, JSON.stringify(directConfig));
      }
      return prep.codexAuth;
    }

    async function ensureCodexAuth(prep) {
      const auth = prep.codexAuth;
      if (!auth?.accessToken || !auth?.accountId) throw new Error('Codex auth is not cached.');
      if (!codexAccessTokenNeedsRefresh(auth)) return auth;
      return await refreshCodexAuth(prep);
    }

    function codexResponseBody(prep, model, prompt) {
      const body = {
        model: String(model || '').replace(/^codex\//, ''),
        store: false,
        stream: true,
        instructions: 'You are running non-interactively as a text transformation task. Do not use tools. Do not ask questions. Return only the transformed text.',
        input: [{
          type: 'message',
          role: 'user',
          content: [{ type: 'input_text', text: prompt }]
        }],
        text: { verbosity: 'low' },
        parallel_tool_calls: false
      };
      if (prep.reasoningEffort && prep.reasoningEffort !== 'none') {
        body.reasoning = { effort: prep.reasoningEffort };
      }
      return body;
    }

    function parseCodexSse(text) {
      let outputText = '';
      let completed = null;
      for (const line of String(text || '').split(/\r?\n/)) {
        if (!line.startsWith('data:')) continue;
        const data = line.slice(5).trim();
        if (!data || data === '[DONE]') continue;
        const event = JSON.parse(data);
        if (event.type === 'response.output_text.delta' && typeof event.delta === 'string') {
          outputText += event.delta;
        } else if (event.type === 'response.completed' && event.response) {
          completed = event.response;
        } else if (event.type === 'response.failed' || event.type === 'response.incomplete') {
          throw new Error(`Codex prep ended with ${event.type}`);
        }
      }
      if (completed?.output_text) return completed.output_text;
      if (outputText) return outputText;
      const parts = [];
      for (const item of completed?.output || []) {
        if (item?.type !== 'message') continue;
        for (const block of item.content || []) {
          if ((block.type === 'output_text' || block.type === 'text') && block.text) parts.push(block.text);
        }
      }
      return parts.join('');
    }

    async function fetchCodexPrepAttempt(prep, model, prompt, signal) {
      const send = async (auth) => fetch(`${String(prep.baseUrl || '').replace(/\/$/, '').replace(/\/responses$/, '')}/responses`, {
        method: 'POST',
        signal,
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${auth.accessToken}`,
          'chatgpt-account-id': auth.accountId,
          'originator': 'codex-voice-web',
          'User-Agent': 'codex-voice-web',
          'OpenAI-Beta': 'responses=experimental',
          'Accept': 'text/event-stream'
        },
        body: JSON.stringify(codexResponseBody(prep, model, prompt))
      });
      let response = await send(await ensureCodexAuth(prep));
      if (response.status === 401 || response.status === 403) {
        response = await send(await refreshCodexAuth(prep));
      }
      return response;
    }

    async function fetchGooglePrepAttempt(prep, model, body, signal) {
      return await fetch(`${prep.baseUrl}/models/${encodeURIComponent(normalizeGoogleModelName(model))}:generateContent`, {
        method: 'POST',
        signal,
        headers: {
          'Content-Type': 'application/json',
          'x-goog-api-key': prep.apiKey
        },
        body: JSON.stringify(body)
      });
    }

    async function fetchSpeechPrepAttempt(prep, model, body, prompt, signal) {
      if (prep.provider === 'codex') return await fetchCodexPrepAttempt(prep, model, prompt, signal);
      return await fetchGooglePrepAttempt(prep, model, body, signal);
    }

    async function prepareForProvider(config, provider, input, persona, prepCache) {
      const basePrep = browserSpeechPrepForDirect(config);
      const maxTextLength = providerMaxTextLength(config, provider);
      const mustShorten = Array.from(input).length > maxTextLength;
      const prep = mustShorten ? speechPrepForProviderLimit(basePrep, maxTextLength) : basePrep;
      const strategy = speechPrepStrategy({ ...config, speechPrep: prep }, provider);
      const decision = prepareDecision(input, prep, strategy);
      if (!decision.shouldPrepare) {
        return { input, instructions: null, changed: false, skipped: true, reason: decision.reason, strategy, elapsedMs: 0 };
      }
      const cacheKey = `${prep.provider}\n${prep.mode}\n${prep.maxLength}\n${strategy}\n${input}`;
      if (prepCache?.has(cacheKey)) return prepCache.get(cacheKey);
      if (!prep.browserSupported) {
        const result = {
          input,
          instructions: null,
          changed: false,
          skipped: true,
          error: prep.mode === 'shorten'
            ? 'Configured summarization prep is server-only.'
            : 'Configured emotion prep is server-only.',
          strategy,
          elapsedMs: 0
        };
        prepCache?.set(cacheKey, result);
        return result;
      }
      if (prep.provider === 'google' && !prep.apiKey) {
        throw new Error('Google emotion prep is missing an API key.');
      }
      if (prep.provider === 'codex' && !prep.codexAuth?.accessToken) {
        throw new Error('Codex emotion prep is missing cached auth.');
      }
      const startedAt = performance.now();
      const prepInput = prep.mode === 'shorten'
        ? truncateToChars(input, Number(prep.maxInputLength) || Infinity)
        : input;
      const prompt = prep.mode === 'shorten'
        ? buildShortenPrompt(prepInput, prep)
        : strategy === 'style-instruction'
          ? buildStyleInstructionPrompt(prepInput, prep, persona)
          : buildPerformanceTagsPrompt(prepInput, prep, persona);
      const body = {
        contents: [{ role: 'user', parts: [{ text: prompt }] }],
        generationConfig: {
          temperature: prep.mode === 'shorten' ? 0.2 : 0.45,
          maxOutputTokens: prep.mode === 'shorten'
            ? clamp(Math.floor(prep.maxLength / 3), 64, 4096)
            : strategy === 'style-instruction'
              ? 128
              : performanceTagsOutputTokens(prepInput, prep),
          ...(prep.mode === 'performance-tags' ? {
            thinkingConfig: {
              thinkingLevel: 'MINIMAL'
            }
          } : {})
        }
      };
      const overallTimeoutMs = Number(prep.timeoutMs) || 30000;
      const attemptTimeoutMs = speechPrepAttemptTimeoutMs(prep);
      let lastError = null;
      try {
        for (const model of speechPrepModels(prep)) {
          const remainingMs = overallTimeoutMs - elapsedMs(startedAt);
          if (remainingMs <= 0) break;
          let response;
          const controller = new AbortController();
          const timer = setTimeout(() => controller.abort(), Math.min(attemptTimeoutMs, remainingMs));
          try {
            response = await fetchSpeechPrepAttempt(prep, model, body, prompt, controller.signal);
            if (!response.ok) {
              const error = await providerError(response, 'Emotion prep failed');
              lastError = error;
              console.warn(error.message);
              if (speechPrepErrorIsRetryable(error)) continue;
              const result = { input, instructions: null, changed: false, error: error.message, strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            let prepared = prep.provider === 'codex'
              ? parseCodexSse(await response.text()).trim()
              : extractTextOutput(await response.json()).trim();
            if (!prepared) {
              const result = { input, instructions: null, changed: false, error: 'Emotion prep returned no text.', strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            if (prep.mode === 'performance-tags' && strategy === 'inline-tags') {
              prepared = repairBareLeadingPerformanceCue(input, prepared, prep);
            }
            if (prep.mode === 'performance-tags' && strategy === 'inline-tags' && Array.from(prepared).length > prep.maxLength) {
              const result = { input, instructions: null, changed: false, error: 'Emotion prep returned text above the configured limit.', strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            if (prep.mode === 'performance-tags' && strategy === 'inline-tags' && !performanceTagsAreValid(input, prepared)) {
              const result = { input, instructions: null, changed: false, error: 'Emotion prep changed the text too much, so it was ignored.', strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            if (prep.mode === 'performance-tags' && strategy === 'style-instruction' && !styleInstructionIsValid(input, prepared)) {
              const result = { input, instructions: null, changed: false, error: 'Emotion prep returned an invalid delivery instruction, so it was ignored.', strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            const output = prep.mode === 'shorten'
              ? Array.from(prepared).slice(0, prep.maxLength).join('')
              : prepared;
            if (prep.mode === 'shorten' && Array.from(output).length < shortenMinOutputChars(input, prep)) {
              const extracted = extractiveShortenToFit(prepInput, prep.maxLength);
              const result = { input: extracted, instructions: null, changed: extracted !== input, warning: 'Summarization returned text below the minimum length, so a fitted source excerpt was used.', strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            if (strategy === 'style-instruction') {
              const result = { input, instructions: output, changed: false, strategy, elapsedMs: elapsedMs(startedAt) };
              prepCache?.set(cacheKey, result);
              return result;
            }
            const result = { input: output, instructions: null, changed: output !== input, strategy, elapsedMs: elapsedMs(startedAt) };
            prepCache?.set(cacheKey, result);
            return result;
          } catch (error) {
            if (prep.provider === 'codex' && error?.name === 'TypeError') {
              error.message = 'Codex direct emotion prep is blocked by the browser or network.';
            }
            lastError = error;
            if (speechPrepErrorIsRetryable(error)) continue;
            throw error;
          } finally {
            clearTimeout(timer);
          }
        }
        if (lastError) {
          const fallback = fallbackPerformanceTags(input, prep, strategy);
          if (fallback) {
            const result = {
              input: fallback,
              instructions: null,
              changed: true,
              warning: lastError?.message || 'Emotion prep failed, so a local sparse performance tag was used.',
              strategy,
              elapsedMs: elapsedMs(startedAt)
            };
            prepCache?.set(cacheKey, result);
            return result;
          }
          const result = {
            input,
            instructions: null,
            changed: false,
            error: lastError?.message || 'Emotion prep failed after retries.',
            strategy,
            elapsedMs: elapsedMs(startedAt)
          };
          prepCache?.set(cacheKey, result);
          return result;
        }
        const fallback = fallbackPerformanceTags(input, prep, strategy);
        if (fallback) {
          const result = {
            input: fallback,
            instructions: null,
            changed: true,
            warning: 'Emotion prep timed out, so a local sparse performance tag was used.',
            strategy,
            elapsedMs: elapsedMs(startedAt)
          };
          prepCache?.set(cacheKey, result);
          return result;
        }
        const result = { input, instructions: null, changed: false, error: 'Emotion prep timed out before a model returned text.', strategy, elapsedMs: elapsedMs(startedAt) };
        prepCache?.set(cacheKey, result);
        return result;
      } catch (error) {
        console.warn(error);
        const fallback = fallbackPerformanceTags(input, prep, strategy);
        if (fallback) {
          const result = {
            input: fallback,
            instructions: null,
            changed: true,
            warning: error?.message || 'Emotion prep failed, so a local sparse performance tag was used.',
            strategy,
            elapsedMs: elapsedMs(startedAt)
          };
          prepCache?.set(cacheKey, result);
          return result;
        }
        const result = {
          input,
          instructions: null,
          changed: false,
          error: error?.message || 'Emotion prep failed.',
          strategy,
          elapsedMs: elapsedMs(startedAt)
        };
        prepCache?.set(cacheKey, result);
        return result;
      }
    }

    function extractTextOutput(json) {
      const parts = json?.candidates?.[0]?.content?.parts || [];
      return parts.map((part) => part.text || '').filter(Boolean).join(' ');
    }

    function buildGoogleTtsPrompt(input, persona, instructions) {
      let prompt = 'Read the following text aloud.\n\n';
      if (persona) {
        prompt += 'Delivery profile:\n';
        if (persona.promptScene) prompt += `- scene: ${persona.promptScene}\n`;
        if (persona.promptStyle) prompt += `- style: ${persona.promptStyle}\n`;
        if (persona.promptPacing) prompt += `- pace: ${persona.promptPacing}\n`;
        for (const constraint of persona.promptConstraints || []) {
          prompt += `- constraint: ${constraint}\n`;
        }
        prompt += '\n';
        if (persona.promptSampleContext) {
          prompt += `Sample context: ${persona.promptSampleContext}\n\n`;
        }
      }
      if (instructions) {
        prompt += 'Additional delivery hints:\n';
        prompt += `- ${instructions}\n\n`;
      }
      prompt += 'Important:\n';
      prompt += '- speak the text exactly as written\n';
      prompt += '- do not add narration or commentary\n';
      prompt += '- do not change wording or paraphrase\n\n';
      prompt += `Text:\n"""${input}"""`;
      return prompt;
    }

    function parseSampleRate(mimeType) {
      const match = /rate=(\d+)/i.exec(mimeType || '');
      return match ? Number(match[1]) : 24000;
    }

    function writeAscii(view, offset, value) {
      for (let i = 0; i < value.length; i += 1) {
        view.setUint8(offset + i, value.charCodeAt(i));
      }
    }

    function wavBlobFromPcm(pcmBytes, sampleRate, channels = 1) {
      const header = new ArrayBuffer(44);
      const view = new DataView(header);
      const blockAlign = channels * 2;
      writeAscii(view, 0, 'RIFF');
      view.setUint32(4, 36 + pcmBytes.length, true);
      writeAscii(view, 8, 'WAVE');
      writeAscii(view, 12, 'fmt ');
      view.setUint32(16, 16, true);
      view.setUint16(20, 1, true);
      view.setUint16(22, channels, true);
      view.setUint32(24, sampleRate, true);
      view.setUint32(28, sampleRate * blockAlign, true);
      view.setUint16(32, blockAlign, true);
      view.setUint16(34, 16, true);
      writeAscii(view, 36, 'data');
      view.setUint32(40, pcmBytes.length, true);
      return new Blob([header, pcmBytes], { type: 'audio/wav' });
    }

    const ttsChunkMinChars = 1600;
    const ttsChunkMaxChars = 900;
    const ttsChunkBoundarySilenceMs = 180;

    function splitTtsText(input, maxChars = ttsChunkMaxChars) {
      const chunks = [];
      let remaining = String(input || '').trim();
      while (Array.from(remaining).length > maxChars) {
        const splitAt = splitIndexAtOrBefore(remaining, maxChars);
        const head = remaining.slice(0, splitAt).trim();
        if (head) chunks.push(head);
        remaining = remaining.slice(splitAt).trimStart();
      }
      if (remaining) chunks.push(remaining);
      return chunks;
    }

    function splitIndexAtOrBefore(input, maxChars) {
      const chars = Array.from(input);
      const hardLimit = chars.slice(0, maxChars).join('').length;
      const prefix = input.slice(0, hardLimit);
      for (const pattern of ['. ', '! ', '? ', '\n\n', '\n', '; ', ', ', ' ']) {
        const index = prefix.lastIndexOf(pattern);
        if (index >= 0) return index + pattern.length;
      }
      return hardLimit;
    }

    function concatUint8Arrays(parts) {
      const total = parts.reduce((sum, part) => sum + part.length, 0);
      const output = new Uint8Array(total);
      let offset = 0;
      for (const part of parts) {
        output.set(part, offset);
        offset += part.length;
      }
      return output;
    }

    function pcmBoundarySilence(sampleRate, channels = 1) {
      const frames = Math.floor((sampleRate * ttsChunkBoundarySilenceMs) / 1000);
      return new Uint8Array(frames * channels * 2);
    }

    function concatPcmChunksWithBoundarySilence(parts, sampleRate, channels = 1) {
      if (parts.length <= 1) return concatUint8Arrays(parts);
      const silence = pcmBoundarySilence(sampleRate, channels);
      const interleaved = [];
      parts.forEach((part, index) => {
        if (index > 0) interleaved.push(silence);
        interleaved.push(part);
      });
      return concatUint8Arrays(interleaved);
    }

    function asciiFromBytes(bytes, offset, length) {
      let value = '';
      for (let i = 0; i < length; i += 1) value += String.fromCharCode(bytes[offset + i]);
      return value;
    }

    function wavPcmData(bytes) {
      if (bytes.length < 44) throw new Error('WAV chunk is too small.');
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      if (asciiFromBytes(bytes, 0, 4) !== 'RIFF' || asciiFromBytes(bytes, 8, 4) !== 'WAVE') {
        throw new Error('WAV chunk has invalid RIFF/WAVE header.');
      }
      let offset = 12;
      let format = null;
      let data = null;
      while (offset + 8 <= bytes.length) {
        const id = asciiFromBytes(bytes, offset, 4);
        const size = view.getUint32(offset + 4, true);
        const body = offset + 8;
        if (body + size > bytes.length) break;
        if (id === 'fmt ') {
          format = {
            audioFormat: view.getUint16(body, true),
            channels: view.getUint16(body + 2, true),
            sampleRate: view.getUint32(body + 4, true),
            bitsPerSample: view.getUint16(body + 14, true)
          };
        } else if (id === 'data') {
          data = bytes.slice(body, body + size);
        }
        offset = body + size + (size % 2);
      }
      if (!format || !data) throw new Error('WAV chunk is missing fmt or data.');
      if (format.audioFormat !== 1 || format.bitsPerSample !== 16) {
        throw new Error('Only 16-bit PCM WAV chunks can be stitched.');
      }
      return { ...format, data };
    }

    function concatWavChunksWithBoundarySilence(parts) {
      const decoded = parts.map(wavPcmData);
      const first = decoded[0];
      if (!first) throw new Error('No WAV chunks to stitch.');
      for (const chunk of decoded) {
        if (chunk.sampleRate !== first.sampleRate || chunk.channels !== first.channels) {
          throw new Error('WAV chunks have mismatched sample rates or channels.');
        }
      }
      const pcm = concatPcmChunksWithBoundarySilence(
        decoded.map((chunk) => chunk.data),
        first.sampleRate,
        first.channels
      );
      return wavBlobFromPcm(pcm, first.sampleRate, first.channels);
    }

    async function fetchGoogleAudio(config, input, persona, instructions) {
      const google = config.providers?.google;
      if (!google) throw new Error('Google TTS is not configured.');
      const model = resolveGoogleModel(google);
      const voiceName = persona?.google?.voiceName || google.voice;
      const response = await fetch(`${google.baseUrl}/models/${encodeURIComponent(model)}:generateContent`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'x-goog-api-key': google.apiKey
        },
        body: JSON.stringify({
          contents: [{ role: 'user', parts: [{ text: buildGoogleTtsPrompt(input, persona, instructions) }] }],
          generationConfig: {
            responseModalities: ['AUDIO'],
            speechConfig: {
              voiceConfig: {
                prebuiltVoiceConfig: { voiceName }
              }
            }
          }
        })
      });
      if (!response.ok) throw await providerError(response, 'Google TTS failed');
      const json = await response.json();
      const parts = json?.candidates?.[0]?.content?.parts || [];
      const inline = parts.map((part) => part.inlineData || part.inline_data).find(Boolean);
      if (!inline?.data) throw new Error('Google TTS returned no audio.');
      const mimeType = inline.mimeType || inline.mime_type || 'audio/L16;codec=pcm;rate=24000';
      const bytes = bytesFromBase64(inline.data);
      return { bytes, mimeType };
    }

    async function synthesizeGoogle(config, input, persona, instructions) {
      if (Array.from(input).length >= ttsChunkMinChars) {
        const chunks = splitTtsText(input);
        if (chunks.length > 1) {
          const audios = [];
          for (const chunk of chunks) {
            audios.push(await fetchGoogleAudio(config, chunk, persona, instructions));
          }
          const mimeType = audios[0].mimeType || 'audio/L16;codec=pcm;rate=24000';
          const sampleRate = parseSampleRate(mimeType);
          if (audios.every((audio) => (audio.mimeType || '').toLowerCase().startsWith('audio/l16') || (audio.mimeType || '').toLowerCase().startsWith('audio/pcm'))) {
            return wavBlobFromPcm(concatPcmChunksWithBoundarySilence(audios.map((audio) => audio.bytes), sampleRate), sampleRate);
          }
          if (audios.every((audio) => (audio.mimeType || '').toLowerCase().startsWith('audio/wav'))) {
            return concatWavChunksWithBoundarySilence(audios.map((audio) => audio.bytes));
          }
          return new Blob(audios.map((audio) => audio.bytes), { type: mimeType });
        }
      }
      const { bytes, mimeType } = await fetchGoogleAudio(config, input, persona, instructions);
      if (mimeType.toLowerCase().startsWith('audio/l16') || mimeType.toLowerCase().startsWith('audio/pcm')) {
        return wavBlobFromPcm(bytes, parseSampleRate(mimeType));
      }
      return new Blob([bytes], { type: mimeType });
    }

    function resolveElevenLabsSpeed(persona) {
      const speed = persona?.elevenlabs?.voiceSettings?.speed;
      return Math.round(clamp(Number.isFinite(speed) ? speed : 1.0, 0.7, 1.2) * 100) / 100;
    }

    function elevenLabsMimeType(outputFormat) {
      if ((outputFormat || '').startsWith('wav')) return 'audio/wav';
      if ((outputFormat || '').startsWith('pcm')) return 'audio/pcm';
      if ((outputFormat || '').startsWith('opus')) return 'audio/opus';
      return 'audio/mpeg';
    }

    function elevenLabsSampleRate(outputFormat) {
      const match = /^pcm_(\d+)/i.exec(outputFormat || '');
      return match ? Number(match[1]) : 24000;
    }

    async function synthesizeElevenLabsSingle(config, input, persona, outputFormatOverride = null, rawPcm = false) {
      const elevenlabs = config.providers?.elevenlabs;
      if (!elevenlabs) throw new Error('ElevenLabs TTS is not configured.');
      const voiceId = persona?.elevenlabs?.voiceId;
      if (!voiceId) throw new Error('ElevenLabs voice_id is not configured for this persona.');
      const voiceSettings = persona?.elevenlabs?.voiceSettings
        ? { ...persona.elevenlabs.voiceSettings, speed: resolveElevenLabsSpeed(persona) }
        : { speed: 1.0 };
      const outputFormat = outputFormatOverride || elevenlabs.outputFormat;
      const url = `${elevenlabs.baseUrl}/v1/text-to-speech/${encodeURIComponent(voiceId)}?output_format=${encodeURIComponent(outputFormat)}`;
      const response = await fetch(url, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'xi-api-key': elevenlabs.apiKey
        },
        body: JSON.stringify({
          text: input,
          model_id: resolveElevenLabsModel(elevenlabs),
          voice_settings: voiceSettings,
          language_code: elevenlabs.languageCode,
          apply_text_normalization: elevenlabs.applyTextNormalization
        })
      });
      if (!response.ok) throw await providerError(response, 'ElevenLabs TTS failed');
      const bytes = await response.arrayBuffer();
      if ((outputFormat || '').startsWith('pcm') && !rawPcm) {
        return wavBlobFromPcm(new Uint8Array(bytes), elevenLabsSampleRate(outputFormat));
      }
      return new Blob([bytes], {
        type: response.headers.get('content-type') || elevenLabsMimeType(outputFormat)
      });
    }

    async function synthesizeElevenLabs(config, input, persona) {
      if (Array.from(input).length >= ttsChunkMinChars) {
        const chunks = splitTtsText(input);
        if (chunks.length > 1) {
          const outputFormat = 'pcm_24000';
          const parts = [];
          for (const chunk of chunks) {
            const blob = await synthesizeElevenLabsSingle(config, chunk, persona, outputFormat, true);
            parts.push(new Uint8Array(await blob.arrayBuffer()));
          }
          const sampleRate = elevenLabsSampleRate(outputFormat);
          return wavBlobFromPcm(concatPcmChunksWithBoundarySilence(parts, sampleRate), sampleRate);
        }
      }
      return synthesizeElevenLabsSingle(config, input, persona);
    }

    function isRetryable(error) {
      if (!error?.status) return true;
      return error.status === 401 || error.status === 403 || error.status === 429 || error.status >= 500;
    }

    async function synthesizeProvider(config, provider, input, persona, prepCache) {
      setGenerateProgress(0.32, 'Preparing');
      let prep = await prepareForProvider(config, provider, input, persona, prepCache);
      if (prep.strategy === 'shorten' && prep.input !== input) {
        const performancePrep = await prepareForProvider(config, provider, prep.input, persona, prepCache);
        if (performancePrep.input !== prep.input || performancePrep.instructions) {
          prep = {
            ...performancePrep,
            shortened: prep,
            elapsedMs: (prep.elapsedMs || 0) + (performancePrep.elapsedMs || 0)
          };
        }
      }
      const ttsStartedAt = performance.now();
      setGenerateProgress(0.64, 'Synthesizing');
      const blob = provider === 'google'
        ? await synthesizeGoogle(config, prep.input, persona, prep.instructions)
        : await synthesizeElevenLabs(config, prep.input, persona);
      return {
        blob,
        input: prep.input,
        inputChanged: prep.input !== input,
        provider,
        prep,
        ttsElapsedMs: elapsedMs(ttsStartedAt)
      };
    }

    async function generateDirect(input) {
      const config = directConfig;
      const prepCache = new Map();
      const selectedProvider = settings.provider !== 'auto' ? settings.provider : null;
      const primary = selectedProvider || resolveProvider(config, resolvePersona(config, null));
      const persona = resolvePersona(config, primary);
      try {
        return await synthesizeProvider(config, primary, input, persona, prepCache);
      } catch (error) {
        if (!isRetryable(error) || persona?.fallbackPolicy !== 'preserve-persona') throw error;
        const fallback = fallbackProvider(primary);
        if (!config.providers?.[fallback]) throw error;
        return await synthesizeProvider(config, fallback, input, persona, prepCache);
      }
    }

    function canGenerateDirectWithConfiguredPrep(config) {
      return Boolean(config?.providers?.google || config?.providers?.elevenlabs);
    }

    function settingsMatchServerDefaults() {
      return settings.provider === 'auto'
        && settings.voice === 'default'
        && settings.model === 'default'
        && settings.emotionPreprocessing === true
        && settings.summarization === false;
    }

    function shouldPreferServerGeneration(config) {
      return config?.speechPrep?.provider === 'codex' && settingsMatchServerDefaults();
    }

    function serverGenerationUnavailable(error) {
      return error?.name === 'TypeError' || /failed to fetch/i.test(error?.message || '');
    }

    const serverJobPollMs = 1200;

    function sleep(ms) {
      return new Promise((resolve) => setTimeout(resolve, ms));
    }

    async function createWebSpeechJob(input) {
      const response = await fetch('/web/speech-jobs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ input })
      });
      if (!response.ok) {
        let message = `TTS job failed (${response.status})`;
        try {
          const json = await response.json();
          message = json?.error?.message || message;
        } catch (_) {}
        throw new Error(message);
      }
      const job = await response.json();
      if (!job?.id) throw new Error('TTS job did not return an id.');
      return job.id;
    }

    async function fetchWebSpeechJob(jobId) {
      const response = await fetch(`/web/speech-jobs/${encodeURIComponent(jobId)}`, { cache: 'no-store' });
      if (!response.ok) {
        let message = `TTS job status failed (${response.status})`;
        try {
          const json = await response.json();
          message = json?.error?.message || message;
        } catch (_) {}
        throw new Error(message);
      }
      return response.json();
    }

    async function waitForWebSpeechJob(jobId) {
      setGenerateProgress(0.82, 'Loading');
      for (;;) {
        const job = await fetchWebSpeechJob(jobId);
        if (job.status === 'complete' && job.result) return job.result;
        if (job.status === 'failed') throw new Error(job.error?.message || 'TTS job failed.');
        setGenerateProgress(0.64, 'Synthesizing');
        await sleep(serverJobPollMs);
      }
    }

    async function generateViaServer(input, jobId = null) {
      setGenerateProgress(0.35, jobId ? 'Resuming' : 'Preparing');
      const activeJobId = jobId || await createWebSpeechJob(input);
      savePendingGeneration(input, activeJobId);
      const result = await waitForWebSpeechJob(activeJobId);
      return {
        blob: audioBlobFromBase64(result.audio_base64, result.mime_type),
        input: result.input,
        inputChanged: Boolean(result.input_changed),
        provider: 'server',
        jobId: activeJobId
      };
    }

    async function runGeneration(input, resumeJobId = null) {
      generate.disabled = true;
      clear.disabled = true;
      play.disabled = true;
      generationActive = true;
      lifecycleInterruptedGeneration = false;
      savePendingGeneration(input, resumeJobId);
      clearError();
      setGenerating(true, 'Starting', 0.08);
      let resumeAfterLifecycleInterruption = false;
      try {
        let result;
        if (resumeJobId) {
          result = await generateViaServer(input, resumeJobId);
        } else if (directConfig && shouldPreferServerGeneration(directConfig)) {
          try {
            result = await generateViaServer(input);
          } catch (error) {
            if (!serverGenerationUnavailable(error) || !canGenerateDirectWithConfiguredPrep(directConfig)) throw error;
            setGenerateProgress(0.25, 'Direct');
            result = await generateDirect(input);
          }
        } else {
          setGenerateProgress(0.25, directConfig && canGenerateDirectWithConfiguredPrep(directConfig) ? 'Direct' : 'Server');
          result = directConfig && canGenerateDirectWithConfiguredPrep(directConfig)
            ? await generateDirect(input)
            : await generateViaServer(input);
        }
        if (
          typeof result.input === 'string'
          && result.input !== text.value
          && shouldApplyGeneratedText(input, result.input)
        ) {
          text.value = result.input;
          localStorage.setItem(textStorageKey, text.value);
          updateCount();
        }
        setGenerateProgress(0.9, 'Saving');
        loadAudioBlob(result.blob);
        await saveLastGeneratedAudio(result.blob, result.input, result.inputChanged);
        clearPendingGeneration();
        clearError();
        setGenerateProgress(1, 'Done');
      } catch (error) {
        resumeAfterLifecycleInterruption = shouldKeepPendingGeneration(error);
        if (!resumeAfterLifecycleInterruption) clearPendingGeneration();
        play.disabled = !audio.src;
        seek.disabled = !audio.src;
        if (!resumeAfterLifecycleInterruption) {
          showError(error.message || 'TTS failed.');
        }
      } finally {
        generationActive = false;
        generate.disabled = false;
        clear.disabled = false;
        setTimeout(() => {
          if (!generationActive) setGenerating(false, 'Generate', 0);
        }, 350);
        if (pendingWorkerReload) {
          pendingWorkerReload = false;
          serviceWorkerRefreshing = true;
          window.location.reload();
        }
        if (
          resumeAfterLifecycleInterruption
          && document.visibilityState === 'visible'
          && loadPendingGeneration()
        ) {
          resumePendingGeneration();
        }
      }
    }

    text.addEventListener('input', () => {
      localStorage.setItem(textStorageKey, text.value);
      updateCount();
    });

    window.addEventListener('pagehide', () => {
      if (generationActive) lifecycleInterruptedGeneration = true;
      localStorage.setItem(textStorageKey, text.value);
    });

    window.addEventListener('pageshow', (event) => {
      if (event.persisted && !audio.src) restoreLastGeneratedAudio();
      if (generationActive) setGenerating(true, 'Generating', 0.45);
      if (!generationActive && loadPendingGeneration()) resumePendingGeneration();
    });

    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState !== 'visible' && generationActive) {
        lifecycleInterruptedGeneration = true;
        return;
      }
      if (document.visibilityState === 'visible' && generationActive) setGenerating(true, 'Generating', 0.45);
      if (document.visibilityState === 'visible' && !generationActive && loadPendingGeneration()) {
        resumePendingGeneration();
      }
    });

    providerSelect.addEventListener('change', populateSettings);
    voiceSelect.addEventListener('change', saveSettings);
    modelSelect.addEventListener('change', saveSettings);
    emotion.addEventListener('change', saveSettings);
    summarize.addEventListener('change', saveSettings);

    settingsToggle.addEventListener('click', () => {
      const open = settingsPanel.hasAttribute('hidden');
      settingsPanel.toggleAttribute('hidden', !open);
      settingsToggle.setAttribute('aria-expanded', String(open));
    });

    paste.addEventListener('click', async () => {
      try {
        const value = await navigator.clipboard.readText();
        if (!value) return;
        text.value = value;
        localStorage.setItem(textStorageKey, text.value);
        updateCount();
        clearError();
        text.focus();
      } catch (error) {
        showError(error.message || 'Clipboard paste failed.');
      }
    });

    clear.addEventListener('click', async () => {
      text.value = '';
      localStorage.removeItem(textStorageKey);
      clearPendingGeneration();
      updateCount();
      resetAudio();
      await deleteLastGeneratedAudio();
      clearError();
      text.focus();
    });

    generate.addEventListener('click', async () => {
      const input = text.value.trim();
      if (!input) {
        showError('Enter some text first.');
        return;
      }
      await runGeneration(input);
    });

    play.addEventListener('click', async () => {
      if (!audio.src) return;
      if (audio.paused) {
        try {
          await audio.play();
        } catch (error) {
          showError(error.message || 'Playback failed.');
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
      playSvg(false);
      clearError();
    });
    audio.addEventListener('pause', () => {
      playSvg(true);
    });
    audio.addEventListener('ended', () => {
      playSvg(true);
      updatePosition();
    });
  </script>
</body>
</html>"##;

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
    apply_text_normalization: String,
    output_format: String,
    language_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_audio_tags: Option<bool>,
    max_text_length: usize,
    timeout_ms: u64,
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
                        apply_text_normalization: elevenlabs.apply_text_normalization.clone(),
                        output_format: elevenlabs.output_format.clone(),
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
    pub(crate) speech: Option<Arc<dyn SpeechClient>>,
    pub(crate) web_tts_config: Option<BrowserTtsConfig>,
    pub(crate) web_speech_jobs: WebSpeechJobStore,
    pub(crate) auth: ServiceAuth,
    pub(crate) codex_upload_limit_bytes: u64,
    pub(crate) client_upload_limit_bytes: u64,
    pub(crate) chunk_seconds: u64,
    pub(crate) ffmpeg_binary: String,
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

pub async fn serve(
    config: super::ServeConfig,
    speech: Option<Arc<dyn SpeechClient>>,
    tts_config: Option<ResolvedTtsConfig>,
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
        web_tts_config: tts_config.as_ref().map(BrowserTtsConfig::from_resolved),
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

async fn web_config(State(state): State<ServiceState>) -> Result<impl IntoResponse, ApiError> {
    let config = state
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

fn web_manifest_body() -> String {
    serde_json::json!({
        "name": "Codex Voice",
        "short_name": "Voice",
        "description": "Quick text-to-speech for Codex Voice.",
        "id": "/web",
        "start_url": "/web",
        "scope": "/web",
        "display": "standalone",
        "background_color": "#101214",
        "theme_color": "#101214",
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
        web_manifest_body(),
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
        assert!(html.contains(
            r#"<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover">"#
        ));
        assert!(html.contains("<textarea id=\"text\""));
        assert!(
            html.contains(r#"<img class="app-icon" src="/web/icon-192.png" alt="Codex Voice">"#)
        );
        assert!(!html.contains("<h1>Codex Voice</h1>"));
        assert!(html.contains("id=\"provider\""));
        assert!(html.contains("id=\"voice\""));
        assert!(html.contains("id=\"model\""));
        assert!(html.contains("id=\"emotion\""));
        assert!(html.contains("id=\"summarize\""));
        assert!(html.contains("id=\"generate\""));
        assert!(html.contains("id=\"generate-label\""));
        assert!(html.contains("id=\"clear\""));
        assert!(html.contains("id=\"paste\""));
        assert!(html.contains("id=\"settings-toggle\""));
        assert!(html.contains("id=\"error-banner\""));
        assert!(html.contains("id=\"seek\""));
        assert!(!html.contains("id=\"status\""));
        assert!(html.contains("codex-voice.web.config.v1"));
        assert!(html.contains("codex-voice.web.settings.v1"));
        assert!(html.contains("emotionPreprocessing"));
        assert!(html.contains("summarization"));
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
        assert!(text_idx < scrubber_idx);
        assert!(text_idx < clear_idx);
        assert!(clear_idx < scrubber_idx);
        assert!(scrubber_idx < buttons_idx);
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
        assert!(html.contains("lifecycleInterruptedGeneration"));
        assert!(html.contains("function shouldKeepPendingGeneration"));
        assert!(html.contains("if (error?.status) return false;"));
        assert!(html.contains("showError(error.message || 'TTS failed.')"));
        assert!(html.contains("settings.provider !== 'auto'"));
        assert!(html.contains("function providerModelOptions"));
        assert!(html.contains("function selectedProviderModel"));
        assert!(html.contains("return selectedProviderModel('google', google.model);"));
        assert!(html.contains("model_id: resolveElevenLabsModel(elevenlabs)"));
        assert!(html.contains("prep.mode === 'shorten'"));
        assert!(html.contains("function prepareDecision"));
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
        assert!(html.contains("settingsToggle.addEventListener('click'"));
        assert!(html.contains("paste.addEventListener('click'"));
        assert!(html.contains("navigator.clipboard.readText()"));
        assert!(html.contains("setGenerateProgress(0.64, 'Synthesizing')"));
        assert!(html.contains("setGenerateProgress(0.9, 'Saving')"));
        assert!(html.contains("setGenerateProgress(1, 'Done')"));
        assert!(html.contains("performanceTagsMaxOutputTokens = 384"));
        assert!(html.contains("performanceTagsAbsoluteMaxOutputTokens = 4096"));
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
        assert!(html.contains("function splitTtsText"));
        assert!(html.contains("function concatUint8Arrays"));
        assert!(html.contains("ttsChunkBoundarySilenceMs = 180"));
        assert!(html.contains("function concatPcmChunksWithBoundarySilence"));
        assert!(html.contains("function concatWavChunksWithBoundarySilence"));
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
        assert!(
            html.contains("synthesizeElevenLabsSingle(config, chunk, persona, outputFormat, true)")
        );
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
        assert!(html.contains("function shouldPreferServerGeneration"));
        assert!(html.contains("settingsMatchServerDefaults()"));
        assert!(html.contains("function serverGenerationUnavailable"));
        assert!(html.contains("Configured emotion prep is server-only."));
        assert!(html.contains("'/web/speech-jobs'"));
        assert!(html.contains(r#"<link rel="manifest" href="/web/manifest.webmanifest">"#));
        assert!(html.contains(r##"<meta name="theme-color" content="#101214">"##));
        assert!(html.contains(r#"<link rel="apple-touch-icon" href="/web/apple-touch-icon.png">"#));
        assert!(html.contains("navigator.serviceWorker.register('/web-sw.js'"));
        assert!(html.contains("updateViaCache: 'none'"));
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
        assert_eq!(config["providers"]["elevenlabs"]["apiKey"], "eleven-key");
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
        assert_eq!(manifest["theme_color"], "#101214");
        assert_eq!(manifest["background_color"], "#101214");
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
        assert!(script.contains("networkFirst(request, url.pathname)"));
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
