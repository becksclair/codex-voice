import { useRef, useState, type ClipboardEvent } from "react";
import { clearPendingGeneration, deleteLastGeneratedAudio, TEXT_STORAGE_KEY } from "./lib/index.ts";
import { GenerateBar } from "./components/GenerateBar.tsx";
import { SettingsPanel } from "./components/SettingsPanel.tsx";
import { TextEditor } from "./components/TextEditor.tsx";
import { WaveformPlayer } from "./components/WaveformPlayer.tsx";
import { useGeneration } from "./hooks/useGeneration.ts";
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

  const charCount = Array.from(text).length;

  const handleNativePaste = (event: ClipboardEvent<HTMLTextAreaElement>): void => {
    if (settings.settings.generateOnPaste === false) return;
    const pasted = event.clipboardData?.getData("text") || "";
    if (!pasted.trim()) return;
    const valueBeforePaste = textRef.current?.value ?? "";
    setTimeout(() => {
      const current = textRef.current?.value ?? "";
      if (current === valueBeforePaste || !current.trim()) return;
      generation.generate().catch((e: Error) => errorApi.show(e.message || "TTS failed."));
    }, 0);
  };

  const handlePasteClick = async (): Promise<void> => {
    try {
      const value = await navigator.clipboard.readText();
      if (!value) {
        setText("", { persist: false });
        localStorage.removeItem(TEXT_STORAGE_KEY);
        return;
      }
      setText(value);
      errorApi.clear();
      if (settings.settings.generateOnPaste !== false) await generation.generate();
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
    <main>
      <header>
        <img className="app-icon" src="/web/icon-192.png" alt="Codex Voice" />
        <div className="header-actions">
          <span id="count">
            {charCount} {charCount === 1 ? "char" : "chars"}
          </span>
        </div>
      </header>
      <div
        id="error-banner"
        className={error ? "error-banner visible" : "error-banner"}
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
      <section className="controls">
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
        <SettingsPanel open={settingsOpen} settings={settings} />
      </section>
    </main>
  );
}
