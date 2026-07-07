# Plan 015: Fix stale documentation — dead execplan pointers, "placeholder" ui crate, undocumented PWA, missing TTS config example

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- AGENTS.md README.md ROADMAP.md crates/*/AGENTS.md`
> On drift, re-run the locator greps below; the fixes apply wherever the
> stale text now lives.

## Status

- **Priority**: P2 (cheap, and it actively misdirects both agents and the maintainer today)
- **Effort**: S
- **Risk**: LOW (docs only — but factual accuracy matters; every claim you write must be verified against code)
- **Depends on**: none
- **Category**: docs
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

Four documentation defects actively mislead: (1) six pointers direct readers — and the "Definition of Done" directs *editors* — to `docs/execplan-rust-native-cross-platform.md`, which does not exist (only `.ARCHIVED` remains; `ROADMAP.md:5` explicitly superseded it). (2) The root index calls `codex-voice-ui` a "UI placeholder" and its crate AGENTS.md frames Slint as the future — but the crate ships 1,232 lines of live tray code for three platforms and ROADMAP Phase 6 records "Slint ... was never adopted". (3) The embedded web PWA — the most actively developed surface (last 10 commits) — has zero README presence. (4) TTS setup is gated on `~/.codex/read-aloud-defaults.json` whose structure exists only as prose; users must reverse-engineer serde test fixtures to write one.

## Current state

Locator greps (run each; fix every hit):

```bash
grep -rn "execplan-rust-native-cross-platform" --include="*.md" . | grep -v ARCHIVED | grep -v plans/
# Expected hits at planning: AGENTS.md:33, AGENTS.md:60,
# crates/codex-voice-app/AGENTS.md:35, crates/codex-voice-codex/AGENTS.md:36,
# crates/codex-voice-platform/AGENTS.md:42, crates/codex-voice-ui/AGENTS.md:31 and :37

grep -n "UI placeholder" AGENTS.md            # AGENTS.md:59
grep -n "Slint" crates/codex-voice-ui/AGENTS.md
grep -n "/web" README.md                       # no PWA section at planning
grep -n "read-aloud-defaults" README.md AGENTS.md
```

Facts to write from (verified during the audit — re-verify anything you state):
- `ROADMAP.md:5`: "This roadmap replaces `@docs/execplan-rust-native-cross-platform.md` as the canonical plan of record."
- `ROADMAP.md:150-159` (Phase 6): decision to keep per-platform native UI; Slint never adopted.
- ui crate contents: `linux_tray.rs` (451 lines), `macos_tray.rs` (319), `windows_tray.rs` (462) — live tray/HUD for all three platforms, imported by the app at `crates/codex-voice-app/src/main.rs:20-25`.
- PWA surface (grep the route registrations in the transcriber server, `fn service_router`): `GET /web` (installable TTS web app), `GET /web/config`, `/web/manifest.webmanifest`, `/web/manifest-light.webmanifest`, `/web-sw.js`, icon routes, `POST /web/speech`, `POST /web/speech-jobs`, `GET /web/speech-jobs/{id}`. Features per recent commits: paste-to-speech, touch waveform playback, chunked stitching, install-to-homescreen.
- `read-aloud-defaults.json` structure: derive the example from the config loader — `crates/codex-voice-tts/src/config/mod.rs` (struct `ReadAloudDefaultsFile` and its serde tests, e.g. fixtures around lines 338/374) and `crates/codex-voice-tts/src/config/provider.rs`. The README prose at 117-128 describes personas/providers/`messages.tts.speechPrep`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Locators | the greps above | zero stale hits after the fix |
| Example validity | `cargo test -p codex-voice-tts config` | config tests still pass (they don't read your example, but if you add a parse test per Step 4, it does) |
| Full gates | the four AGENTS.md commands | exit 0 |

## Scope

**In scope**:
- `AGENTS.md` (root), `crates/{codex-voice-app,codex-voice-codex,codex-voice-platform,codex-voice-ui}/AGENTS.md`
- `README.md`
- `docs/read-aloud-defaults.example.json` (create)
- Optionally one parse test in `crates/codex-voice-tts/src/config/mod.rs` (Step 4)

**Out of scope** (do NOT touch):
- `ROADMAP.md` (already correct), `docs/*.ARCHIVED` (leave archived).
- Any Rust behavior. No persona/style leakage: repo docs are professional prose.
- Secrets: the example JSON uses obvious placeholders (`"YOUR_GOOGLE_API_KEY"`), never real values.

## Git workflow

- Branch: `advisor/015-docs-accuracy`
- One commit, e.g. `Fix stale doc pointers; document web PWA and TTS config example`.

## Steps

### Step 1: Repoint the six execplan references

- `AGENTS.md:33` ("Update `README.md` and `docs/execplan...` when command contracts change") → "Update `README.md` and `ROADMAP.md` when command contracts change."
- `AGENTS.md:60` ("Architecture plan: ...") → point to `ROADMAP.md`, optionally noting the archived execplan for history.
- The four crate-level AGENTS.md references: same treatment — `ROADMAP.md` for plan-of-record, `.ARCHIVED` suffix only where the historical document is genuinely meant.
- Also `AGENTS.md:81` (Definition of Done "Update README/ExecPlan...") — reword to README/ROADMAP.

**Verify**: the first locator grep → zero hits.

### Step 2: Correct the ui crate's description

- `AGENTS.md:59`: replace "UI placeholder" with e.g. "Native tray/HUD/settings surfaces for Linux, macOS, and Windows".
- `crates/codex-voice-ui/AGENTS.md`: rewrite the Slint-future framing: the crate ships per-platform native trays; per ROADMAP Phase 6, Slint was evaluated and not adopted. Remove instructions that tell agents to prepare Slint wrappers.

**Verify**: `grep -n "placeholder" AGENTS.md` → no hit for the ui line; `grep -n "Slint" crates/codex-voice-ui/AGENTS.md` → remaining mentions only in a "not adopted" context.

### Step 3: Add a "Web App" section to README.md

After the "Local Audio Server" section. Content (verify each claim against `service_router` before writing):
- `GET /web` serves an installable TTS web app (PWA) from the same service; reachable wherever the server is bound (localhost or the Tailscale address configured by `mise run setup`).
- What it does: paste or type text, generate speech (with generate-on-paste), waveform/touch playback, async speech jobs; installable to a phone homescreen via the manifest + service worker.
- Note that `/web/config` and `/web/speech*` are deliberately unauthenticated for the PWA's use — private-network deployment is the trust boundary (cite the existing "Deployment Context" section of AGENTS.md).

**Verify**: `grep -n "/web" README.md` → the new section exists; every endpoint named in it appears in `service_router` (cross-check by grep).

### Step 4: Ship a TTS config example

Create `docs/read-aloud-defaults.example.json` with one persona, both providers (placeholder keys), and a `speechPrep` block — derived from the serde structures in `crates/codex-voice-tts/src/config/mod.rs`, NOT invented. Link it from README's TTS section ("copy to `~/.codex/read-aloud-defaults.json` and fill in keys").

Then make it self-verifying: add one test in `config/mod.rs`:

```rust
#[test]
fn example_config_in_docs_parses() {
    let text = include_str!("../../../../docs/read-aloud-defaults.example.json");
    // parse with the same entry point the loader uses; assert Ok
}
```

(Adjust the relative path and the parse entry point to the real loader fn — grep `fn load` / `from_str` in `config/mod.rs`.) This test pins the example against schema drift forever.

**Verify**: `cargo test -p codex-voice-tts config` → passes including the new test.

### Step 5: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

The single `example_config_in_docs_parses` test (Step 4). Everything else is prose verified by the locator greps.

## Done criteria

- [ ] Zero live references to the non-ARCHIVED execplan path outside `plans/`
- [ ] ui crate described accurately in both AGENTS.md files
- [ ] README has a Web App section whose every endpoint exists in `service_router`
- [ ] `docs/read-aloud-defaults.example.json` exists, contains no real secrets, and is parse-tested
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- The config loader's structure resists a minimal valid example (deeply conditional requirements) — report what a minimal config actually requires rather than shipping an example that 503s.
- You cannot verify a README claim against code — omit the claim and note it.

## Maintenance notes

- The parse test makes the example self-maintaining: schema changes break CI (once plan 001 lands) until the example is updated.
- If the PWA later gains transcription features, the README Web App section is the place to extend.
- Reviewer should scrutinize: that no example value looks like a real credential, and that the AGENTS.md rewording didn't weaken the cache-busting or secrets rules that live in adjacent lines.
