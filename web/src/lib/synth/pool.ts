/**
 * Ordered, bounded-concurrency worker pool.
 *
 * Ports `synthesizeChunksOrdered` from app.html (line ~3291) verbatim in
 * behavior.
 */

/**
 * Synthesize chunks with bounded concurrency while preserving input order.
 *
 * `fn` is invoked as `fn(chunk, index)`; results are returned in the same order
 * as `chunks`. A shared `nextIndex` counter hands work to `poolSize` workers,
 * where `poolSize = max(1, min(limit, chunks.length))`. Fails fast: the first
 * rejection propagates immediately (via `Promise.all`), though already-started
 * `fn` calls are not cancelled by this function itself.
 *
 * Edge cases (preserved from the original): an empty `chunks` array still
 * spawns one worker, which immediately exits and yields `[]`; a `limit` of 0 or
 * negative is floored to a single worker.
 */
export async function synthesizeChunksOrdered<T>(
  chunks: readonly string[],
  limit: number,
  fn: (chunk: string, index: number) => Promise<T>,
): Promise<T[]> {
  const results = new Array<T>(chunks.length);
  let nextIndex = 0;
  const worker = async (): Promise<void> => {
    while (true) {
      const index = nextIndex++;
      if (index >= chunks.length) return;
      results[index] = await fn(chunks[index], index);
    }
  };
  const poolSize = Math.max(1, Math.min(limit, chunks.length));
  await Promise.all(Array.from({ length: poolSize }, () => worker()));
  return results;
}
