# Plan 011: Introduce a TtsProvider trait and collapse the ProviderKind dispatch

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-tts/src`
> Plans 009/010 touch the same file; adapt around their changes. If the
> dispatch structure itself no longer matches "Current state", STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED (hot synthesis path + provider fallback logic)
- **Depends on**: plans/009-concurrent-tts-synthesis.md (soft — same region; land 009 first to avoid rebase pain)
- **Category**: tech-debt
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

`ConfiguredSpeechClient` holds `Option<GoogleSpeechClient>` and `Option<ElevenLabsSpeechClient>` as concrete fields and dispatches through 12 `ProviderKind::Google | ProviderKind::ElevenLabs` match arms spread across the file. The two providers already expose a structurally identical surface (`supports_inline_audio_tags`, `resolved_model_id`, `max_text_length`, `synthesize`) but share no trait. Adding a third provider today means editing ~6 dispatch sites in lockstep; missing one compiles fine and silently misroutes. A trait turns that into one impl block plus registration.

## Current state

- `crates/codex-voice-tts/src/client.rs`:

```rust
// client.rs:20-25
pub struct ConfiguredSpeechClient {
    config: ResolvedTtsConfig,
    speech_prep: Option<SpeechPrepClient>,
    google: Option<GoogleSpeechClient>,
    elevenlabs: Option<ElevenLabsSpeechClient>,
}
```

- Dispatch sites (grep `ProviderKind::` in client.rs): paired arms at lines 112/116, 129/134, 161/166, 352/359 — plus two sites that are NOT dispatch and must stay as matches: 404/405 (`ElevenLabs => SpeechFormat::Pcm, Google => request.format` — a per-provider format *policy*) and 548/549 (fallback provider swap `Google => ElevenLabs, ElevenLabs => Google`).
- Provider surfaces (verified):
  - `crates/codex-voice-tts/src/google.rs`: `new(GoogleRuntimeConfig) -> Result<Self, SpeechError>` (15), `supports_inline_audio_tags(&self, &SpeechRequest) -> bool` (22), `resolved_model_id<'a>(&'a self, &'a SpeechRequest) -> &'a str` (29), `max_text_length(&self) -> usize` (41), `async synthesize(...)` (45).
  - `crates/codex-voice-tts/src/elevenlabs.rs`: same names, but `resolved_model_id(&self, &SpeechRequest) -> SpeechResult<String>` (31) — **signatures differ**; the trait must unify on the fallible owned form: `fn resolved_model_id(&self, request: &SpeechRequest) -> SpeechResult<String>` (Google's impl wraps its `&str` in `Ok(_.to_string())`).
  - Read both `synthesize` signatures fully before defining the trait — the trait's `synthesize` must match their common shape (params and return type), async via `async_trait` (workspace dep, already used in core — see `crates/codex-voice-core/src/speech.rs:89` for the existing `SpeechClient` trait as the style exemplar).
- The aggregate `ConfiguredSpeechClient` implements core's `SpeechClient` at client.rs:518 — that outer trait stays untouched.
- Conventions: typed errors (`SpeechError`/`SpeechResult`), `async_trait` for async traits.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check -p codex-voice-tts` | exit 0 |
| Tests | `cargo test -p codex-voice-tts` | all pass |
| Lint | `cargo clippy -p codex-voice-tts --all-targets -- -D warnings` | exit 0 |
| Full workspace | `cargo test --workspace` | all pass |

## Scope

**In scope**:
- `crates/codex-voice-tts/src/client.rs`
- `crates/codex-voice-tts/src/google.rs`, `elevenlabs.rs` (trait impls only — no behavior edits)
- A new `crates/codex-voice-tts/src/provider.rs` for the trait definition (declare in the crate's module tree; check `lib.rs`/`mod` declarations)

**Out of scope** (do NOT touch):
- The fallback *policy* (which provider is tried next, `client.rs:548-549`) and the format policy (404/405) — they stay as explicit `ProviderKind` matches; they are decisions, not dispatch.
- `speech_prep.rs`, `codex_llm.rs`, `config/`, `convert.rs`.
- Core's `SpeechClient` trait and the server crate.

## Git workflow

- Branch: `advisor/011-tts-provider-trait`
- Commits: (1) trait + impls, (2) dispatch collapse.

## Steps

### Step 1: Define the trait and implement it for both providers

In `provider.rs`:

```rust
#[async_trait::async_trait]
pub(crate) trait TtsProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn supports_inline_audio_tags(&self, request: &SpeechRequest) -> bool;
    fn resolved_model_id(&self, request: &SpeechRequest) -> SpeechResult<String>;
    fn max_text_length(&self) -> usize;
    async fn synthesize(&self /* , mirror the concrete providers' exact params */) -> /* their common return type */;
}
```

Fill the `synthesize` signature from the concrete methods (read both first; if their parameter lists differ, STOP — see STOP conditions). Implement the trait for `GoogleSpeechClient` and `ElevenLabsSpeechClient` by delegating to the existing inherent methods (Google's `resolved_model_id` wraps in `Ok(...to_string())`). Do not delete the inherent methods in this step.

**Verify**: `cargo check -p codex-voice-tts` → exit 0.

### Step 2: Store providers behind the trait and collapse dispatch

1. Replace the two concrete fields with `providers: HashMap<ProviderKind, Box<dyn TtsProvider>>` (or two `Option<Box<dyn TtsProvider>>` fields if the HashMap fights the borrow checker — either removes the duplicated dispatch; prefer the map).
2. Add one accessor: `fn provider(&self, kind: ProviderKind) -> SpeechResult<&dyn TtsProvider>` returning the existing "provider not configured" error (find the exact error message the current match arms produce and reuse it verbatim so error-path tests stay green).
3. Rewrite the paired dispatch arms at 112/116, 129/134, 161/166, 352/359 to `self.provider(kind)?.method(...)`.
4. Leave 404/405 and 548/549 as matches (policy, per Scope).
5. Remove now-unused inherent wrappers only if nothing else calls them (`cargo clippy -- -D warnings` will flag dead code).

**Verify**: `cargo check -p codex-voice-tts && cargo test -p codex-voice-tts` → exit 0, all pass. `grep -c "ProviderKind::Google" crates/codex-voice-tts/src/client.rs` → expect ≤4 (the two policy sites, construction, and possibly the accessor error).

### Step 3: Full gates

**Verify**: the four AGENTS.md gates → exit 0.

## Test plan

No new behavior → no new behavior tests; the existing 77 tts tests (config resolution, fallback, chunking, formats) are the harness and must pass unchanged. Add exactly one new test: `provider_lookup_returns_not_configured_error_for_missing_provider` — build a config with only one provider, request the other, assert the same error text as before the refactor.

## Done criteria

- [ ] `TtsProvider` trait exists; both providers implement it
- [ ] Concrete `Option<GoogleSpeechClient>`/`Option<ElevenLabsSpeechClient>` fields are gone from `ConfiguredSpeechClient`
- [ ] Paired dispatch matches at the four sites are collapsed; the two policy matches remain
- [ ] All 77+ existing tests pass unchanged; `cargo test --workspace` exits 0
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- The two `synthesize` methods have materially different parameter lists or return types (the trait can't unify them without adapters — that is a design decision, report the diff).
- Any existing test's expected error message changes — the accessor must reproduce the current not-configured error verbatim; if the current arms produce *different* messages per site, report rather than pick one.
- Object safety fails (e.g. a generic method) — do not fall back to enum-dispatch silently; report.

## Maintenance notes

- Adding provider #3 after this: implement `TtsProvider`, extend `ProviderKind`, register in `try_new`, and update the two policy matches (format + fallback order) — the compiler's exhaustiveness check on those two matches is now the complete to-do list.
- The fallback swap at 548/549 hardcodes a two-provider cycle; with a third provider it should become an ordered list — deferred deliberately.
- Reviewer should scrutinize: that no dispatch site changed which provider it routes to (the diff should read as pure mechanical substitution).
