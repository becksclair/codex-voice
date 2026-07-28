import { useEffect, useRef, useState, type RefObject } from "react";
import { audioBlobFromBase64, type BrowserTtsConfig, type WebSettings } from "../lib/index.ts";
import {
  cancelWebSpeechJob,
  createWebSpeechJob,
  prepareWebSpeech,
  waitForWebSpeechJob,
} from "../lib/synth/serverJobs.ts";
import { resolvePersona, resolveProvider } from "../lib/personas.ts";
import {
  canStreamElevenLabsDirect,
  canStreamGoogleDirect,
  streamElevenLabsDirect,
  streamGoogleDirect,
} from "../lib/audio/direct-stream.ts";
import type { SetText } from "./usePersistedText.ts";
import type { TextMirrorElement } from "../components/TextEditor.tsx";
import type { PlaybackApi } from "./usePlayback.ts";
import type { WaveformRef } from "./useWaveform.ts";

interface UseGenerationOptions {
  config: BrowserTtsConfig | null;
  settings: WebSettings;
  textRef: RefObject<TextMirrorElement | null>;
  setText: SetText;
  playback: PlaybackApi;
  waveformRef: WaveformRef;
  showError: (message: string) => void;
  clearError: () => void;
}

export interface GenerationState {
  busy: boolean;
  generating: boolean;
  progress: number;
  label: string;
  generate: (inputOverride?: string) => Promise<boolean>;
  toggleActive: () => void;
  cancelActive: () => void;
}

function jobOptions(config: BrowserTtsConfig, settings: WebSettings) {
  const provider = settings.provider === "auto" ? undefined : settings.provider;
  const effectiveProvider = provider ?? config.defaultProvider;
  const voice =
    settings.voice === "provider-default" && effectiveProvider === "google"
      ? config.providers.google?.voice
      : settings.voice.startsWith("persona:")
        ? settings.voice.slice("persona:".length)
        : undefined;
  const model = settings.model === "default" ? undefined : settings.model.split(":", 2)[1];
  return {
    provider,
    voice,
    model,
    speechPrepEnabled: settings.emotionPreprocessing,
    speechPrepShortenEnabled: settings.summarization,
  };
}

export function useGeneration(options: UseGenerationOptions): GenerationState {
  const { config, settings, textRef, setText, playback, waveformRef, showError, clearError } =
    options;
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState(0);
  const [label, setLabel] = useState("Generate");
  const activeJob = useRef<string | null>(null);
  const abort = useRef<AbortController | null>(null);
  const resetTimer = useRef<number | null>(null);
  const releaseJobBestEffort = (job: string): void => {
    void cancelWebSpeechJob(job).catch(() => {});
  };

  const cancelActive = (): void => {
    if (resetTimer.current !== null) {
      window.clearTimeout(resetTimer.current);
      resetTimer.current = null;
    }
    abort.current?.abort();
    abort.current = null;
    const job = activeJob.current;
    activeJob.current = null;
    if (job) releaseJobBestEffort(job);
    setBusy(false);
    setProgress(0);
    setLabel("Generate");
  };

  useEffect(() => {
    const cancelOnUnload = (): void => {
      const job = activeJob.current;
      if (job) releaseJobBestEffort(job);
    };
    window.addEventListener("pagehide", cancelOnUnload);
    return () => {
      window.removeEventListener("pagehide", cancelOnUnload);
      cancelActive();
    };
    // Cancellation owns only refs and stable React setters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const generate = async (inputOverride?: string): Promise<boolean> => {
    const input = (inputOverride ?? textRef.current?.value ?? "").trim();
    if (!input) {
      showError("Enter some text first.");
      return false;
    }
    if (!config) {
      showError("Speech backend is unavailable.");
      return false;
    }
    cancelActive();
    const controller = new AbortController();
    abort.current = controller;
    setBusy(true);
    setLabel("Starting");
    setProgress(0.08);
    clearError();
    try {
      const options = jobOptions(config, settings);
      const persona = resolvePersona(config, settings);
      const provider = resolveProvider(config, persona, settings);
      const canStreamDirect =
        (provider === "elevenlabs" &&
          Boolean(persona?.elevenlabs) &&
          canStreamElevenLabsDirect(config, settings.model)) ||
        (provider === "google" && canStreamGoogleDirect(config, settings.model));
      if (canStreamDirect) {
        let attemptedStream: import("../lib/audio/direct-stream.ts").DirectStreamPlayback | null =
          null;
        try {
          setLabel("Preparing");
          setProgress(0.25);
          const prepared = await prepareWebSpeech(input, controller.signal, options);
          if (prepared.input_changed) setText(prepared.input, { persist: false });
          setLabel("Connecting");
          setProgress(0.45);
          const callbacks = {
            onReady: (stream: import("../lib/audio/direct-stream.ts").DirectStreamPlayback) => {
              attemptedStream = stream;
              playback.attachStream(stream);
            },
            onPlayingChange: (playing: boolean) => playback.onStreamPlayingChange(playing),
            onProgress: (current: number, buffered: number, finished: boolean) =>
              playback.onStreamProgress(current, buffered, finished),
            onWaveformChunk: (peaks: number[], durationDelta: number, sampleRate: number) =>
              waveformRef.current?.appendStreamingPeaks(peaks, durationDelta, sampleRate, 1),
            onWaveformSnapshot: (peaks: number[], duration: number) =>
              waveformRef.current?.replaceStreamingPeaks(peaks, duration),
            onFinished: () => waveformRef.current?.markStreamFinished(),
            onDrained: () => playback.onStreamDrained(),
          };
          const streamed =
            provider === "google"
              ? await streamGoogleDirect(
                  config,
                  prepared.input,
                  persona,
                  settings.model,
                  controller.signal,
                  callbacks,
                )
              : await streamElevenLabsDirect(
                  config,
                  prepared.input,
                  persona!,
                  settings.model,
                  controller.signal,
                  callbacks,
                );
          playback.finishStream(streamed.blob, streamed.playback);
          setProgress(1);
          setLabel("Done");
          return true;
        } catch (streamError) {
          playback.failStream(attemptedStream);
          if (controller.signal.aborted) throw streamError;
          setLabel("Server fallback");
          setProgress(0.35);
        }
      }
      const job = await createWebSpeechJob(input, controller.signal, options);
      activeJob.current = job;
      let result;
      try {
        result = await waitForWebSpeechJob(job, {
          signal: controller.signal,
          onProgress: (value, nextLabel) => {
            setProgress(value);
            setLabel(nextLabel);
          },
        });
      } finally {
        if (activeJob.current === job) activeJob.current = null;
        releaseJobBestEffort(job);
      }
      if (result.input_changed) setText(result.input, { persist: false });
      playback.loadAudioBlob(audioBlobFromBase64(result.audio_base64, result.mime_type));
      setProgress(1);
      setLabel("Done");
      return true;
    } catch (cause) {
      if (!controller.signal.aborted) showError((cause as Error).message || "TTS failed.");
      return false;
    } finally {
      if (abort.current === controller) {
        abort.current = null;
        setBusy(false);
        resetTimer.current = window.setTimeout(() => {
          resetTimer.current = null;
          if (!abort.current) {
            setProgress(0);
            setLabel("Generate");
          }
        }, 350);
      }
    }
  };

  const toggleActive = (): void => {
    if (abort.current) {
      cancelActive();
    } else {
      void generate();
    }
  };

  return { busy, generating: busy, progress, label, generate, toggleActive, cancelActive };
}
