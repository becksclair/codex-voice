import { describe, expect, it } from "vitest";
import {
  DARK_THEME_COLOR,
  LIGHT_THEME_COLOR,
  applyTheme,
  manifestDatasetKey,
  resolveTheme,
  themeApplication,
  themeColorFor,
} from "./theme.ts";

describe("resolveTheme", () => {
  it("passes explicit preferences through", () => {
    expect(resolveTheme("light", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", true)).toBe("dark");
    expect(resolveTheme("dark", false)).toBe("dark");
  });

  it("resolves auto from prefers-color-scheme: light", () => {
    expect(resolveTheme("auto", true)).toBe("light");
    expect(resolveTheme("auto", false)).toBe("dark");
  });
});

describe("theme colors and manifest", () => {
  it("maps resolved themes to hex colors", () => {
    expect(themeColorFor("light")).toBe(LIGHT_THEME_COLOR);
    expect(themeColorFor("dark")).toBe(DARK_THEME_COLOR);
    expect(LIGHT_THEME_COLOR).toBe("#f3dff1");
    expect(DARK_THEME_COLOR).toBe("#17091f");
  });

  it("chooses the manifest dataset key", () => {
    expect(manifestDatasetKey("light")).toBe("manifestLight");
    expect(manifestDatasetKey("dark")).toBe("manifestDark");
  });
});

describe("themeApplication", () => {
  it("bundles the full application for auto+light", () => {
    expect(themeApplication("auto", true)).toEqual({
      resolved: "light",
      datasetTheme: "light",
      themeColor: LIGHT_THEME_COLOR,
      manifestKey: "manifestLight",
    });
  });
});

describe("applyTheme", () => {
  it("mutates dataset, meta theme-color, and manifest href", () => {
    document.head.innerHTML = `
      <meta name="theme-color" content="#000000" />
      <link rel="manifest" href="/manifest.webmanifest"
        data-manifest-light="/manifest-light.webmanifest"
        data-manifest-dark="/manifest.webmanifest" />
    `;
    const resolved = applyTheme(document, "light", false);
    expect(resolved).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(document.querySelector('meta[name="theme-color"]')?.getAttribute("content")).toBe(
      LIGHT_THEME_COLOR,
    );
    expect(document.querySelector<HTMLLinkElement>('link[rel="manifest"]')?.href).toContain(
      "manifest-light.webmanifest",
    );
  });
});
