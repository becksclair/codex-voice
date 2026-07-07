#!/usr/bin/env node
/**
 * Initial-load JS budget guard.
 *
 * Measures the gzipped size of the JavaScript the browser must download and
 * execute for first paint of the app shell: the module entry `<script>` plus
 * any `<link rel="modulepreload">` chunks referenced by `dist/index.html`. The
 * lazily `import()`-ed generation pipeline chunk is deliberately excluded — it
 * is fetched on the first generate, not at load.
 *
 * Fails the build (exit 1) when the budget is exceeded. The budget is set from
 * the measured Phase-E baseline (72,678 bytes gzip after code-splitting the
 * generation pipeline out of the entry) plus ~10% headroom, rounded.
 *
 * Run indirectly via `bun run build`, or directly with `node
 * scripts/check-bundle-size.mjs` after a build.
 */
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** Gzip budget in bytes for initial-load JS (entry + modulepreloaded chunks). */
const BUDGET_BYTES = 80_000;

const here = dirname(fileURLToPath(import.meta.url));
const distDir = resolve(here, "..", "dist");
const indexPath = join(distDir, "index.html");

let html;
try {
  html = readFileSync(indexPath, "utf8");
} catch {
  console.error(`bundle-size: ${indexPath} not found — run \`vite build\` first.`);
  process.exit(1);
}

/** Resolve a `/web/assets/...` URL to its on-disk path under dist/. */
function assetPath(url) {
  const stripped = url.replace(/^\/web\//, "").replace(/^\//, "");
  return join(distDir, stripped);
}

// The module entry script and any statically preloaded chunks are the initial
// JS payload. Dynamic imports (the generation chunk) are not referenced here.
const urls = new Set();
for (const m of html.matchAll(/<script[^>]+type="module"[^>]+src="([^"]+)"/g)) urls.add(m[1]);
for (const m of html.matchAll(/<link[^>]+rel="modulepreload"[^>]+href="([^"]+)"/g)) urls.add(m[1]);

if (urls.size === 0) {
  console.error("bundle-size: no entry module script found in index.html.");
  process.exit(1);
}

let total = 0;
const rows = [];
for (const url of urls) {
  if (!url.endsWith(".js")) continue;
  const bytes = readFileSync(assetPath(url));
  const gz = gzipSync(bytes, { level: 9 }).length;
  total += gz;
  rows.push({ url, gz });
}

rows.sort((a, b) => b.gz - a.gz);
const fmt = (n) => `${(n / 1000).toFixed(2)} kB`;
console.log("Initial-load JS (gzip):");
for (const { url, gz } of rows) console.log(`  ${fmt(gz).padStart(10)}  ${url}`);
console.log(`  ${fmt(total).padStart(10)}  TOTAL  (budget ${fmt(BUDGET_BYTES)})`);

if (total > BUDGET_BYTES) {
  const over = total - BUDGET_BYTES;
  console.error(
    `\nbundle-size: initial-load JS ${total} B exceeds budget ${BUDGET_BYTES} B by ${over} B.`,
  );
  console.error(
    "Split heavy code behind a dynamic import() at a feature boundary, or raise the",
    "budget in scripts/check-bundle-size.mjs if the growth is justified and recorded.",
  );
  process.exit(1);
}

const headroom = BUDGET_BYTES - total;
console.log(
  `\nbundle-size: OK — ${headroom} B (${((headroom / BUDGET_BYTES) * 100).toFixed(1)}%) under budget.`,
);
