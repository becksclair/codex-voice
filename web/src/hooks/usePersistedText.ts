import { useState } from "react";
import { loadText, saveText } from "../lib/index.ts";

/** Options for {@link setText}. */
interface SetTextOptions {
  /** Whether to persist the new value to localStorage (defaults to `true`). */
  persist?: boolean;
}

/** A setter that updates the draft text and optionally persists it. */
export type SetText = (value: string, options?: SetTextOptions) => void;

/**
 * Owns the draft text state, seeded from localStorage.
 *
 * Ports the `text.value = loadText()` seed and the `saveText`/input wiring from
 * the legacy mount effect. The returned setter persists by default; the
 * programmatic text-replace path (`onTextReplace`) opts out with
 * `{ persist: false }`, mirroring the legacy behavior where a replaced prompt is
 * not written to storage until the next user edit.
 */
export function usePersistedText(): [string, SetText] {
  const [text, setTextState] = useState(loadText);
  const setText: SetText = (value, options) => {
    setTextState(value);
    if (options?.persist !== false) saveText(value);
  };
  return [text, setText];
}
