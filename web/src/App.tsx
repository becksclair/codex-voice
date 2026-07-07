import { useEffect, useRef } from "react";
import {
  applyTheme,
  clearPendingGeneration,
  deleteLastGeneratedAudio,
  fetchConfig,
  firstPersonaForProvider,
  GenerationController,
  getLastGeneratedAudio,
  loadCachedConfig,
  loadPendingGeneration,
  loadSettings,
  loadText,
  personaSupportsProvider,
  saveCachedConfig,
  saveSettings as persistSettings,
  saveText,
  shouldApplyGeneratedText,
  type BrowserTtsConfig,
  type GenerationMeta,
  type StreamingPlayback,
  type ThemePreference,
  type WebSettings,
} from "./lib/index.ts";
import { reloadForWorkerUpdateWhenIdle, setBusyPredicate } from "./pwa.ts";
import { WaveformController } from "./waveform-controller.ts";

/** Format seconds as `m:ss`. Ports `formatTime` (app.html line ~1037). */
function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "0:00";
  const whole = Math.floor(seconds);
  const minutes = Math.floor(whole / 60);
  return `${minutes}:${String(whole % 60).padStart(2, "0")}`;
}

/** Download-file extension from a blob's mime type. Ports `audioDownloadExtension` (app.html line ~1381). */
function audioDownloadExtension(blob: Blob | null): string {
  const type = String(blob?.type || "").toLowerCase();
  if (type.includes("mpeg") || type.includes("mp3")) return "mp3";
  if (type.includes("opus")) return "opus";
  if (type.includes("ogg")) return "ogg";
  if (type.includes("wav") || type.includes("pcm")) return "wav";
  return "wav";
}

const PLAY_PATH = '<path d="M8 5v14l11-7Z"/>';
const PAUSE_PATH = '<path d="M8 5v14"/><path d="M16 5v14"/>';

/**
 * The Codex Voice web shell.
 *
 * The JSX reproduces the exact DOM of the legacy PWA (app.html body, lines
 * ~662-743). All behavior is wired imperatively in a mount effect that ports the
 * legacy `<script>` (line ~744 onward), delegating the generation/streaming/prep
 * pipeline to {@link GenerationController} and {@link StreamingPlayback}.
 */
export function App() {
  const textRef = useRef<HTMLTextAreaElement>(null);
  const generateRef = useRef<HTMLButtonElement>(null);
  const generateLabelRef = useRef<HTMLSpanElement>(null);
  const playRef = useRef<HTMLButtonElement>(null);
  const playIconRef = useRef<SVGSVGElement>(null);
  const downloadRef = useRef<HTMLButtonElement>(null);
  const clearRef = useRef<HTMLButtonElement>(null);
  const pasteRef = useRef<HTMLButtonElement>(null);
  const settingsToggleRef = useRef<HTMLButtonElement>(null);
  const settingsPanelRef = useRef<HTMLDivElement>(null);
  const providerRef = useRef<HTMLSelectElement>(null);
  const voiceRef = useRef<HTMLSelectElement>(null);
  const modelRef = useRef<HTMLSelectElement>(null);
  const themeRef = useRef<HTMLSelectElement>(null);
  const emotionRef = useRef<HTMLInputElement>(null);
  const summarizeRef = useRef<HTMLInputElement>(null);
  const generateOnPasteRef = useRef<HTMLInputElement>(null);
  const seekSliderRef = useRef<HTMLDivElement>(null);
  const waveformCanvasRef = useRef<HTMLCanvasElement>(null);
  const elapsedRef = useRef<HTMLTimeElement>(null);
  const durationRef = useRef<HTMLTimeElement>(null);
  const errorBannerRef = useRef<HTMLDivElement>(null);
  const countRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const text = textRef.current;
    const generate = generateRef.current;
    const generateLabel = generateLabelRef.current;
    const play = playRef.current;
    const playIcon = playIconRef.current;
    const download = downloadRef.current;
    const clear = clearRef.current;
    const paste = pasteRef.current;
    const settingsToggle = settingsToggleRef.current;
    const settingsPanel = settingsPanelRef.current;
    const providerSelect = providerRef.current;
    const voiceSelect = voiceRef.current;
    const modelSelect = modelRef.current;
    const themeSelect = themeRef.current;
    const emotion = emotionRef.current;
    const summarize = summarizeRef.current;
    const generateOnPaste = generateOnPasteRef.current;
    const seekSlider = seekSliderRef.current;
    const waveformCanvas = waveformCanvasRef.current;
    const elapsed = elapsedRef.current;
    const duration = durationRef.current;
    const errorBanner = errorBannerRef.current;
    const count = countRef.current;
    if (
      !text ||
      !generate ||
      !generateLabel ||
      !play ||
      !playIcon ||
      !download ||
      !clear ||
      !paste ||
      !settingsToggle ||
      !settingsPanel ||
      !providerSelect ||
      !voiceSelect ||
      !modelSelect ||
      !themeSelect ||
      !emotion ||
      !summarize ||
      !generateOnPaste ||
      !seekSlider ||
      !waveformCanvas ||
      !elapsed ||
      !duration ||
      !errorBanner ||
      !count
    ) {
      return;
    }

    const audio = new Audio();
    let objectUrl: string | null = null;
    let currentAudioBlob: Blob | null = null;
    let seeking = false;
    let generationActive = false;
    let streamPlayback: StreamingPlayback | null = null;
    let directConfig: BrowserTtsConfig | null = loadCachedConfig();
    let settings: WebSettings = loadSettings();
    const themeMedia = window.matchMedia?.("(prefers-color-scheme: light)") || null;
    const waveform = new WaveformController(waveformCanvas, seekSlider);
    const cleanups: Array<() => void> = [];

    setBusyPredicate(() => generationActive || Boolean(streamPlayback));

    // --- theme ---

    function applyThemeSetting(preference: ThemePreference = settings.theme || "auto"): void {
      applyTheme(document, preference, Boolean(themeMedia?.matches));
    }

    // --- error banner ---

    function showError(message: string): void {
      errorBanner!.textContent = message || "Something went wrong.";
      errorBanner!.classList.add("visible");
    }

    function clearError(): void {
      errorBanner!.textContent = "";
      errorBanner!.classList.remove("visible");
    }

    // --- generate button state ---

    function setGenerateProgress(value: number, label = "Generate"): void {
      const progress = Math.max(0, Math.min(1, Number(value) || 0));
      generate!.style.setProperty("--generate-progress", String(progress));
      generateLabel!.textContent = label;
    }

    function setGenerating(active: boolean, label = "Generate", progress = 0): void {
      generate!.classList.toggle("generating", active);
      setGenerateProgress(progress, label);
    }

    function playSvg(paused: boolean): void {
      playIcon!.innerHTML = paused ? PLAY_PATH : PAUSE_PATH;
      play!.setAttribute("aria-label", paused ? "Play" : "Pause");
    }

    // --- settings form ---

    function option(value: string, label: string): HTMLOptionElement {
      const node = document.createElement("option");
      node.value = value;
      node.textContent = label;
      return node;
    }

    function personaEntries(config: BrowserTtsConfig | null) {
      return Object.entries(config?.personas || {});
    }

    function providerCanGenerate(config: BrowserTtsConfig | null, provider: string): boolean {
      if (provider === "google") return Boolean(config?.providers?.google);
      if (provider === "elevenlabs") {
        return Boolean(
          config?.providers?.elevenlabs && firstPersonaForProvider(config, "elevenlabs"),
        );
      }
      return false;
    }

    function providerModelOptions(
      config: BrowserTtsConfig | null,
      provider: string,
    ): HTMLOptionElement[] {
      const seen = new Set<string>();
      const options: string[] = [];
      const pushModel = (value: string | undefined | null): void => {
        if (!value || seen.has(value)) return;
        seen.add(value);
        options.push(value);
      };
      if (provider === "google") {
        const google = config?.providers?.google;
        pushModel(google?.model);
        for (const model of google?.fallbackModels || []) pushModel(model);
      } else if (provider === "elevenlabs") {
        pushModel(config?.providers?.elevenlabs?.modelId);
      }
      return options.map((model) => option(`${provider}:${model}`, model));
    }

    function applySettingsToForm(): void {
      providerSelect!.value = settings.provider;
      voiceSelect!.value = settings.voice;
      modelSelect!.value = settings.model;
      themeSelect!.value = settings.theme || "auto";
      emotion!.checked = settings.emotionPreprocessing;
      summarize!.checked = settings.summarization;
      generateOnPaste!.checked = settings.generateOnPaste !== false;
    }

    function saveSettings(): void {
      settings = {
        provider: providerSelect!.value || "auto",
        voice: voiceSelect!.value || "default",
        model: modelSelect!.value || "default",
        theme: (themeSelect!.value || "auto") as ThemePreference,
        emotionPreprocessing: emotion!.checked,
        summarization: summarize!.checked,
        generateOnPaste: generateOnPaste!.checked,
      };
      persistSettings(settings);
      controller.update({ settings });
      applyThemeSetting(settings.theme);
    }

    function populateSettings(): void {
      const priorProvider = providerSelect!.value || settings.provider;
      const priorVoice = voiceSelect!.value || settings.voice;
      const priorModel = modelSelect!.value || settings.model;
      providerSelect!.replaceChildren(option("auto", "Auto"));
      if (providerCanGenerate(directConfig, "google"))
        providerSelect!.append(option("google", "Google"));
      if (providerCanGenerate(directConfig, "elevenlabs")) {
        providerSelect!.append(option("elevenlabs", "ElevenLabs"));
      }
      providerSelect!.value = [...providerSelect!.options].some(
        (item) => item.value === priorProvider,
      )
        ? priorProvider
        : "auto";

      voiceSelect!.replaceChildren(option("default", "Default"));
      if (providerSelect!.value !== "elevenlabs") {
        voiceSelect!.append(option("provider-default", "Provider default"));
      }
      for (const [name, persona] of personaEntries(directConfig)) {
        if (!personaSupportsProvider(persona, providerSelect!.value)) continue;
        voiceSelect!.append(option(`persona:${name}`, persona.label || name));
      }
      voiceSelect!.value = [...voiceSelect!.options].some((item) => item.value === priorVoice)
        ? priorVoice
        : "default";

      modelSelect!.replaceChildren(option("default", "Default"));
      if (providerSelect!.value !== "auto") {
        for (const modelOption of providerModelOptions(directConfig, providerSelect!.value)) {
          modelSelect!.append(modelOption);
        }
      }
      modelSelect!.value = [...modelSelect!.options].some((item) => item.value === priorModel)
        ? priorModel
        : "default";
      saveSettings();
    }

    async function refreshConfig(): Promise<void> {
      const config = await fetchConfig();
      if (!config) return;
      directConfig = config;
      saveCachedConfig(config);
      controller.update({ config });
      populateSettings();
    }

    // --- character count ---

    function updateCount(): void {
      const chars = Array.from(text!.value).length;
      count!.textContent = `${chars} ${chars === 1 ? "char" : "chars"}`;
      clear!.hidden = chars === 0;
    }

    // --- visual viewport ---

    function updateVisualViewportLayout(): void {
      const viewport = window.visualViewport;
      const height =
        viewport?.height || window.innerHeight || document.documentElement.clientHeight;
      const offsetTop = viewport?.offsetTop || 0;
      const keyboardInset = Math.max(0, (window.innerHeight || height) - height - offsetTop);
      document.documentElement.style.setProperty("--visual-viewport-height", `${height}px`);
      document.documentElement.style.setProperty("--visual-viewport-offset-top", `${offsetTop}px`);
      document.documentElement.classList.toggle("keyboard-open", keyboardInset > 80);
      waveform.scheduleDraw();
    }

    // --- audio playback ---

    function updatePosition(): void {
      const total = audio.duration || 0;
      if (!seeking && total > 0) waveform.setCurrent(audio.currentTime);
      elapsed!.textContent = formatTime(audio.currentTime);
      duration!.textContent = formatTime(total);
    }

    function stopStreamPlayback(): void {
      if (!streamPlayback) return;
      const playback = streamPlayback;
      streamPlayback = null;
      playback.stop();
      reloadForWorkerUpdateWhenIdle();
    }

    function resetAudio(): void {
      stopStreamPlayback();
      audio.pause();
      audio.removeAttribute("src");
      audio.load();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
      objectUrl = null;
      currentAudioBlob = null;
      play!.disabled = true;
      download!.disabled = true;
      playSvg(true);
      elapsed!.textContent = "0:00";
      duration!.textContent = "0:00";
      waveform.reset();
    }

    function loadAudioBlob(blob: Blob): void {
      resetAudio();
      currentAudioBlob = blob;
      objectUrl = URL.createObjectURL(blob);
      audio.src = objectUrl;
      audio.load();
      play!.disabled = false;
      download!.disabled = false;
      void waveform.decodeBlob(blob, audio.currentTime || 0);
    }

    function downloadCurrentAudio(): void {
      if (!currentAudioBlob) return;
      const url = URL.createObjectURL(currentAudioBlob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `codex-voice-${new Date()
        .toISOString()
        .replace(/[:.]/g, "-")}.${audioDownloadExtension(currentAudioBlob)}`;
      document.body.append(link);
      link.click();
      link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    }

    // --- seek ---

    function seekToWaveformTime(seconds: number): void {
      const target = Math.max(0, Number(seconds) || 0);
      if (streamPlayback) {
        streamPlayback
          .seekTo(target)
          .catch((error: Error) => showError(error.message || "Seek failed."));
        return;
      }
      const total = audio.duration || 0;
      if (total > 0) {
        audio.currentTime = Math.min(Math.max(target, 0), total);
        updatePosition();
      }
    }

    function handleWaveformPointer(event: PointerEvent): void {
      if (seekSlider!.getAttribute("aria-disabled") === "true") return;
      seeking = true;
      seekSlider!.classList.add("scrubbing");
      seekToWaveformTime(waveform.seekTimeFromClientX(event.clientX));
      event.preventDefault();
    }

    let keyboardScrubTimer: ReturnType<typeof setTimeout> | null = null;
    function showKeyboardScrubFeedback(): void {
      seekSlider!.classList.add("scrubbing");
      if (keyboardScrubTimer) clearTimeout(keyboardScrubTimer);
      keyboardScrubTimer = setTimeout(() => {
        keyboardScrubTimer = null;
        if (!seeking) seekSlider!.classList.remove("scrubbing");
      }, 420);
    }

    // --- restore + resume ---

    function currentDraftText(): string {
      return text!.value || loadText() || "";
    }

    async function restoreLastGeneratedAudio(): Promise<void> {
      try {
        const record = await getLastGeneratedAudio();
        if (!record?.blob) return;
        if (
          typeof record.text === "string" &&
          shouldApplyGeneratedText(currentDraftText(), record.text, record.text)
        ) {
          text!.value = record.text;
          saveText(text!.value);
          updateCount();
        }
        loadAudioBlob(record.blob);
        clearError();
      } catch {
        // Ignored, matching app.html behavior.
      }
    }

    async function initializeStoredState(): Promise<void> {
      await restoreLastGeneratedAudio();
      await controller.resumePending();
    }

    // --- generation ---

    const controller = new GenerationController({
      config: directConfig,
      settings,
      getDraftText: currentDraftText,
      callbacks: {
        onStatus: (label, fraction) => setGenerateProgress(fraction, label),
        onGeneratingChange: (active) => {
          if (active) {
            generationActive = true;
            generate.disabled = true;
            clear.disabled = false;
            play.disabled = true;
            setGenerating(true, "Starting", 0.08);
          } else {
            generationActive = false;
            generate.disabled = false;
            clear.disabled = false;
            setTimeout(() => {
              if (!generationActive) setGenerating(false, "Generate", 0);
            }, 350);
            reloadForWorkerUpdateWhenIdle();
          }
        },
        onError: (message) => {
          play.disabled = Boolean(!audio.src);
          if (!audio.src && !streamPlayback) waveform.reset();
          showError(message);
        },
        onClearError: clearError,
        onTextReplace: (value) => {
          text.value = value;
          updateCount();
        },
        onAudioReady: (blob, meta: GenerationMeta) => {
          if (meta.streamed && meta.playback) {
            currentAudioBlob = blob;
            download.disabled = false;
            play.disabled = false;
            streamPlayback = meta.playback.stopped ? null : meta.playback;
          } else {
            loadAudioBlob(blob);
          }
        },
        playbackCallbacks: {
          onPlayingChange: (playing) => playSvg(!playing),
          onProgress: (current, estimated, finished) => {
            elapsed.textContent = formatTime(current);
            duration.textContent = finished ? formatTime(estimated) : "Live";
            waveform.setCurrent(current);
          },
          onPeaks: (peaks, durationDelta) => waveform.appendStreamingPeaks(peaks, durationDelta),
          onFinished: () => waveform.markStreamFinished(),
          onReplayReady: (blob) => {
            streamPlayback = null;
            loadAudioBlob(blob);
            reloadForWorkerUpdateWhenIdle();
          },
        },
        onStreamState: (state) => {
          if (state === "buffering") {
            resetAudio();
            waveform.resetStreaming();
            duration.textContent = "Live";
            play.disabled = false;
          }
        },
      },
    });

    async function generateCurrentText(): Promise<boolean> {
      const input = text!.value.trim();
      if (!input) {
        showError("Enter some text first.");
        return false;
      }
      if (controller.isActive) {
        controller.cancel();
        generationActive = false;
        generate!.disabled = false;
        clear!.disabled = false;
      }
      await controller.generate(input);
      return true;
    }

    function generateAfterPaste(event: ClipboardEvent): void {
      if (settings.generateOnPaste === false) return;
      const pastedText = event?.clipboardData?.getData("text") || "";
      if (!pastedText.trim()) return;
      const valueBeforePaste = text!.value;
      setTimeout(() => {
        if (text!.value === valueBeforePaste) return;
        const input = text!.value.trim();
        if (!input) return;
        generateCurrentText().catch((error: Error) => showError(error.message || "TTS failed."));
      }, 0);
    }

    // --- listener registration helper ---

    function on<T extends EventTarget>(
      target: T,
      type: string,
      handler: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions,
    ): void {
      target.addEventListener(type, handler, options);
      cleanups.push(() => target.removeEventListener(type, handler, options));
    }

    // --- initialization (ports app.html lines ~801-823) ---

    applyThemeSetting(settings.theme);
    text.value = loadText();
    updateVisualViewportLayout();
    updateCount();
    waveform.reset();
    applySettingsToForm();
    populateSettings();
    void refreshConfig();
    void initializeStoredState();

    // --- event wiring ---

    if (window.visualViewport) {
      on(window.visualViewport, "resize", updateVisualViewportLayout);
      on(window.visualViewport, "scroll", updateVisualViewportLayout);
    }
    on(window, "resize", updateVisualViewportLayout);
    on(window, "orientationchange", updateVisualViewportLayout);
    on(text, "focus", updateVisualViewportLayout);
    on(text, "blur", () => setTimeout(updateVisualViewportLayout, 120));

    on(text, "input", () => {
      saveText(text.value);
      updateCount();
    });
    on(text, "paste", generateAfterPaste as EventListener);

    on(window, "pagehide", () => {
      if (generationActive) controller.markLifecycleInterrupted();
      saveText(text.value);
    });
    on(window, "pageshow", (event) => {
      if ((event as PageTransitionEvent).persisted && !audio.src) void restoreLastGeneratedAudio();
      if (generationActive) setGenerating(true, "Generating", 0.45);
      if (!generationActive && loadPendingGeneration()) void controller.resumePending();
    });
    on(document, "visibilitychange", () => {
      if (document.visibilityState !== "visible" && generationActive) {
        controller.markLifecycleInterrupted();
        return;
      }
      if (document.visibilityState === "visible" && generationActive) {
        setGenerating(true, "Generating", 0.45);
      }
      if (document.visibilityState === "visible" && !generationActive && loadPendingGeneration()) {
        void controller.resumePending();
      }
    });

    on(providerSelect, "change", populateSettings);
    on(voiceSelect, "change", saveSettings);
    on(modelSelect, "change", saveSettings);
    on(themeSelect, "change", saveSettings);
    on(emotion, "change", saveSettings);
    on(summarize, "change", saveSettings);
    on(generateOnPaste, "change", saveSettings);

    function handleThemeMediaChange(): void {
      if ((settings.theme || "auto") === "auto") applyThemeSetting("auto");
    }
    if (themeMedia?.addEventListener) {
      themeMedia.addEventListener("change", handleThemeMediaChange);
      cleanups.push(() => themeMedia.removeEventListener("change", handleThemeMediaChange));
    }

    on(settingsToggle, "click", () => {
      const open = settingsPanel.hasAttribute("hidden");
      settingsPanel.toggleAttribute("hidden", !open);
      settingsToggle.setAttribute("aria-expanded", String(open));
    });

    on(paste, "click", async () => {
      try {
        const value = await navigator.clipboard.readText();
        text.value = "";
        if (!value) {
          localStorage.removeItem("codex-voice.web.text");
          updateCount();
          return;
        }
        text.value = value;
        saveText(text.value);
        updateCount();
        clearError();
        if (settings.generateOnPaste !== false) await generateCurrentText();
      } catch (error) {
        showError((error as Error).message || "Clipboard paste failed.");
      }
    });

    on(clear, "click", async () => {
      if (controller.isActive) {
        controller.cancel();
        generationActive = false;
        generate.disabled = false;
        clear.disabled = false;
        setGenerating(false, "Generate", 0);
      }
      text.value = "";
      localStorage.removeItem("codex-voice.web.text");
      clearPendingGeneration();
      updateCount();
      resetAudio();
      await deleteLastGeneratedAudio();
      clearError();
      text.focus();
    });

    on(generate, "click", async () => {
      await generateCurrentText();
    });

    on(play, "click", async () => {
      if (streamPlayback) {
        try {
          await streamPlayback.toggle();
        } catch (error) {
          showError((error as Error).message || "Streaming playback failed.");
        }
        return;
      }
      if (!audio.src) return;
      if (audio.paused) {
        try {
          await audio.play();
        } catch (error) {
          showError((error as Error).message || "Playback failed.");
        }
      } else {
        audio.pause();
      }
    });

    on(download, "click", downloadCurrentAudio);

    on(seekSlider, "pointerdown", (event) => {
      if (seekSlider.getAttribute("aria-disabled") === "true") return;
      seekSlider.setPointerCapture?.((event as PointerEvent).pointerId);
      handleWaveformPointer(event as PointerEvent);
    });
    on(seekSlider, "pointermove", (event) => {
      if (!seeking) return;
      handleWaveformPointer(event as PointerEvent);
    });
    on(seekSlider, "pointerup", (event) => {
      if (!seeking) return;
      handleWaveformPointer(event as PointerEvent);
      seeking = false;
      seekSlider.classList.remove("scrubbing");
    });
    on(seekSlider, "pointercancel", () => {
      seeking = false;
      seekSlider.classList.remove("scrubbing");
    });
    on(seekSlider, "keydown", (event) => {
      const keyEvent = event as KeyboardEvent;
      if (seekSlider.getAttribute("aria-disabled") === "true") return;
      const max = waveform.mode === "complete" ? waveform.duration : waveform.bufferedDuration || 0;
      const step = keyEvent.shiftKey ? 10 : 5;
      let target: number | null = null;
      if (keyEvent.key === "ArrowLeft" || keyEvent.key === "ArrowDown")
        target = waveform.currentTime - step;
      if (keyEvent.key === "ArrowRight" || keyEvent.key === "ArrowUp")
        target = waveform.currentTime + step;
      if (keyEvent.key === "Home") target = 0;
      if (keyEvent.key === "End") target = max;
      if (target !== null) {
        keyEvent.preventDefault();
        showKeyboardScrubFeedback();
        seekToWaveformTime(Math.min(Math.max(target, 0), max));
      }
    });

    on(audio, "loadedmetadata", updatePosition);
    on(audio, "timeupdate", updatePosition);
    on(audio, "play", () => {
      playSvg(false);
      clearError();
    });
    on(audio, "pause", () => playSvg(true));
    on(audio, "ended", () => {
      playSvg(true);
      updatePosition();
    });

    return () => {
      for (const cleanup of cleanups) cleanup();
      if (keyboardScrubTimer) clearTimeout(keyboardScrubTimer);
      controller.cancel();
      audio.pause();
      audio.removeAttribute("src");
      if (objectUrl) URL.revokeObjectURL(objectUrl);
      setBusyPredicate(() => false);
    };
  }, []);

  return (
    <main>
      <header>
        <img className="app-icon" src="/web/icon-192.png" alt="Codex Voice" />
        <div className="header-actions">
          <span id="count" ref={countRef}>
            0 chars
          </span>
        </div>
      </header>
      <div id="error-banner" className="error-banner" role="alert" ref={errorBannerRef}></div>
      <div className="text-shell">
        <textarea
          id="text"
          ref={textRef}
          autoComplete="off"
          autoCapitalize="sentences"
          spellCheck={true}
          placeholder="Type something to hear it spoken..."
        ></textarea>
        <button
          id="paste"
          type="button"
          className="icon-button"
          aria-label="Paste clipboard contents"
          ref={pasteRef}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M8 4h8" />
            <path d="M9 2h6a1 1 0 0 1 1 1v2H8V3a1 1 0 0 1 1-1Z" />
            <path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" />
          </svg>
        </button>
        <button
          id="clear"
          type="button"
          className="secondary icon-button"
          aria-label="Clear text"
          hidden
          ref={clearRef}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 6h18" />
            <path d="M8 6V4h8v2" />
            <path d="M19 6l-1 14H6L5 6" />
            <path d="M10 11v5" />
            <path d="M14 11v5" />
          </svg>
        </button>
      </div>
      <section className="controls">
        <div className="scrubber">
          <time id="elapsed" ref={elapsedRef}>
            0:00
          </time>
          <div
            id="waveform-slider"
            className="waveform-slider"
            role="slider"
            tabIndex={0}
            aria-label="Audio position"
            aria-valuemin={0}
            aria-valuemax={0}
            aria-valuenow={0}
            aria-valuetext="0:00 of 0:00"
            aria-disabled="true"
            ref={seekSliderRef}
          >
            <canvas id="waveform" aria-hidden="true" ref={waveformCanvasRef}></canvas>
            <span className="waveform-marker" aria-hidden="true"></span>
            <span className="waveform-thumb" aria-hidden="true"></span>
          </div>
          <time id="duration" ref={durationRef}>
            0:00
          </time>
        </div>
        <div className="buttons">
          <button id="generate" type="button" ref={generateRef}>
            <span className="generate-main">
              <span className="spinner" aria-hidden="true"></span>
              <span id="generate-label" ref={generateLabelRef}>
                Generate
              </span>
            </span>
            <span className="generate-progress" aria-hidden="true">
              <span></span>
            </span>
          </button>
          <button
            id="play"
            type="button"
            className="secondary icon-button"
            disabled
            aria-label="Play"
            ref={playRef}
          >
            <svg id="play-icon" viewBox="0 0 24 24" aria-hidden="true" ref={playIconRef}>
              <path d="M8 5v14l11-7Z" />
            </svg>
          </button>
          <button
            id="download"
            type="button"
            className="secondary icon-button"
            disabled
            aria-label="Download audio"
            ref={downloadRef}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 3v12" />
              <path d="m7 10 5 5 5-5" />
              <path d="M5 21h14" />
            </svg>
          </button>
          <button
            id="settings-toggle"
            type="button"
            className="secondary icon-button"
            aria-label="Toggle settings"
            aria-expanded="false"
            ref={settingsToggleRef}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z" />
              <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.04 1.56V21a2 2 0 1 1-4 0v-.08a1.7 1.7 0 0 0-1.04-1.56 1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.56-1.04H3a2 2 0 1 1 0-4h.08A1.7 1.7 0 0 0 4.64 8.9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-1.56V3a2 2 0 1 1 4 0v.08a1.7 1.7 0 0 0 1.04 1.56 1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.4 9c.1.38.4.7.76.86.25.1.52.15.8.14H21a2 2 0 1 1 0 4h-.08A1.7 1.7 0 0 0 19.4 15Z" />
            </svg>
          </button>
        </div>
        <div className="settings" id="settings-panel" hidden ref={settingsPanelRef}>
          <div className="settings-grid">
            <label className="field">
              Provider
              <select id="provider" ref={providerRef}></select>
            </label>
            <label className="field">
              Voice
              <select id="voice" ref={voiceRef}></select>
            </label>
            <label className="field">
              Model
              <select id="model" ref={modelRef}></select>
            </label>
            <label className="field">
              Theme
              <select id="theme" ref={themeRef}>
                <option value="auto">Auto</option>
                <option value="dark">Dark</option>
                <option value="light">Light</option>
              </select>
            </label>
            <div className="toggles">
              <label className="toggle">
                <input id="emotion" type="checkbox" ref={emotionRef} />
                Emotion
              </label>
              <label className="toggle">
                <input id="summarize" type="checkbox" ref={summarizeRef} />
                Summarize
              </label>
              <label className="toggle">
                <input id="generate-on-paste" type="checkbox" ref={generateOnPasteRef} />
                Generate on paste
              </label>
            </div>
          </div>
        </div>
      </section>
    </main>
  );
}
