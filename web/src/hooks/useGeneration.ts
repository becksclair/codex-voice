import { useEffect, useRef, useState, type RefObject } from "react";
import {
  clamp,
  getLastGeneratedAudio,
  loadPendingGeneration,
  loadText,
  saveText,
  shouldApplyGeneratedText,
  type BrowserTtsConfig,
  type WebSettings,
} from "../lib/index.ts";
import type { GenerationController, GenerationMeta } from "../lib/generation.ts";
import { reloadForWorkerUpdateWhenIdle, setBusyPredicate } from "../pwa.ts";
import type { SetText } from "./usePersistedText.ts";
import type { PlaybackApi } from "./usePlayback.ts";
import type { WaveformRef } from "./useWaveform.ts";

interface UseGenerationOptions {
  config: BrowserTtsConfig | null;
  settings: WebSettings;
  textRef: RefObject<HTMLTextAreaElement | null>;
  setText: SetText;
  playback: PlaybackApi;
  waveformRef: WaveformRef;
  showError: (message: string) => void;
  clearError: () => void;
}

/** The public surface of {@link useGeneration}. */
export interface GenerationState {
  /** Whether the generate button is busy (disabled) — mirrors `generate.disabled`. */
  busy: boolean;
  /** Whether the spinner/`.generating` treatment is showing. */
  generating: boolean;
  /** Progress fraction (0..1) for the generate button's progress bar. */
  progress: number;
  /** The generate button's label text. */
  label: string;
  /** Generate the supplied text, or the current draft when omitted. */
  generate: (inputOverride?: string) => Promise<boolean>;
  /** Cancel any active run (used by the clear button). */
  cancelActive: () => void;
}

/**
 * Owns the {@link GenerationController} lifecycle and translates its callbacks
 * into React state + playback/waveform side effects.
 *
 * Replaces the legacy controller construction, the `setGenerating`/`showError`
 * button wiring, the pagehide/pageshow/visibilitychange lifecycle handlers, and
 * the stored-state restore/resume bootstrap.
 *
 * The generation pipeline (`lib/generation.ts` and its speech-prep/synthesis/
 * streaming dependencies) is loaded lazily via dynamic `import()`: the shell
 * boots with only the editor, settings, and restored-audio playback. The
 * controller is constructed on the first generate, or eagerly at load only when
 * a pending generation must be resumed.
 */
export function useGeneration(options: UseGenerationOptions): GenerationState {
  const { config, settings, textRef, setText, playback, waveformRef, showError, clearError } =
    options;

  const [busy, setBusy] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [progress, setProgress] = useState(0);
  const [label, setLabel] = useState("Generate");

  const controllerRef = useRef<GenerationController | null>(null);
  const ensureControllerRef = useRef<(() => Promise<GenerationController>) | null>(null);
  const activeRef = useRef(false);
  // Generation epoch: bumped by cancel/clear/unmount so a generate() that is
  // still awaiting the lazy pipeline import can tell it was abandoned. Without
  // this, a cancel issued before the controller exists is a no-op and the
  // awaited generate resurrects the just-cleared state.
  const epochRef = useRef(0);
  const configRef = useRef(config);
  const settingsRef = useRef(settings);
  configRef.current = config;
  settingsRef.current = settings;

  // Keep the controller's config/settings in sync (mirrors `controller.update`).
  useEffect(() => {
    controllerRef.current?.update({ config, settings });
  }, [config, settings]);

  useEffect(() => {
    let disposed = false;
    const getDraftText = (): string => textRef.current?.value || loadText() || "";

    const setGeneratingClass = (active: boolean, text: string, fraction: number): void => {
      setGenerating(active);
      setLabel(text);
      setProgress(clamp(fraction, 0, 1));
    };

    // Construct the controller with the current config/settings; the sync effect
    // above keeps it current afterwards.
    const buildController = (Controller: typeof GenerationController): GenerationController =>
      new Controller({
        config: configRef.current,
        settings: settingsRef.current,
        getDraftText,
        callbacks: {
          onStatus: (statusLabel, fraction) => {
            // The controller already clamps the fraction to [0, 1].
            setProgress(fraction);
            setLabel(statusLabel);
          },
          onGeneratingChange: (active) => {
            if (active) {
              activeRef.current = true;
              setBusy(true);
              playback.setPlayDisabled(true);
              setGeneratingClass(true, "Starting", 0.08);
            } else {
              activeRef.current = false;
              setBusy(false);
              setTimeout(() => {
                if (!activeRef.current) setGeneratingClass(false, "Generate", 0);
              }, 350);
              reloadForWorkerUpdateWhenIdle();
            }
          },
          onError: (message) => {
            playback.setPlayDisabled(!playback.audioHasSrc());
            if (!playback.audioHasSrc() && !playback.hasStream()) waveformRef.current?.reset();
            showError(message);
          },
          onClearError: clearError,
          onTextReplace: (value) => setText(value, { persist: false }),
          onAudioReady: (blob, meta: GenerationMeta) => {
            if (meta.streamed && meta.playback) {
              playback.onStreamAudioReady(blob, meta.playback.stopped ? null : meta.playback);
            } else {
              playback.loadAudioBlob(blob);
            }
          },
          playbackCallbacks: {
            onPlayingChange: (playing) => playback.onPlayingChange(playing),
            onProgress: (current, estimated, finished) =>
              playback.onStreamProgress(current, estimated, finished),
            onPeaks: (peaks, durationDelta) =>
              waveformRef.current?.appendStreamingPeaks(peaks, durationDelta),
            onFinished: () => waveformRef.current?.markStreamFinished(),
            onReplayReady: (blob) => playback.onReplayReady(blob),
          },
          onStreamState: (state) => playback.onStreamState(state),
        },
      });

    // Lazily import + construct the controller. Idempotent: repeated calls
    // resolve to the same instance. Serialized via a pending promise so two
    // concurrent triggers (e.g. generate + a resume) don't build two controllers.
    let pending: Promise<GenerationController> | null = null;
    const ensureController = (): Promise<GenerationController> => {
      if (controllerRef.current) return Promise.resolve(controllerRef.current);
      if (pending) return pending;
      pending = import("../lib/generation.ts").then((mod) => {
        if (controllerRef.current) return controllerRef.current;
        const controller = buildController(mod.GenerationController);
        controllerRef.current = controller;
        activeRef.current = false;
        if (disposed) controller.cancel();
        return controller;
      });
      return pending;
    };
    ensureControllerRef.current = ensureController;

    setBusyPredicate(() => activeRef.current || playback.hasStream());

    const restoreLastGeneratedAudio = async (): Promise<void> => {
      try {
        const record = await getLastGeneratedAudio();
        if (!record?.blob) return;
        if (
          typeof record.text === "string" &&
          shouldApplyGeneratedText(getDraftText(), record.text, record.text)
        ) {
          setText(record.text);
        }
        playback.loadAudioBlob(record.blob);
        clearError();
      } catch {
        // Ignored, matching app.html behavior.
      }
    };

    void (async () => {
      await restoreLastGeneratedAudio();
      if (loadPendingGeneration()) {
        const controller = await ensureController();
        await controller.resumePending();
      }
    })();

    const cleanups: Array<() => void> = [];
    const on = (target: EventTarget, type: string, handler: EventListener): void => {
      target.addEventListener(type, handler);
      cleanups.push(() => target.removeEventListener(type, handler));
    };

    on(window, "pagehide", () => {
      if (activeRef.current) controllerRef.current?.markLifecycleInterrupted();
      saveText(textRef.current?.value || "");
    });
    on(window, "pageshow", (event) => {
      if ((event as PageTransitionEvent).persisted && !playback.audioHasSrc()) {
        void restoreLastGeneratedAudio();
      }
      if (activeRef.current) setGeneratingClass(true, "Generating", 0.45);
      if (!activeRef.current && loadPendingGeneration()) {
        void ensureController().then((controller) => controller.resumePending());
      }
    });
    on(document, "visibilitychange", () => {
      if (document.visibilityState !== "visible" && activeRef.current) {
        controllerRef.current?.markLifecycleInterrupted();
        return;
      }
      if (document.visibilityState === "visible" && activeRef.current) {
        setGeneratingClass(true, "Generating", 0.45);
      }
      if (document.visibilityState === "visible" && !activeRef.current && loadPendingGeneration()) {
        void ensureController().then((controller) => controller.resumePending());
      }
    });

    return () => {
      disposed = true;
      epochRef.current += 1;
      for (const cleanup of cleanups) cleanup();
      controllerRef.current?.cancel();
      controllerRef.current = null;
      ensureControllerRef.current = null;
      setBusyPredicate(() => false);
    };
    // Mount-once setup of the generation controller + lifecycle listeners; the
    // captured callbacks only touch refs and stable setters, so they are stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const generate = async (inputOverride?: string): Promise<boolean> => {
    const input = (inputOverride ?? textRef.current?.value ?? "").trim();
    if (!input) {
      showError("Enter some text first.");
      return false;
    }
    const ensureController = ensureControllerRef.current;
    if (!ensureController) return false;
    const epoch = epochRef.current;
    const controller = await ensureController();
    // Abandoned while the pipeline import was in flight (cancel/clear/unmount).
    if (epoch !== epochRef.current) return false;
    if (controller.isActive) {
      controller.cancel();
      activeRef.current = false;
      setBusy(false);
    }
    await controller.generate(input);
    return true;
  };

  const cancelActive = (): void => {
    epochRef.current += 1;
    const controller = controllerRef.current;
    if (controller?.isActive) {
      controller.cancel();
      activeRef.current = false;
      setBusy(false);
      setGenerating(false);
      setLabel("Generate");
      setProgress(0);
    }
  };

  return { busy, generating, progress, label, generate, cancelActive };
}
