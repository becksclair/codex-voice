/**
 * TTS text chunking.
 *
 * Ports `splitTtsText`/`splitIndexAtOrBefore` and the chunk-size constants from
 * app.html (lines ~2825-2851). Splitting is codepoint-aware (`Array.from`) so
 * astral characters count as one, matching the legacy algorithm exactly.
 */

/**
 * Minimum input length (in codepoints) before providers attempt chunking at
 * all. Below this, callers synthesize the whole input in one request.
 * Ports `ttsChunkMinChars` (app.html line ~2825).
 */
export const TTS_CHUNK_MIN_CHARS = 1600;

/**
 * Maximum codepoints per chunk. Ports `ttsChunkMaxChars` (app.html line ~2826).
 */
export const TTS_CHUNK_MAX_CHARS = 900;

/**
 * Boundary silence inserted between stitched chunks, in milliseconds.
 * Ports `ttsChunkBoundarySilenceMs` (app.html line ~2827).
 */
export const TTS_CHUNK_BOUNDARY_SILENCE_MS = 180;

/**
 * Find the byte index at or before `maxChars` codepoints to split on.
 *
 * Ports `splitIndexAtOrBefore` (app.html line ~2842): takes the first
 * `maxChars` codepoints as a hard limit, then searches that prefix for the
 * last occurrence of a preferred boundary (`'. '`, `'! '`, `'? '`, `'\n\n'`,
 * `'\n'`, `'; '`, `', '`, `' '`, in that priority order), returning the index
 * just past the delimiter. Falls back to the hard limit when no boundary is
 * found. The returned index is a string (UTF-16) offset, as in the original.
 */
export function splitIndexAtOrBefore(input: string, maxChars: number): number {
  const chars = Array.from(input);
  const hardLimit = chars.slice(0, maxChars).join("").length;
  const prefix = input.slice(0, hardLimit);
  for (const pattern of [". ", "! ", "? ", "\n\n", "\n", "; ", ", ", " "]) {
    const index = prefix.lastIndexOf(pattern);
    if (index >= 0) return index + pattern.length;
  }
  return hardLimit;
}

/**
 * Split input text into chunks of at most `maxChars` codepoints.
 *
 * Ports `splitTtsText` (app.html line ~2829): trims the input, then repeatedly
 * carves off a leading chunk at {@link splitIndexAtOrBefore} while the
 * remaining codepoint count exceeds `maxChars`. Each chunk is trimmed; empty
 * chunks are dropped. The final remainder (`trimStart`-ed between iterations)
 * is appended if non-empty.
 */
export function splitTtsText(input: string, maxChars: number = TTS_CHUNK_MAX_CHARS): string[] {
  const chunks: string[] = [];
  let remaining = String(input || "").trim();
  while (Array.from(remaining).length > maxChars) {
    const splitAt = splitIndexAtOrBefore(remaining, maxChars);
    const head = remaining.slice(0, splitAt).trim();
    if (head) chunks.push(head);
    remaining = remaining.slice(splitAt).trimStart();
  }
  if (remaining) chunks.push(remaining);
  return chunks;
}
