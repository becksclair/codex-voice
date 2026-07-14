import { useEffect, useRef, useState, type ClipboardEvent } from "react";
import {
  clearPendingGeneration,
  consumeDesktopIntent,
  deleteLastGeneratedAudio,
  settingsView,
  speakIntentId,
  TEXT_STORAGE_KEY,
} from "./lib/index.ts";
import { GenerateBar } from "./components/GenerateBar.tsx";
import { SettingsPanel } from "./components/SettingsPanel.tsx";
import { TextEditor } from "./components/TextEditor.tsx";
import { WaveformPlayer } from "./components/WaveformPlayer.tsx";
import { useGeneration } from "./hooks/useGeneration.ts";
import { useLatest } from "./hooks/useLatest.ts";
import { usePersistedText } from "./hooks/usePersistedText.ts";
import { usePlayback } from "./hooks/usePlayback.ts";
import { useSeekGestures } from "./hooks/useSeekGestures.ts";
import { useServerConfig } from "./hooks/useServerConfig.ts";
import { useSettings } from "./hooks/useSettings.ts";
import { useVisualViewport } from "./hooks/useVisualViewport.ts";
import { useWaveform } from "./hooks/useWaveform.ts";

/**
 * The Codex Voice web shell.
 *
 * Composes the settings/config/text/playback/generation hooks and the shell
 * components. Each subsystem (audio element, canvas waveform, generation
 * controller, service worker, visual viewport, storage) is owned by its hook;
 * this component wires them together and holds the small amount of cross-cutting
 * UI state (the error banner and the settings drawer).
 */
export function App() {
  return settingsView(location.search) ? <SettingsWindowApp /> : <MainWindowApp />;
}

function SettingsWindowApp() {
  const config = useServerConfig();
  const settings = useSettings(config);
  return (
    <main className="mx-auto h-dvh max-w-[520px] overflow-y-auto p-4">
      <SettingsPanel open settings={settings} />
    </main>
  );
}

function MainWindowApp() {
  const textRef = useRef<HTMLTextAreaElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const sliderRef = useRef<HTMLDivElement>(null);

  const [error, setError] = useState("");
  const errorApi = useRef({
    show: (message: string) => setError(message || "Something went wrong."),
    clear: () => setError(""),
  }).current;

  const [settingsOpen, setSettingsOpen] = useState(false);

  const config = useServerConfig();
  const settings = useSettings(config);
  const [text, setText] = usePersistedText();

  const waveformRef = useWaveform(canvasRef, sliderRef);
  const playback = usePlayback(waveformRef, errorApi.show, errorApi.clear);
  const generation = useGeneration({
    config,
    settings: settings.settings,
    textRef,
    setText,
    playback: playback.api,
    waveformRef,
    showError: errorApi.show,
    clearError: errorApi.clear,
  });
  const seek = useSeekGestures(sliderRef, waveformRef, playback);
  useVisualViewport(textRef, waveformRef);

  // Desktop-app `#intent=<id>` intake: clear the fragment, consume selected
  // text once from the local service, seed the normal persisted-text path, and
  // auto-generate. Refs keep hashchange intake current without remounting.
  const setTextRef = useLatest(setText);
  const generateRef = useLatest(generation.generate);
  const showErrorRef = useLatest(errorApi.show);
  const speakIntakeSequence = useRef(0);

  useEffect(() => {
    const handleSpeakIntake = async (): Promise<void> => {
      const intentId = speakIntentId(location.hash);
      if (intentId === null) return;
      const sequence = ++speakIntakeSequence.current;
      history.replaceState(null, "", location.pathname + location.search);
      try {
        const text = await consumeDesktopIntent(intentId);
        if (sequence !== speakIntakeSequence.current) return;
        setTextRef.current(text);
        await generateRef.current(text);
      } catch (error) {
        if (sequence !== speakIntakeSequence.current) return;
        showErrorRef.current((error as Error).message || "Selected text handoff failed.");
      }
    };
    const onHashChange = (): void => void handleSpeakIntake();
    void handleSpeakIntake();
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
    // Refs are stable across renders, so this still mounts exactly once.
  }, [setTextRef, generateRef, showErrorRef]);

  const charCount = Array.from(text).length;

  const handleNativePaste = (event: ClipboardEvent<HTMLTextAreaElement>): void => {
    if (settings.settings.generateOnPaste === false) return;
    const pasted = event.clipboardData?.getData("text") || "";
    if (!pasted.trim()) return;
    const valueBeforePaste = textRef.current?.value ?? "";
    setTimeout(() => {
      const current = textRef.current?.value ?? "";
      if (current === valueBeforePaste || !current.trim()) return;
      generation.generate(current).catch((e: Error) => errorApi.show(e.message || "TTS failed."));
    }, 0);
  };

  const handlePasteClick = async (): Promise<void> => {
    try {
      const value = await navigator.clipboard.readText();
      if (!value) return;
      setText(value);
      errorApi.clear();
      if (settings.settings.generateOnPaste !== false) await generation.generate(value);
    } catch (error) {
      errorApi.show((error as Error).message || "Clipboard paste failed.");
    }
  };

  const handleClear = async (): Promise<void> => {
    generation.cancelActive();
    setText("", { persist: false });
    localStorage.removeItem(TEXT_STORAGE_KEY);
    clearPendingGeneration();
    playback.api.resetAudio();
    await deleteLastGeneratedAudio();
    errorApi.clear();
    textRef.current?.focus();
  };

  return (
    <main
      className={`mx-auto flex h-[var(--visual-viewport-height,100dvh)] min-h-0 max-w-[760px] translate-y-[var(--visual-viewport-offset-top,0px)] flex-col gap-3 pt-[max(12px,env(safe-area-inset-top))] pr-[18px] pb-[max(18px,env(safe-area-inset-bottom))] pl-[18px] max-[420px]:px-3 ${settingsOpen ? "overflow-y-auto overscroll-contain" : "overflow-hidden"}`}
    >
      <header className="flex items-center justify-between gap-2.5">
        <img
          className="block h-3.5 w-3.5 rounded-[4px] shadow-[var(--icon-shadow)]"
          src="/web/icon-192.png"
          alt="Codex Voice"
        />
        <div className="flex items-center gap-2">
          <span
            id="count"
            className="whitespace-nowrap text-[0.76rem] font-semibold text-[var(--count-color)] [text-shadow:var(--count-shadow)]"
          >
            {charCount} {charCount === 1 ? "char" : "chars"}
          </span>
        </div>
      </header>
      <div
        id="error-banner"
        className={`${error ? "flex" : "hidden"} min-h-11 items-center rounded-2xl border border-[var(--error-border)] bg-[var(--error-bg)] px-3 py-2.5 text-[0.95rem] text-[var(--error-text)]`}
        role="alert"
      >
        {error}
      </div>
      <TextEditor
        textRef={textRef}
        value={text}
        onChange={(value) => setText(value)}
        onPaste={handleNativePaste}
        onPasteClick={() => void handlePasteClick()}
        onClearClick={() => void handleClear()}
        clearVisible={charCount > 0}
      />
      <section className="grid flex-none gap-3.5">
        <WaveformPlayer
          elapsed={playback.elapsed}
          duration={playback.duration}
          sliderRef={sliderRef}
          canvasRef={canvasRef}
          seek={seek}
        />
        <GenerateBar
          generating={generation.generating}
          progress={generation.progress}
          label={generation.label}
          generateDisabled={generation.busy}
          onGenerate={() => void generation.generate()}
          paused={playback.paused}
          playDisabled={playback.playDisabled}
          onTogglePlay={() => void playback.api.togglePlay()}
          downloadDisabled={playback.downloadDisabled}
          onDownload={() => playback.api.download()}
          settingsOpen={settingsOpen}
          onToggleSettings={() => setSettingsOpen((open) => !open)}
        />
        <SettingsPanel open={settingsOpen} settings={settings} generationBusy={generation.busy} />
      </section>
    </main>
  );
}
