import { useRef, type MutableRefObject } from "react";

/**
 * Keeps a ref pointing at the latest `value` on every render. Lets an effect
 * that must mount once (e.g. an event listener bound with `[]`) still call the
 * newest closure without re-subscribing. The ref is written during render,
 * which is safe for this read-latest use: nothing reads it during the same
 * render, only later from event/timer callbacks.
 */
export function useLatest<T>(value: T): MutableRefObject<T> {
  const ref = useRef(value);
  ref.current = value;
  return ref;
}
