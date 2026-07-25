import { useEffect, useRef, useState } from "react";
import { audioDownloadExtension, formatTime } from "../format.ts";
import type { WaveformRef } from "./useWaveform.ts";
import type { DirectStreamPlayback } from "../lib/audio/direct-stream.ts";

export interface PlaybackApi {
  loadAudioBlob(blob: Blob): void;
  resetAudio(): void;
  togglePlay(): Promise<void>;
  download(): void;
  seekToWaveformTime(seconds: number): void;
  audioHasSrc(): boolean;
  hasStream(): boolean;
  setPlayDisabled(disabled: boolean): void;
  attachStream(stream: DirectStreamPlayback): void;
  finishStream(blob: Blob, stream: DirectStreamPlayback): void;
  failStream(stream: DirectStreamPlayback | null): void;
  onStreamDrained(): void;
  onStreamPlayingChange(playing: boolean): void;
  onStreamProgress(current: number, buffered: number, finished: boolean): void;
}

export interface PlaybackState {
  paused: boolean;
  elapsed: string;
  duration: string;
  playDisabled: boolean;
  downloadDisabled: boolean;
  api: PlaybackApi;
  seekingRef: React.RefObject<boolean>;
}

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
  const blobRef = useRef<Blob | null>(null);
  const urlRef = useRef<string | null>(null);
  const seekingRef = useRef(false);
  const streamRef = useRef<DirectStreamPlayback | null>(null);
  const streamDrainedRef = useRef(false);

  useEffect(() => {
    const audio = new Audio();
    audioRef.current = audio;
    const update = (): void => {
      const total = Number.isFinite(audio.duration) ? audio.duration : 0;
      if (!seekingRef.current && total > 0) waveformRef.current?.setCurrent(audio.currentTime);
      setElapsed(formatTime(audio.currentTime));
      setDuration(formatTime(total));
    };
    const onPlay = (): void => {
      setPaused(false);
      clearError();
    };
    const onPause = (): void => setPaused(true);
    audio.addEventListener("loadedmetadata", update);
    audio.addEventListener("timeupdate", update);
    audio.addEventListener("play", onPlay);
    audio.addEventListener("pause", onPause);
    audio.addEventListener("ended", onPause);
    return () => {
      streamRef.current?.stop();
      streamRef.current = null;
      audio.pause();
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
      audioRef.current = null;
    };
    // The audio element is a mount-scoped external system.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const resetAudio = (): void => {
    streamRef.current?.stop();
    streamRef.current = null;
    streamDrainedRef.current = false;
    const audio = audioRef.current;
    audio?.pause();
    if (audio) audio.removeAttribute("src");
    if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    urlRef.current = null;
    blobRef.current = null;
    waveformRef.current?.reset();
    setPaused(true);
    setElapsed("0:00");
    setDuration("0:00");
    setPlayDisabled(true);
    setDownloadDisabled(true);
  };

  const api = useRef<PlaybackApi>({
    loadAudioBlob(blob) {
      resetAudio();
      blobRef.current = blob;
      const url = URL.createObjectURL(blob);
      urlRef.current = url;
      const audio = audioRef.current;
      if (audio) {
        audio.src = url;
        audio.load();
      }
      setPlayDisabled(false);
      setDownloadDisabled(false);
      void waveformRef.current?.decodeBlob(blob);
    },
    resetAudio,
    async togglePlay() {
      if (streamRef.current) {
        try {
          await streamRef.current.toggle();
        } catch (cause) {
          showError((cause as Error).message || "Streaming playback failed.");
        }
        return;
      }
      const audio = audioRef.current;
      if (!audio?.src) return;
      try {
        if (audio.paused) await audio.play();
        else audio.pause();
      } catch (cause) {
        showError((cause as Error).message || "Audio playback failed.");
      }
    },
    download() {
      const blob = blobRef.current;
      if (!blob) return;
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `codex-voice.${audioDownloadExtension(blob)}`;
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
    },
    seekToWaveformTime(seconds) {
      if (streamRef.current) {
        void streamRef.current
          .seekTo(seconds)
          .catch((cause) => showError((cause as Error).message || "Seek failed."));
        return;
      }
      const audio = audioRef.current;
      if (audio?.src) audio.currentTime = seconds;
    },
    audioHasSrc: () => Boolean(audioRef.current?.src),
    hasStream: () => Boolean(streamRef.current),
    setPlayDisabled,
    attachStream(stream) {
      resetAudio();
      streamRef.current = stream;
      streamDrainedRef.current = false;
      waveformRef.current?.resetStreaming(24_000, 1, stream.seekable);
      setPaused(!stream.playing);
      setDuration("Live");
      setPlayDisabled(false);
    },
    finishStream(blob, stream) {
      if (streamRef.current !== stream) return;
      blobRef.current = blob;
      setDownloadDisabled(false);
      setPlayDisabled(false);
      if (streamDrainedRef.current) {
        streamRef.current = null;
        stream.stop();
        api.loadAudioBlob(blob);
      }
    },
    failStream(stream) {
      if (!stream || streamRef.current !== stream) return;
      resetAudio();
    },
    onStreamDrained() {
      streamDrainedRef.current = true;
      const blob = blobRef.current;
      if (!blob) return;
      const stream = streamRef.current;
      streamRef.current = null;
      stream?.stop();
      api.loadAudioBlob(blob);
    },
    onStreamPlayingChange(playing) {
      setPaused(!playing);
    },
    onStreamProgress(current, buffered, finished) {
      setElapsed(formatTime(current));
      setDuration(finished ? formatTime(buffered) : "Live");
      waveformRef.current?.setCurrent(current);
    },
  }).current;

  return {
    paused,
    elapsed,
    duration,
    playDisabled,
    downloadDisabled,
    api,
    seekingRef,
  };
}
