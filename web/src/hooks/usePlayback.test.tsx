import { act, renderHook } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import type { StreamingPlayback } from "../lib/audio/streaming.ts";
import { usePlayback } from "./usePlayback.ts";
import type { WaveformRef } from "./useWaveform.ts";

test("the transport controls a live stream before its final blob is ready", async () => {
  const waveformRef = { current: null } as WaveformRef;
  const toggle = vi.fn(async () => {});
  const stream = {
    playing: true,
    toggle,
    stop: vi.fn(),
  } as unknown as StreamingPlayback;
  const { result } = renderHook(() => usePlayback(waveformRef, vi.fn(), vi.fn()));

  act(() => result.current.api.onStreamPlaybackChange(stream));
  expect(result.current.playDisabled).toBe(false);
  expect(result.current.downloadDisabled).toBe(true);
  expect(result.current.paused).toBe(false);

  await act(() => result.current.api.togglePlay());
  expect(toggle).toHaveBeenCalledOnce();
});
