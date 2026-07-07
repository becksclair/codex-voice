import { useEffect, type RefObject } from "react";
import type { WaveformRef } from "./useWaveform.ts";

/**
 * Sync the `--visual-viewport-*` custom properties and the `keyboard-open`
 * class on `<html>` so the layout tracks the mobile keyboard.
 *
 * Ports `updateVisualViewportLayout` and its listeners from the legacy mount
 * effect. Writing to `document.documentElement` is a real external-system sync,
 * so it stays an effect; it schedules a waveform redraw on every layout change.
 */
export function useVisualViewport(
  textRef: RefObject<HTMLTextAreaElement | null>,
  waveformRef: WaveformRef,
): void {
  useEffect(() => {
    const update = (): void => {
      const viewport = window.visualViewport;
      const height =
        viewport?.height || window.innerHeight || document.documentElement.clientHeight;
      const offsetTop = viewport?.offsetTop || 0;
      const keyboardInset = Math.max(0, (window.innerHeight || height) - height - offsetTop);
      const root = document.documentElement;
      root.style.setProperty("--visual-viewport-height", `${height}px`);
      root.style.setProperty("--visual-viewport-offset-top", `${offsetTop}px`);
      root.classList.toggle("keyboard-open", keyboardInset > 80);
      waveformRef.current?.scheduleDraw();
    };

    update();

    const cleanups: Array<() => void> = [];
    const on = (target: EventTarget, type: string, handler: EventListener): void => {
      target.addEventListener(type, handler);
      cleanups.push(() => target.removeEventListener(type, handler));
    };

    if (window.visualViewport) {
      on(window.visualViewport, "resize", update);
      on(window.visualViewport, "scroll", update);
    }
    on(window, "resize", update);
    on(window, "orientationchange", update);

    const text = textRef.current;
    if (text) {
      on(text, "focus", update);
      on(text, "blur", () => setTimeout(update, 120));
    }

    return () => {
      for (const cleanup of cleanups) cleanup();
    };
    // Mount-once external-system sync (document.documentElement + listeners).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
