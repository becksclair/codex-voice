/** Presentation-only formatting helpers shared by the shell components/hooks. */

/** Format seconds as `m:ss`. Ports `formatTime` (app.html line ~1037). */
export function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return "0:00";
  const whole = Math.floor(seconds);
  const minutes = Math.floor(whole / 60);
  return `${minutes}:${String(whole % 60).padStart(2, "0")}`;
}

/** Download-file extension from a blob's mime type. Ports `audioDownloadExtension` (app.html line ~1381). */
export function audioDownloadExtension(blob: Blob | null): string {
  const type = String(blob?.type || "").toLowerCase();
  if (type.includes("mpeg") || type.includes("mp3")) return "mp3";
  if (type.includes("opus")) return "opus";
  if (type.includes("ogg")) return "ogg";
  if (type.includes("wav") || type.includes("pcm")) return "wav";
  return "wav";
}
