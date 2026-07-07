import { useEffect, useRef, type RefObject } from "react";
import { WaveformController } from "../waveform-controller.ts";

/** A live-updated ref to the {@link WaveformController} (null until mounted). */
export type WaveformRef = RefObject<WaveformController | null>;

/**
 * Owns the imperative {@link WaveformController} bound to the canvas + slider.
 *
 * The controller is created once the DOM nodes exist and torn down on unmount.
 * Canvas drawing and the slider aria/progress state stay imperative (they are a
 * true external system); the rest of the app reads the controller through the
 * returned ref.
 */
export function useWaveform(
  canvasRef: RefObject<HTMLCanvasElement | null>,
  sliderRef: RefObject<HTMLElement | null>,
): WaveformRef {
  const controllerRef = useRef<WaveformController | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const slider = sliderRef.current;
    if (!canvas || !slider) return;
    const controller = new WaveformController(canvas, slider);
    controllerRef.current = controller;
    controller.reset();
    // The canvas palette comes from `--waveform-*` CSS variables read at draw
    // time, so a theme change (settings or OS auto) needs an explicit redraw —
    // an idle waveform otherwise keeps the previous theme's colors until the
    // next incidental draw. `applyTheme` always stamps `data-theme` on <html>.
    const themeObserver = new MutationObserver(() => controller.scheduleDraw());
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => {
      themeObserver.disconnect();
      controllerRef.current = null;
    };
  }, [canvasRef, sliderRef]);

  return controllerRef;
}
