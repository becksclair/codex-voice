/**
 * Vitest global setup for the lib modules.
 *
 * happy-dom does not provide `localStorage`, and no DOM env provides IndexedDB,
 * so this installs an in-memory `localStorage` and the `fake-indexeddb` polyfill
 * before any test runs. Pure browser APIs (`fetch`/`Response`, `atob`/`btoa`,
 * `AudioContext`) are provided by the runtime/happy-dom and are not touched.
 */

import "fake-indexeddb/auto";

class MemoryStorage implements Storage {
  private store = new Map<string, string>();

  get length(): number {
    return this.store.size;
  }

  clear(): void {
    this.store.clear();
  }

  getItem(key: string): string | null {
    return this.store.has(key) ? (this.store.get(key) as string) : null;
  }

  key(index: number): string | null {
    return Array.from(this.store.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.store.delete(key);
  }

  setItem(key: string, value: string): void {
    this.store.set(key, String(value));
  }
}

if (typeof globalThis.localStorage === "undefined") {
  Object.defineProperty(globalThis, "localStorage", {
    value: new MemoryStorage(),
    writable: true,
    configurable: true,
  });
}
