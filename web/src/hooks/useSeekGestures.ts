import { useRef, type PointerEvent as ReactPointerEvent, type RefObject } from "react";
import { clamp } from "../lib/index.ts";
import type { PlaybackState } from "./usePlayback.ts";
import type { WaveformRef } from "./useWaveform.ts";

/** Pointer + keyboard handlers for the waveform seek slider. */
export interface SeekHandlers {
  onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerUp: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerCancel: () => void;
  onKeyDown: (event: React.KeyboardEvent<HTMLDivElement>) => void;
}

/**
 * The seek-slider gesture state machine.
 *
 * Ports `handleWaveformPointer`, the pointer listeners, `showKeyboardScrubFeedback`,
 * and the keyboard seek handler. The `scrubbing` class is toggled imperatively on
 * the slider (it is not otherwise React-controlled), and the shared `seekingRef`
 * suppresses the audio position sync while dragging.
 */
export function useSeekGestures(
  sliderRef: RefObject<HTMLDivElement | null>,
  waveformRef: WaveformRef,
  playback: PlaybackState,
): SeekHandlers {
  const scrubTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const seekingRef = playback.seekingRef;

  const isDisabled = (): boolean => sliderRef.current?.getAttribute("aria-disabled") === "true";

  const handlePointer = (event: ReactPointerEvent<HTMLDivElement>): void => {
    const waveform = waveformRef.current;
    if (isDisabled() || !waveform) return;
    seekingRef.current = true;
    sliderRef.current?.classList.add("scrubbing");
    playback.api.seekToWaveformTime(waveform.seekTimeFromClientX(event.clientX));
    event.preventDefault();
  };

  const showKeyboardScrubFeedback = (): void => {
    sliderRef.current?.classList.add("scrubbing");
    if (scrubTimerRef.current) clearTimeout(scrubTimerRef.current);
    scrubTimerRef.current = setTimeout(() => {
      scrubTimerRef.current = null;
      if (!seekingRef.current) sliderRef.current?.classList.remove("scrubbing");
    }, 420);
  };

  return {
    onPointerDown: (event) => {
      if (isDisabled()) return;
      sliderRef.current?.setPointerCapture?.(event.pointerId);
      handlePointer(event);
    },
    onPointerMove: (event) => {
      if (!seekingRef.current) return;
      handlePointer(event);
    },
    onPointerUp: (event) => {
      if (!seekingRef.current) return;
      handlePointer(event);
      seekingRef.current = false;
      sliderRef.current?.classList.remove("scrubbing");
    },
    onPointerCancel: () => {
      seekingRef.current = false;
      sliderRef.current?.classList.remove("scrubbing");
    },
    onKeyDown: (event) => {
      const waveform = waveformRef.current;
      if (isDisabled() || !waveform) return;
      const max = waveform.mode === "complete" ? waveform.duration : waveform.bufferedDuration || 0;
      const step = event.shiftKey ? 10 : 5;
      let target: number | null = null;
      if (event.key === "ArrowLeft" || event.key === "ArrowDown") {
        target = waveform.currentTime - step;
      }
      if (event.key === "ArrowRight" || event.key === "ArrowUp") {
        target = waveform.currentTime + step;
      }
      if (event.key === "Home") target = 0;
      if (event.key === "End") target = max;
      if (target !== null) {
        event.preventDefault();
        showKeyboardScrubFeedback();
        playback.api.seekToWaveformTime(clamp(target, 0, max));
      }
    },
  };
}
