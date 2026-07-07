/**
 * Canvas waveform renderer + seek-slider state machine.
 *
 * Ports the DOM/canvas half of the waveform code in app.html (lines
 * ~1050-1339): the `waveform` state object, `resetWaveform`,
 * `resetStreamingWaveform`, `setWaveformSeekable`, `setWaveformProgress`,
 * `waveformBufferedRatio`, `waveformPositionRatio`, `updateWaveformAria`,
 * `setWaveformCurrent`, `scheduleWaveformDraw`, `resizeWaveformCanvas`,
 * `drawRoundedBar`, `drawEmptyWaveform`, `drawPeakWaveform`, `drawWaveform`,
 * the decode-to-peaks wiring of `decodeWaveformBlob`, and the PCM append of
 * `appendStreamingWaveformPcm`. The pure peak math lives in
 * `lib/audio/waveform.ts`; this class owns only the canvas, the slider element,
 * and the mutable draw state.
 */

import { decodeAudioPeaks, peakContrastRange, samplePeaks } from "./lib/audio/waveform.ts";
import { clamp } from "./lib/util.ts";

type WaveformMode = "empty" | "streaming" | "complete";

interface WaveformState {
  mode: WaveformMode;
  peaks: number[];
  duration: number;
  bufferedDuration: number;
  currentTime: number;
  sampleRate: number;
  channels: number;
  finished: boolean;
  drawing: boolean;
  decodeId: number;
}

function newWaveformState(mode: WaveformMode = "empty"): WaveformState {
  return {
    mode,
    peaks: [],
    duration: 0,
    bufferedDuration: 0,
    currentTime: 0,
    sampleRate: 24000,
    channels: 1,
    finished: false,
    drawing: false,
    decodeId: 0,
  };
}

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "0:00";
  const whole = Math.floor(seconds);
  const minutes = Math.floor(whole / 60);
  return `${minutes}:${String(whole % 60).padStart(2, "0")}`;
}

/** Owns the `#waveform` canvas and the `#waveform-slider` element. */
export class WaveformController {
  private canvas: HTMLCanvasElement;
  private slider: HTMLElement;
  private state: WaveformState = newWaveformState("empty");
  private decodeIdCounter = 0;

  constructor(canvas: HTMLCanvasElement, slider: HTMLElement) {
    this.canvas = canvas;
    this.slider = slider;
  }

  get mode(): WaveformMode {
    return this.state.mode;
  }

  get currentTime(): number {
    return this.state.currentTime;
  }

  get duration(): number {
    return this.state.duration;
  }

  get bufferedDuration(): number {
    return this.state.bufferedDuration;
  }

  get isSeekable(): boolean {
    return this.slider.getAttribute("aria-disabled") !== "true";
  }

  /** Ports `resetWaveform`. */
  reset(): void {
    this.decodeIdCounter += 1;
    this.state = newWaveformState("empty");
    this.setSeekable(false);
    this.setProgress(0);
    this.scheduleDraw();
  }

  /** Ports `resetStreamingWaveform`. */
  resetStreaming(sampleRate = 24000, channels = 1): void {
    this.decodeIdCounter += 1;
    this.state = newWaveformState("streaming");
    this.state.sampleRate = sampleRate;
    this.state.channels = channels;
    this.setSeekable(false);
    this.setProgress(0);
    this.scheduleDraw();
  }

  /** Ports `setWaveformSeekable`. */
  private setSeekable(enabled: boolean): void {
    this.slider.setAttribute("aria-disabled", enabled ? "false" : "true");
    this.slider.tabIndex = enabled ? 0 : -1;
  }

  /** Ports `setWaveformProgress`. */
  private setProgress(progress: number): void {
    this.slider.style.setProperty("--seek-progress", String(clamp(progress, 0, 1)));
  }

  /** Ports `waveformBufferedRatio`. */
  private bufferedRatio(): number {
    const w = this.state;
    if (!w || w.mode !== "streaming") return 1;
    if (w.finished) return 1;
    if (w.bufferedDuration <= 0) return 0;
    return clamp(w.bufferedDuration / (w.bufferedDuration + 8), 0.12, 0.72);
  }

  /** Ports `waveformPositionRatio`. */
  private positionRatio(): number {
    const w = this.state;
    if (!w) return 0;
    if (w.mode === "complete") {
      return w.duration > 0 ? clamp(w.currentTime / w.duration, 0, 1) : 0;
    }
    if (w.mode === "streaming") {
      const loaded = this.bufferedRatio();
      return w.bufferedDuration > 0
        ? clamp((w.currentTime / w.bufferedDuration) * loaded, 0, loaded)
        : 0;
    }
    return 0;
  }

  /** Ports `updateWaveformAria`. */
  private updateAria(): void {
    const w = this.state;
    const max =
      w?.mode === "complete" ? w.duration : w?.mode === "streaming" ? w.bufferedDuration : 0;
    const now = Math.min(max || 0, Math.max(0, w?.currentTime || 0));
    this.slider.setAttribute("aria-valuemax", String(Math.round(max || 0)));
    this.slider.setAttribute("aria-valuenow", String(Math.round(now)));
    this.slider.setAttribute(
      "aria-valuetext",
      `${formatTime(now)} of ${max > 0 ? formatTime(max) : "0:00"}`,
    );
    this.setSeekable((w?.mode === "complete" && max > 0) || (w?.mode === "streaming" && max > 0));
  }

  /** Ports `setWaveformCurrent`. */
  setCurrent(seconds: number): void {
    const w = this.state;
    if (!w) return;
    const max = w.mode === "complete" ? w.duration : w.bufferedDuration;
    w.currentTime = clamp(Number(seconds) || 0, 0, max || 0);
    this.setProgress(this.positionRatio());
    this.updateAria();
    this.scheduleDraw();
  }

  private cssColor(name: string): string {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  /** Ports `scheduleWaveformDraw`. */
  scheduleDraw(): void {
    const w = this.state;
    if (!w || w.drawing) return;
    w.drawing = true;
    requestAnimationFrame(() => {
      w.drawing = false;
      if (this.state === w) this.draw();
    });
  }

  /** Ports `resizeWaveformCanvas`. */
  private resizeCanvas(rect: DOMRect): CanvasRenderingContext2D {
    const dpr = Math.max(1, window.devicePixelRatio || 1);
    const width = Math.max(1, Math.round(rect.width * dpr));
    const height = Math.max(1, Math.round(rect.height * dpr));
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
    const ctx = this.canvas.getContext("2d") as CanvasRenderingContext2D;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return ctx;
  }

  /** Ports `drawRoundedBar`. */
  private drawRoundedBar(
    ctx: CanvasRenderingContext2D,
    x: number,
    centerY: number,
    width: number,
    height: number,
    color: string,
  ): void {
    const top = centerY - height / 2;
    const radius = Math.min(width / 2, height / 2);
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.roundRect(x, top, width, height, radius);
    ctx.fill();
  }

  /** Ports `drawEmptyWaveform`. */
  private drawEmpty(
    ctx: CanvasRenderingContext2D,
    width: number,
    height: number,
    startX = 0,
  ): void {
    const centerY = height / 2;
    const color = this.cssColor("--waveform-empty");
    for (let x = startX; x < width; x += 5) {
      this.drawRoundedBar(ctx, x, centerY, 2, 2 + (Math.floor(x / 20) % 3) * 1.5, color);
    }
  }

  /** Ports `drawPeakWaveform`. */
  private drawPeaks(
    ctx: CanvasRenderingContext2D,
    width: number,
    height: number,
    loadedWidth: number,
    playedX: number,
  ): void {
    const gap = 2;
    const barWidth = 2;
    const step = barWidth + gap;
    const centerY = height / 2;
    const maxBar = Math.max(12, height * 0.86);
    const loadedCount = Math.max(1, Math.floor(loadedWidth / step));
    const peaks = samplePeaks(this.state.peaks, loadedCount);
    const contrast = peakContrastRange(peaks);
    const contrastRange = Math.max(0.08, contrast.ceiling - contrast.floor);
    for (let i = 0; i < loadedCount; i += 1) {
      const x = i * step;
      const peak = peaks[i] || 0;
      const relativePeak = clamp((peak - contrast.floor) / contrastRange, 0, 1);
      const visualPeak = clamp(Math.pow(relativePeak, 0.86) * 0.94 + peak * 0.08, 0, 1);
      const barHeight = Math.max(3, Math.min(maxBar, 3 + visualPeak * (maxBar - 3)));
      const color =
        x <= playedX ? this.cssColor("--waveform-played") : this.cssColor("--waveform-future");
      this.drawRoundedBar(ctx, x, centerY, barWidth, barHeight, color);
    }
    if (loadedWidth < width - step) this.drawEmpty(ctx, width, height, loadedWidth + step);
  }

  /** Ports `drawWaveform`. */
  private draw(): void {
    const rect = this.canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    const ctx = this.resizeCanvas(rect);
    ctx.clearRect(0, 0, rect.width, rect.height);
    const playedX = this.positionRatio() * rect.width;
    const w = this.state;
    if (!w || w.mode === "empty" || !w.peaks.length) {
      this.drawEmpty(ctx, rect.width, rect.height);
    } else {
      const loadedWidth = w.mode === "streaming" ? this.bufferedRatio() * rect.width : rect.width;
      this.drawPeaks(ctx, rect.width, rect.height, loadedWidth, playedX);
    }
    this.setProgress(this.positionRatio());
    this.updateAria();
  }

  /**
   * Decode a blob and populate complete-mode peaks. Ports the DOM/state half of
   * `decodeWaveformBlob`; the peak math is `decodeAudioPeaks` from lib.
   */
  async decodeBlob(blob: Blob, currentTime = 0): Promise<void> {
    const decodeId = this.decodeIdCounter + 1;
    this.decodeIdCounter = decodeId;
    this.state = { ...newWaveformState("complete"), decodeId };
    this.scheduleDraw();
    try {
      const decoded = await decodeAudioPeaks(blob);
      if (!decoded) {
        if (this.state.decodeId === decodeId) this.reset();
        return;
      }
      if (this.state.decodeId !== decodeId) return;
      this.state.mode = "complete";
      this.state.peaks = decoded.peaks;
      this.state.duration = decoded.duration;
      this.state.bufferedDuration = decoded.duration;
      this.state.currentTime = currentTime || 0;
      this.setSeekable(decoded.duration > 0);
      this.scheduleDraw();
    } catch (error) {
      console.warn(error);
      if (this.state.decodeId === decodeId) this.reset();
    }
  }

  /**
   * Append streaming peaks for a decoded PCM chunk. Ports the state-mutating
   * tail of `appendStreamingWaveformPcm`; `peaks`/`durationDelta` come from
   * `streamingPcmPeaks` (invoked inside `StreamingPlayback`).
   */
  appendStreamingPeaks(
    peaks: number[],
    durationDelta: number,
    sampleRate = 24000,
    channels = 1,
  ): void {
    if (!this.state || this.state.mode !== "streaming") this.resetStreaming(sampleRate, channels);
    this.state.sampleRate = sampleRate;
    this.state.channels = channels;
    for (const peak of peaks) this.state.peaks.push(peak);
    this.state.bufferedDuration += durationDelta;
    this.state.duration = this.state.bufferedDuration;
    this.setSeekable(this.state.bufferedDuration > 0);
    this.scheduleDraw();
  }

  /** Ports the streaming-finished branch of `markFinished`. */
  markStreamFinished(): void {
    if (this.state?.mode === "streaming") {
      this.state.finished = true;
      this.scheduleDraw();
    }
  }

  /** Ports `seekTimeFromClientX`. */
  seekTimeFromClientX(clientX: number): number {
    const w = this.state;
    if (!w) return 0;
    const rect = this.slider.getBoundingClientRect();
    const ratio = clamp((clientX - rect.left) / Math.max(1, rect.width), 0, 1);
    if (w.mode === "streaming") {
      const loaded = this.bufferedRatio();
      return loaded > 0 ? clamp(ratio / loaded, 0, 1) * w.bufferedDuration : 0;
    }
    return ratio * (w.duration || 0);
  }
}
