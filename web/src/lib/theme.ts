/**
 * Theme resolution and the pure "what to apply" helpers.
 *
 * Ports `resolvedTheme`/`applyThemeSetting` and the theme-color constants from
 * app.html (lines ~782-783, 939-956). The DOM-mutating `applyTheme` helper is
 * intentionally minimal so B2 can call it directly.
 */

import type { ThemePreference } from "./settings.ts";

export type { ThemePreference };

/** Resolved (concrete) theme after collapsing `auto`. */
export type ResolvedTheme = "light" | "dark";

/** `<meta name="theme-color">` value for dark mode. */
export const DARK_THEME_COLOR = "#17091f";
/** `<meta name="theme-color">` value for light mode. */
export const LIGHT_THEME_COLOR = "#f3dff1";

/**
 * Resolve a theme preference to a concrete theme.
 *
 * Ports `resolvedTheme` (app.html line ~939): explicit `light`/`dark` pass
 * through; `auto` (or any other value) resolves to `light` when the
 * `(prefers-color-scheme: light)` media query matches, else `dark`.
 */
export function resolveTheme(preference: ThemePreference, prefersLight: boolean): ResolvedTheme {
  if (preference === "dark" || preference === "light") return preference;
  return prefersLight ? "light" : "dark";
}

/** The `theme-color` hex for a resolved theme. */
export function themeColorFor(resolved: ResolvedTheme): string {
  return resolved === "light" ? LIGHT_THEME_COLOR : DARK_THEME_COLOR;
}

/**
 * Which manifest dataset key to prefer for a resolved theme.
 *
 * Ports the manifest selection in `applyThemeSetting` (app.html line ~950):
 * light mode prefers `link.dataset.manifestLight`, dark prefers
 * `link.dataset.manifestDark`, each falling back to the current `href`.
 */
export function manifestDatasetKey(resolved: ResolvedTheme): "manifestLight" | "manifestDark" {
  return resolved === "light" ? "manifestLight" : "manifestDark";
}

/** The pure description of everything a theme change should apply to the DOM. */
export interface ThemeApplication {
  resolved: ResolvedTheme;
  datasetTheme: ResolvedTheme;
  themeColor: string;
  manifestKey: "manifestLight" | "manifestDark";
}

/**
 * Compute the full set of theme values to apply, without touching the DOM.
 *
 * Bundles {@link resolveTheme}, {@link themeColorFor}, and
 * {@link manifestDatasetKey} so callers/tests can assert the outcome directly.
 */
export function themeApplication(
  preference: ThemePreference,
  prefersLight: boolean,
): ThemeApplication {
  const resolved = resolveTheme(preference, prefersLight);
  return {
    resolved,
    datasetTheme: resolved,
    themeColor: themeColorFor(resolved),
    manifestKey: manifestDatasetKey(resolved),
  };
}

/**
 * Apply a resolved theme to a `Document`.
 *
 * Ports the DOM side-effects of `applyThemeSetting` (app.html line ~944): sets
 * `documentElement.dataset.theme`, updates the `theme-color` meta tag, and
 * swaps the manifest `href` to the theme-specific one when the dataset provides
 * it. `prefersLight` should reflect the current
 * `matchMedia('(prefers-color-scheme: light)').matches`.
 */
export function applyTheme(
  doc: Document,
  preference: ThemePreference,
  prefersLight: boolean,
): ResolvedTheme {
  const { resolved, themeColor, manifestKey } = themeApplication(preference, prefersLight);
  doc.documentElement.dataset.theme = resolved;
  doc.querySelector('meta[name="theme-color"]')?.setAttribute("content", themeColor);
  const manifest = doc.querySelector<HTMLLinkElement>('link[rel="manifest"]');
  if (manifest) {
    manifest.href = manifest.dataset[manifestKey] || manifest.href;
  }
  return resolved;
}
