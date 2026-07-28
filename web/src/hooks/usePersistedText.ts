import { useEffect, useRef, useState } from "react";
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
 * not written to storage until the next user edit. User edits persist after a
 * short debounce so large Markdown pastes do not synchronously write on every
 * editor transaction.
 */
export function usePersistedText(): [string, SetText] {
  const [text, setTextState] = useState(loadText);
  const lastAppliedValue = useRef(text);
  const saveTimer = useRef<number | null>(null);
  const pendingValue = useRef<string | null>(null);
  useEffect(
    () => () => {
      // Flush a pending debounced save on unmount so a tab close or
      // route change inside the 350 ms window does not drop the draft.
      if (saveTimer.current !== null) {
        window.clearTimeout(saveTimer.current);
        if (pendingValue.current !== null) saveText(pendingValue.current);
      }
    },
    [],
  );
  const setText: SetText = (value, options) => {
    // The Lexical editor echoes every committed value back through
    // onChange. Without this guard, a `{ persist: false }` programmatic
    // update would still trigger a debounced save when the editor
    // re-emits the same markdown, breaking the "not written to storage
    // until the next user edit" contract.
    if (value === lastAppliedValue.current) return;
    lastAppliedValue.current = value;
    setTextState(value);
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    if (options?.persist === false) pendingValue.current = null;
    if (options?.persist !== false) {
      pendingValue.current = value;
      saveTimer.current = window.setTimeout(() => {
        saveTimer.current = null;
        pendingValue.current = null;
        saveText(value);
      }, 350);
    }
  };
  return [text, setText];
}
