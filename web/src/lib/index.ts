/**
 * Barrel for the ported PWA logic modules.
 *
 * B2 (the React shell) builds against these exports. Prefer importing from the
 * specific module (e.g. `./lib/synth/pool.ts`) when only one is needed; this
 * barrel is for convenience.
 */

export * from "./util.ts";
export * from "./appMode.ts";
export * from "./storage.ts";
export * from "./settings.ts";
export * from "./config.ts";
export * from "./theme.ts";
export * from "./audio/wav.ts";
export * from "./audio/pcm.ts";
export * from "./audio/waveform.ts";
export * from "./personas.ts";
// NOTE: the generation pipeline (`./generation.ts`, `./audio/streaming.ts`,
// `./synth/*`, `./prep/*`) is intentionally NOT re-exported here. It is loaded
// lazily via dynamic `import()` in `useGeneration`; re-exporting any of it from
// this shell-wide barrel would make the pipeline statically reachable from the
// entry chunk and undo the code split (the 80 kB budget only catches a full
// re-inline, not a partial one). Import those modules by path: tests and
// `generation.ts` already do, and type-only imports are safe anywhere.
