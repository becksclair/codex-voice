/**
 * Barrel for the speech-prep pipeline.
 *
 * Ports the prep subsystem of app.html (~lines 1848-2760) as headless,
 * testable modules. The entry point is {@link prepareForProvider}; the other
 * exports are the decision-tree, prompt, tag, and transport helpers it composes.
 */

export * from "./types.ts";
export * from "./tags.ts";
export * from "./prompts.ts";
export * from "./decision.ts";
export * from "./codex.ts";
export * from "./prepare.ts";
