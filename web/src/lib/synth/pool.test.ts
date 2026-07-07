import { describe, expect, it } from "vitest";
import { synthesizeChunksOrdered } from "./pool.ts";

/** A promise whose resolve/reject can be triggered externally. */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
} {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("synthesizeChunksOrdered", () => {
  it("returns results in input order despite out-of-order completion", async () => {
    const chunks = ["a", "b", "c"];
    const deferreds = chunks.map(() => deferred<string>());
    const result = synthesizeChunksOrdered(chunks, 3, (_chunk, index) => deferreds[index].promise);

    // Resolve in reverse order.
    deferreds[2].resolve("C");
    deferreds[0].resolve("A");
    deferreds[1].resolve("B");

    expect(await result).toEqual(["A", "B", "C"]);
  });

  it("passes chunk and index to fn", async () => {
    const seen: Array<[string, number]> = [];
    await synthesizeChunksOrdered(["x", "y"], 2, async (chunk, index) => {
      seen.push([chunk, index]);
      return chunk;
    });
    expect(seen).toEqual([
      ["x", 0],
      ["y", 1],
    ]);
  });

  it("never exceeds the concurrency limit", async () => {
    const chunks = Array.from({ length: 10 }, (_, i) => String(i));
    let active = 0;
    let maxActive = 0;
    const deferreds = chunks.map(() => deferred<string>());
    const result = synthesizeChunksOrdered(chunks, 3, (_chunk, index) => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      return deferreds[index].promise.finally(() => {
        active -= 1;
      });
    });

    // Resolve one at a time, letting the microtask queue drain between each.
    for (let i = 0; i < chunks.length; i += 1) {
      deferreds[i].resolve(`r${i}`);
      await Promise.resolve();
      await Promise.resolve();
    }
    await result;
    expect(maxActive).toBe(3);
  });

  it("floors pool size to the chunk count", async () => {
    const chunks = ["only"];
    let active = 0;
    let maxActive = 0;
    await synthesizeChunksOrdered(chunks, 5, async () => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      await Promise.resolve();
      active -= 1;
      return "x";
    });
    expect(maxActive).toBe(1);
  });

  it("handles an empty chunk list", async () => {
    let calls = 0;
    const result = await synthesizeChunksOrdered([], 4, async () => {
      calls += 1;
      return "x";
    });
    expect(result).toEqual([]);
    expect(calls).toBe(0);
  });

  it("floors a limit of 0 to a single worker", async () => {
    let active = 0;
    let maxActive = 0;
    const chunks = ["a", "b", "c"];
    await synthesizeChunksOrdered(chunks, 0, async (chunk) => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      await Promise.resolve();
      active -= 1;
      return chunk;
    });
    expect(maxActive).toBe(1);
  });

  it("propagates the first rejection", async () => {
    const err = new Error("boom");
    await expect(
      synthesizeChunksOrdered(["a", "b"], 2, async (chunk) => {
        if (chunk === "a") throw err;
        return chunk;
      }),
    ).rejects.toBe(err);
  });
});
