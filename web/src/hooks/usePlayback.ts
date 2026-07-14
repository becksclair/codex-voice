import { useEffect, useRef, useState } from "react";
import { audioDownloadExtension, formatTime } from "../format.ts";
import type { StreamingPlayback, StreamState } from "../lib/audio/streaming.ts";
import { reloadForWorkerUpdateWhenIdle } from "../pwa.ts";
import type { WaveformRef } from "./useWaveform.ts";

/**
 * Imperative playback control surface. Stable across renders: every method
 * closes over refs and the (stable) React state setters, so it can be captured
 * once by the generation controller and the seek gestures.
 */
export interface PlaybackApi {
  /** Load a finished audio blob into the `<audio>` element and waveform. */
  loadAudioBlob(blob: Blob): void;
  /** Tear down the current audio + streaming playback and reset the display. */
  resetAudio(): void;
  /** Toggle play/pause for the current stream or `<audio>` source. */
  togglePlay(): Promise<void>;
  /** Download the current audio blob. */
  download(): void;
  /** Seek the current stream or `<audio>` source to `seconds`. */
  seekToWaveformTime(seconds: number): void;
  /** Whether the `<audio>` element currently has a source. */
  audioHasSrc(): boolean;
  /** Whether a live streaming playback is attached. */
  hasStream(): boolean;
  /** Enable/disable the play button (streaming edge cases). */
  setPlayDisabled(disabled: boolean): void;
  /** A streamed blob became ready: enable play/download and attach the stream. */
  onStreamAudioReady(blob: Blob, playback: StreamingPlayback | null): void;
  /** Streaming playing-state changed (mirrors `playSvg(!playing)`). */
  onPlayingChange(playing: boolean): void;
  /** Attach/detach live playback independently from the final downloadable blob. */
  onStreamPlaybackChange(playback: StreamingPlayback | null): void;
  /** Streaming progress tick. */
  onStreamProgress(current: number, estimated: number, finished: boolean): void;
  /** High-level streaming state passthrough (handles the buffering reset). */
  onStreamState(state: StreamState): void;
  /** The streamed run drained to a replay blob. */
  onReplayReady(blob: Blob): void;
}

/** The public surface of {@link usePlayback}. */
export interface PlaybackState {
  paused: boolean;
  elapsed: string;
  duration: string;
  playDisabled: boolean;
  downloadDisabled: boolean;
  api: PlaybackApi;
  /** Shared "is the user scrubbing" flag (read by seek gestures + progress). */
  seekingRef: React.RefObject<boolean>;
}

/**
 * Owns the `<audio>` element and all playback display state.
 *
 * Replaces the legacy `audio`/`streamPlayback` block: `resetAudio`,
 * `loadAudioBlob`, `downloadCurrentAudio`, `updatePosition`, the seek helpers,
 * the streaming-callback side effects, and the `playSvg` icon toggle (now the
 * `paused` state). The generation controller drives this through {@link PlaybackApi}.
 */
export function usePlayback(
  waveformRef: WaveformRef,
  showError: (message: string) => void,
  clearError: () => void,
): PlaybackState {
  const [paused, setPaused] = useState(true);
  const [elapsed, setElapsed] = useState("0:00");
  const [duration, setDuration] = useState("0:00");
  const [playDisabled, setPlayDisabled] = useState(true);
  const [downloadDisabled, setDownloadDisabled] = useState(true);

  const audioRef = useRef<HTMLAudioElement | null>(null);
  const objectUrlRef = useRef<string | null>(null);
  const currentBlobRef = useRef<Blob | null>(null);
  const streamRef = useRef<StreamingPlayback | null>(null);
  const seekingRef = useRef(false);

  // The audio element + its listeners are a true external system.
  useEffect(() => {
    const audio = new Audio();
    audioRef.current = audio;

    const updatePosition = (): void => {
      const total = audio.duration || 0;
      if (!seekingRef.current && total > 0) waveformRef.current?.setCurrent(audio.currentTime);
      setElapsed(formatTime(audio.currentTime));
      setDuration(formatTime(total));
    };
    const onPlay = (): void => {
      setPaused(false);
      clearError();
    };
    const onPause = (): void => setPaused(true);
    const onEnded = (): void => {
      setPaused(true);
      updatePosition();
    };
    audio.addEventListener("loadedmetadata", updatePosition);
    audio.addEventListener("timeupdate", updatePosition);
    audio.addEventListener("play", onPlay);
    audio.addEventListener("pause", onPause);
    audio.addEventListener("ended", onEnded);

    return () => {
      audio.removeEventListener("loadedmetadata", updatePosition);
      audio.removeEventListener("timeupdate", updatePosition);
      audio.removeEventListener("play", onPlay);
      audio.removeEventListener("pause", onPause);
      audio.removeEventListener("ended", onEnded);
      audio.pause();
      audio.removeAttribute("src");
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
      audioRef.current = null;
    };
    // Mount-once setup of the <audio> element + its listeners (external system).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Built once; every method only touches refs + stable setters.
  const apiRef = useRef<PlaybackApi | null>(null);
  if (apiRef.current === null) {
    const stopStream = (): void => {
      const playback = streamRef.current;
      if (!playback) return;
      streamRef.current = null;
      playback.stop();
      reloadForWorkerUpdateWhenIdle();
    };

    const resetAudio = (): void => {
      stopStream();
      const audio = audioRef.current;
      if (audio) {
        audio.pause();
        audio.removeAttribute("src");
        audio.load();
      }
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
      currentBlobRef.current = null;
      setPlayDisabled(true);
      setDownloadDisabled(true);
      setPaused(true);
      setElapsed("0:00");
      setDuration("0:00");
      waveformRef.current?.reset();
    };

    const loadAudioBlob = (blob: Blob): void => {
      resetAudio();
      const audio = audioRef.current;
      if (!audio) return;
      currentBlobRef.current = blob;
      const url = URL.createObjectURL(blob);
      objectUrlRef.current = url;
      audio.src = url;
      audio.load();
      setPlayDisabled(false);
      setDownloadDisabled(false);
      void waveformRef.current?.decodeBlob(blob, audio.currentTime || 0);
    };

    const seekToWaveformTime = (seconds: number): void => {
      const target = Math.max(0, Number(seconds) || 0);
      if (streamRef.current) {
        streamRef.current
          .seekTo(target)
          .catch((error: Error) => showError(error.message || "Seek failed."));
        return;
      }
      const audio = audioRef.current;
      const total = audio?.duration || 0;
      if (audio && total > 0) {
        audio.currentTime = Math.min(Math.max(target, 0), total);
        setElapsed(formatTime(audio.currentTime));
        setDuration(formatTime(total));
        if (!seekingRef.current) waveformRef.current?.setCurrent(audio.currentTime);
      }
    };

    apiRef.current = {
      loadAudioBlob,
      resetAudio,
      seekToWaveformTime,
      async togglePlay() {
        if (streamRef.current) {
          try {
            await streamRef.current.toggle();
          } catch (error) {
            showError((error as Error).message || "Streaming playback failed.");
          }
          return;
        }
        const audio = audioRef.current;
        if (!audio?.src) return;
        if (audio.paused) {
          try {
            await audio.play();
          } catch (error) {
            showError((error as Error).message || "Playback failed.");
          }
        } else {
          audio.pause();
        }
      },
      download() {
        const blob = currentBlobRef.current;
        if (!blob) return;
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = `codex-voice-${new Date()
          .toISOString()
          .replace(/[:.]/g, "-")}.${audioDownloadExtension(blob)}`;
        document.body.append(link);
        link.click();
        link.remove();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      },
      audioHasSrc() {
        return Boolean(audioRef.current?.src);
      },
      hasStream() {
        return Boolean(streamRef.current);
      },
      setPlayDisabled(disabled) {
        setPlayDisabled(disabled);
      },
      onStreamAudioReady(blob, playback) {
        currentBlobRef.current = blob;
        setDownloadDisabled(false);
        setPlayDisabled(false);
        streamRef.current = playback;
      },
      onStreamPlaybackChange(playback) {
        if (playback) {
          resetAudio();
          streamRef.current = playback;
          waveformRef.current?.resetStreaming();
          setDuration("Live");
          setPlayDisabled(false);
          setPaused(!playback.playing);
          return;
        }
        streamRef.current = null;
        setPaused(true);
        setPlayDisabled(!audioRef.current?.src);
      },
      onPlayingChange(playing) {
        setPaused(!playing);
      },
      onStreamProgress(current, estimated, finished) {
        setElapsed(formatTime(current));
        setDuration(finished ? formatTime(estimated) : "Live");
        waveformRef.current?.setCurrent(current);
      },
      onStreamState(state) {
        if (state === "buffering") {
          setDuration("Live");
          setPlayDisabled(false);
        }
      },
      onReplayReady(blob) {
        streamRef.current = null;
        loadAudioBlob(blob);
        reloadForWorkerUpdateWhenIdle();
      },
    };
  }

  return {
    paused,
    elapsed,
    duration,
    playDisabled,
    downloadDisabled,
    api: apiRef.current,
    seekingRef,
  };
}
