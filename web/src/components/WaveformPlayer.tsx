import type { RefObject } from "react";
import type { SeekHandlers } from "../hooks/useSeekGestures.ts";

interface WaveformPlayerProps {
  elapsed: string;
  duration: string;
  sliderRef: RefObject<HTMLDivElement | null>;
  canvasRef: RefObject<HTMLCanvasElement | null>;
  seek: SeekHandlers;
}

/**
 * The scrubber: elapsed/duration times and the waveform seek slider (`#elapsed`,
 * `#duration`, `#waveform-slider`, `#waveform`).
 *
 * The slider's aria-* attributes, `--seek-progress`, and `scrubbing` class are
 * driven imperatively by the {@link WaveformController}; the JSX only supplies
 * the initial (disabled) state.
 */
export function WaveformPlayer(props: WaveformPlayerProps) {
  return (
    <div className="scrubber">
      <time id="elapsed">{props.elapsed}</time>
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
        ref={props.sliderRef}
        onPointerDown={props.seek.onPointerDown}
        onPointerMove={props.seek.onPointerMove}
        onPointerUp={props.seek.onPointerUp}
        onPointerCancel={props.seek.onPointerCancel}
        onKeyDown={props.seek.onKeyDown}
      >
        <canvas id="waveform" aria-hidden="true" ref={props.canvasRef}></canvas>
        <span className="waveform-marker" aria-hidden="true"></span>
        <span className="waveform-thumb" aria-hidden="true"></span>
      </div>
      <time id="duration">{props.duration}</time>
    </div>
  );
}
