import { afterEach, describe, expect, it, vi } from "vitest";
import type { BrowserPersonaConfig, BrowserTtsConfig } from "../config.ts";
import { wavPcmData } from "./wav.ts";
import type { AudioContextConstructor, StreamState } from "./streaming.ts";
import { createPcmStreamSink, StreamingPlayback, streamElevenLabsHttp } from "./streaming.ts";

/** A minimal scripted AudioContext for scheduling/accounting tests. */
class FakeAudioBuffer {
  duration: number;
  private data: Float32Array[];
  constructor(
    public numberOfChannels: number,
    public length: number,
    public sampleRate: number,
  ) {
    this.duration = length / sampleRate;
    this.data = Array.from({ length: numberOfChannels }, () => new Float32Array(length));
  }
  getChannelData(channel: number): Float32Array {
    return this.data[channel];
  }
}

class FakeBufferSource {
  buffer: FakeAudioBuffer | null = null;
  onended: (() => void) | null = null;
  started: { when: number; offset: number } | null = null;
  connect(): void {}
  start(when = 0, offset = 0): void {
    this.started = { when, offset };
  }
}

class FakeAudioContext {
  currentTime = 0;
  state: "running" | "suspended" | "closed" = "running";
  destination = {};
  sources: FakeBufferSource[] = [];
  createBuffer(channels: number, length: number, sampleRate: number): FakeAudioBuffer {
    return new FakeAudioBuffer(channels, length, sampleRate);
  }
  createBufferSource(): FakeBufferSource {
    const source = new FakeBufferSource();
    this.sources.push(source);
    return source;
  }
  async resume(): Promise<void> {
    this.state = "running";
  }
  async suspend(): Promise<void> {
    this.state = "suspended";
  }
  async close(): Promise<void> {
    this.state = "closed";
  }
}

const fakeCtor = (): AudioContextConstructor =>
  FakeAudioContext as unknown as AudioContextConstructor;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("createPcmStreamSink — PCM accounting", () => {
  it("carries an odd trailing byte across chunks and stitches all even bytes", () => {
    const sink = createPcmStreamSink({ gain: 1, audioContextCtor: fakeCtor });
    void sink.start();
    sink.onAudioChunk(new Uint8Array([1, 2, 3]), { sampleRate: 24000 }); // emits [1,2], carries 3
    sink.onAudioChunk(new Uint8Array([4, 5]), { sampleRate: 24000 }); // 3+[4,5] -> [3,4], carries 5
    const blob = sink.finish();
    return blob.arrayBuffer().then((buf) => {
      const pcm = wavPcmData(new Uint8Array(buf));
      expect(Array.from(pcm.data)).toEqual([1, 2, 3, 4]);
    });
  });

  it("applies gain to the emitted samples", async () => {
    const sink = createPcmStreamSink({ gain: 2, audioContextCtor: fakeCtor });
    void sink.start();
    // one little-endian sample of value 100 -> *2 -> 200
    sink.onAudioChunk(new Uint8Array([100, 0]), { sampleRate: 24000 });
    const pcm = wavPcmData(new Uint8Array(await sink.finish().arrayBuffer()));
    expect(Array.from(pcm.data)).toEqual([200, 0]);
  });

  it("throws when no audio was received", () => {
    const sink = createPcmStreamSink({ gain: 1, audioContextCtor: fakeCtor });
    void sink.start();
    expect(() => sink.finish()).toThrow("Streaming TTS did not return audio.");
  });

  it("emits buffering -> playing -> done state transitions", async () => {
    const states: StreamState[] = [];
    const sink = createPcmStreamSink({
      gain: 1,
      audioContextCtor: fakeCtor,
      callbacks: { onStateChange: (s) => states.push(s) },
    });
    await sink.start();
    sink.onAudioChunk(new Uint8Array([1, 2]), { sampleRate: 24000 });
    sink.finish();
    expect(states).toEqual(["buffering", "playing", "done"]);
  });
});

describe("StreamingPlayback — scheduling math", () => {
  it("advances estimated duration and schedules one source per chunk", async () => {
    const peaks: number[] = [];
    const playback = new StreamingPlayback({
      audioContextCtor: fakeCtor,
      callbacks: { onPeaks: (p) => peaks.push(...p) },
    });
    await playback.start();
    const ctx = playback.context as unknown as FakeAudioContext;
    // 4 bytes @ 1ch = 2 frames @ 24000 Hz
    playback.appendPcm(new Uint8Array([0, 0, 0, 0]), 24000, 1);
    playback.appendPcm(new Uint8Array([0, 0, 0, 0]), 24000, 1);
    expect(ctx.sources.length).toBe(2);
    expect(playback.estimatedDuration).toBeCloseTo((2 / 24000) * 2, 10);
    // second source starts no earlier than the first chunk's end (nextStartTime lead).
    expect(ctx.sources[1].started?.when).toBeGreaterThanOrEqual(ctx.sources[0].started?.when ?? 0);
  });

  it("stop() halts further scheduling and closes the context", async () => {
    const playback = new StreamingPlayback({ audioContextCtor: fakeCtor });
    await playback.start();
    const ctx = playback.context as unknown as FakeAudioContext;
    playback.stop();
    expect(playback.stopped).toBe(true);
    expect(ctx.state).toBe("closed");
    playback.appendPcm(new Uint8Array([1, 2]), 24000, 1);
    expect(ctx.sources.length).toBe(0);
  });

  it("delivers a drained replay blob once finished and sources ended", async () => {
    let replay: Blob | null = null;
    const playback = new StreamingPlayback({
      audioContextCtor: fakeCtor,
      callbacks: { onReplayReady: (b) => (replay = b) },
    });
    await playback.start();
    playback.appendPcm(new Uint8Array([0, 0]), 24000, 1);
    const ctx = playback.context as unknown as FakeAudioContext;
    const blob = new Blob(["x"], { type: "audio/wav" });
    playback.setReplayBlob(blob);
    playback.markFinished();
    // Fire the scheduled source's onended so pendingSources drains to 0.
    ctx.sources[0].onended?.();
    expect(replay).toBe(blob);
    expect(playback.stopped).toBe(true);
  });
});

describe("streamElevenLabsHttp", () => {
  const config = {
    providers: {
      elevenlabs: {
        apiKey: "xi",
        baseUrl: "https://api.elevenlabs.io",
        streamGain: 1,
        applyTextNormalization: "off",
      },
    },
  } as unknown as BrowserTtsConfig;
  const persona = {
    elevenlabs: { voiceId: "voice-1", voiceSettings: { speed: 1.0 } },
  } as unknown as BrowserPersonaConfig;

  it("reads the PCM stream into a WAV blob", async () => {
    const pcm = new Uint8Array([10, 0, 20, 0, 30, 0, 40, 0]);
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(pcm, { status: 200 })),
    );
    const states: StreamState[] = [];
    const result = await streamElevenLabsHttp(
      config,
      "hello",
      persona,
      { outputFormat: "pcm_24000", sampleRate: 24000, channels: 1, modelId: "eleven_flash_v2" },
      { audioContextCtor: fakeCtor, onStateChange: (s) => states.push(s) },
    );
    const parsed = wavPcmData(new Uint8Array(await result.blob.arrayBuffer()));
    expect(Array.from(parsed.data)).toEqual([10, 0, 20, 0, 30, 0, 40, 0]);
    expect(states).toContain("done");
    expect(result.streamingModel).toBe("eleven_flash_v2");
  });

  it("fails and surfaces a provider error on a non-OK response", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("bad key", { status: 401 })),
    );
    const states: StreamState[] = [];
    await expect(
      streamElevenLabsHttp(
        config,
        "hello",
        persona,
        { outputFormat: "pcm_24000", sampleRate: 24000, channels: 1, modelId: "eleven_flash_v2" },
        { audioContextCtor: fakeCtor, onStateChange: (s) => states.push(s) },
      ),
    ).rejects.toThrow("ElevenLabs streaming TTS failed");
    expect(states).toContain("error");
  });

  it("aborts mid-stream when cancellation throws", async () => {
    const pcm = new Uint8Array([1, 0, 2, 0]);
    const fetchMock = vi.fn(async () => new Response(pcm, { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    let calls = 0;
    await expect(
      streamElevenLabsHttp(
        config,
        "hello",
        persona,
        { outputFormat: "pcm_24000", sampleRate: 24000, channels: 1, modelId: "eleven_flash_v2" },
        {
          audioContextCtor: fakeCtor,
          throwIfCancelled: () => {
            calls += 1;
            if (calls > 1) throw new Error("cancelled-midflight");
          },
        },
      ),
    ).rejects.toThrow("cancelled-midflight");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
