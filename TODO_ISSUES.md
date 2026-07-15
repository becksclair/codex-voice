# TODO Issues

Local actionable backlog created by the `triage-issue` skill.

This file is for triaged engineering work that needs a clear owner, evidence, and a behavior-focused fix plan. Do not store secrets, credentials, customer data, private infrastructure details, exploit payloads, or sensitive logs here.

## Done

### [x] TODO-20260715-pwa-server-first-generation - [Web PWA] Healthy backend is bypassed by browser-direct generation

- Status: Done
- Type: Design flaw
- Priority: P1
- Confidence: High
- Security-sensitive: no
- Created: 2026-07-15
- Updated: 2026-07-15
- Source: /triage-issue PWA should use Codex through the backend normally and use browser-direct generation only while the backend is offline

#### Problem

**Actual behavior:** When `/web/config` provides enough browser credentials for direct generation, the PWA performs browser-direct prep and synthesis first. For server-only Codex prep, it eagerly substitutes the exported Google browser fallback. It reaches the backend speech-job route only after direct generation throws and the current settings happen to match server defaults.

**Expected behavior:** A reachable backend owns normal prep and synthesis, including Codex emotion preprocessing. Browser-direct prep and synthesis are an availability fallback used only when the backend is offline or unreachable.

**Reproduction:** Load the PWA while the backend is healthy and `/web/config` contains direct-provider configuration, then generate with default settings. Observe that provider requests are made from the browser before `/web/speech-jobs`. Repeat with the backend unreachable and confirm that the PWA needs a direct fallback.

**Impact:** Normal generation bypasses backend-owned Codex prep, exposes provider-specific routing to the browser path unnecessarily, and produces different emotion-prep behavior depending on which path wins. Backend health does not currently determine ownership.

#### Evidence

- `web/src/lib/generation.ts:498-504` documents the current direct-first decision tree.
- `web/src/lib/generation.ts:532-543` calls `generateDirect` whenever direct-capable config exists and calls `generateViaServer` only after a qualifying direct failure or when direct generation is unavailable.
- `web/src/lib/prep/decision.ts:94-125` eagerly replaces server-only Codex prep with the exported Google browser fallback for direct generation.
- `web/src/lib/generation.ts:471-495` already has a complete backend speech-job path that returns the prepared input and generated audio.
- The live backend was healthy on 2026-07-15, but its exported config made direct generation capable and included a Google fallback for server-owned Codex prep.
- Existing generation tests encode direct-first/server-fallback behavior; there is no server-first test proving direct fallback is restricted to an offline or unreachable backend.

#### Root Cause Analysis

The PWA's generation coordinator treats direct-capable configuration as a preference rather than a fallback capability. Backend reachability is not checked before choosing the owner, and the direct prep resolver silently changes Codex prep into Google prep. This reverses the intended boundary: the backend should be authoritative when available, while direct browser generation should preserve service during backend outages only.

#### TDD Fix Plan

1. **RED:** Add a generation-level test with a healthy speech-job backend and direct-capable config, asserting that generation uses `/web/speech-jobs`, returns the backend-prepared input, and makes no browser-direct provider request.
   **GREEN:** Make the backend speech-job path the default generation route regardless of direct-capable config.

2. **RED:** Add generation-level cases where backend job creation fails because the backend is unreachable, asserting that browser-direct prep and synthesis run once and generation still completes.
   **GREEN:** Fall back to direct generation only for network/offline backend failures, with cancellation and lifecycle interruptions kept distinct from offline status.

3. **RED:** Add cases for reachable backend HTTP errors, invalid requests, and explicit server rejections, asserting that they are surfaced and do not leak into browser-direct generation unless classified as backend unavailability.
   **GREEN:** Centralize the narrow backend-unavailable predicate used to authorize direct fallback.

**REFACTOR:** Rename direct-generation helpers or comments to make their offline-fallback role explicit and remove the now-obsolete direct-first server-default gate.

#### Acceptance Criteria

- [ ] A healthy backend always owns normal PWA prep and synthesis.
- [ ] Codex emotion prep remains server-side during normal operation.
- [ ] Browser-direct generation is attempted only when the backend is offline or unreachable.
- [ ] Backend validation/provider errors are not misclassified as offline failures.
- [ ] Cancellation, resume, pending-job persistence, provider fallback, and generated-text replacement still work.
- [ ] Server-first and offline-direct behavior are covered through generation-level tests.
- [ ] Sensitive details are not exposed in `TODO_ISSUES.md`.

#### Investigation updates

- 2026-07-15: Initial triage. Confirmed that the current coordinator is direct-first and that direct prep replaces server-owned Codex with Google before trying the healthy backend.
- 2026-07-15: Completed. Generation is backend-first, direct fallback is restricted to pre-job network failure, and backend jobs preserve provider, persona, model, and prep settings. Verified by 253 web tests, 266 Rust tests, 19 Playwright tests, clippy, rebuild, restart, and live health checks.
- 2026-07-15: Paid live smoke passed: exactly one backend job, zero browser-direct provider requests, playable/downloadable WAV output. Google synthesis hit a network send failure and the configured backend provider fallback completed audio.

### [x] TODO-20260715-pwa-emotion-tag-density - [Web PWA] Emotion preprocessing may generate with no tags or only one sparse cue

- Status: Done
- Type: Design flaw
- Priority: P1
- Confidence: High
- Security-sensitive: no
- Created: 2026-07-15
- Updated: 2026-07-15
- Source: /triage-issue PWA does not reliably insert emotion tags before generation and often inserts only one or two

#### Problem

**Actual behavior:** With emotion preprocessing enabled, generation can proceed with the original untagged text. When remote preprocessing fails and the local fallback runs, it can add at most one tag to the start of the complete message. Successful remote preprocessing is explicitly encouraged to use tags sparingly and may return the original text unchanged.

**Expected behavior:** Emotion preprocessing should reliably provide useful, text-preserving performance direction before synthesis. Suitable multi-paragraph or emotionally varied input should receive multiple local cues rather than a single message-wide cue, while neutral text may remain unchanged when that outcome is intentional and visible.

**Reproduction:** Enable Emotion in the PWA and generate speech from a message with several distinct emotional or delivery transitions. Repeat generation across successful, timed-out, and non-retryable prep responses. Inspect the text passed to synthesis and the preparation result/status.

**Impact:** Speech delivery is flatter and less predictable than the enabled setting promises. A user cannot distinguish an intentional no-op from a failed prep request, and the fallback cannot represent delivery changes within a message.

#### Evidence

- `web/src/lib/prep/prompts.ts:134-140` tells the model to use tags sparingly and explicitly permits returning the original text unchanged.
- `web/src/lib/prep/tags.ts:353-366` treats unchanged text as a valid performance-tag result.
- `web/src/lib/prep/prepare.ts:200-212` passes the original text through immediately after a non-retryable prep response rather than applying a local tag fallback.
- `web/src/lib/prep/prepare.ts:218-257` also passes the original text through for empty, over-limit, or non-preserving prep output.
- `web/src/lib/prep/tags.ts:402-434` selects only the first matching fallback candidate and prepends exactly one tag to the whole message.
- The live `/web/config` on 2026-07-15 exposed browser prep as a single Google fallback model with a 10-second attempt timeout and a 20-second overall timeout. The configured 6,000-character output limit is not the cause.
- Existing tests cover one successful leading tag, unchanged-output validity, and one fallback tag, but do not assert that the final synthesis input receives multiple cues for a multi-transition message or that all failed/invalid prep outcomes use a consistent fallback.

#### Root Cause Analysis

The enabled Emotion setting currently means “attempt sparse enrichment” rather than “produce useful performance direction before synthesis.” Three behaviors reinforce the reported result: the prompt biases the remote model toward very low tag density and allows a no-op; validation accepts that no-op as success; and error handling either silently uses the original text or invokes a fallback designed to add exactly one global tag. The PWA therefore has no enforceable contract for useful cue coverage and no consistent degraded behavior when remote prep fails.

#### TDD Fix Plan

1. **RED:** Add a generation-level test with emotionally varied, multi-paragraph input proving that, when Emotion is enabled and prep succeeds, the text sent to synthesis preserves the source wording and contains useful cues at more than one relevant transition.
   **GREEN:** Tighten the remote prep instruction and acceptance policy so suitable multi-transition text receives multiple local cues without requiring a fixed tag quota for neutral or short text.

2. **RED:** Add generation-level cases for unchanged, empty, invalid, non-retryable, and timed-out prep responses, asserting a consistent preparation result and that synthesis never silently mistakes a failed prep attempt for successful enrichment.
   **GREEN:** Route all failed or ineffective inline-tag outcomes through one fallback policy and surface whether preprocessing was enriched, intentionally unchanged, or degraded.

3. **RED:** Add a public fallback test using several distinct emotional transitions and assert that text is preserved while multiple matching cues are inserted near their relevant sentence or paragraph boundaries.
   **GREEN:** Replace the single leading-tag fallback with bounded, deduplicated, boundary-aware local tagging that respects the configured palette and maximum length.

**REFACTOR:** Consolidate remote-result classification and fallback application so each exit path cannot independently drift back to silent raw-text pass-through.

#### Acceptance Criteria

- [ ] Emotion-enabled generation applies text-preserving cues before synthesis for suitable emotionally varied input.
- [ ] Multi-transition input can receive multiple context-local tags; fallback behavior is not limited to one leading tag.
- [ ] Neutral or short input may intentionally remain unchanged without being conflated with prep failure.
- [ ] Empty, invalid, failed, and timed-out prep outcomes follow one tested degraded policy.
- [ ] The final synthesis-input behavior is covered through generation-level tests for each supported provider path.
- [ ] Existing wording-preservation, cancellation, provider fallback, and maximum-length behavior still works.
- [ ] Sensitive details are not exposed in `TODO_ISSUES.md`.

#### Investigation updates

- 2026-07-15: Initial triage. Confirmed the live 6,000-character limit is healthy; the sparse/no-op prompt, unchanged-output acceptance, inconsistent failure fallback, and single-tag heuristic explain the report.
- 2026-07-15: Completed. Removed the arbitrary backend tag-count cap, strengthened backend/browser prompts, added context-local multi-tag fallback and coverage correction, and verified the installed runtime with the same full gate set.
- 2026-07-15: Paid live smoke confirmed the prepared PWA text differed from the source and contained more than two bracketed performance tags before successful audio completion.

<!-- triage-issue:new-entries -->

## In Progress

Move entries here when someone starts work.

## Superseded / Duplicates

Move duplicate, obsolete, or merged entries here. Preserve the reason and the canonical issue ID.

## Entry Template

```md
### [ ] TODO-YYYYMMDD-short-slug - [Area] Concise behavior-focused title

- Status: Open
- Type: Bug | Regression | Missing behavior | Design flaw | Flaky test | Investigation
- Priority: P0 | P1 | P2 | P3 | Unknown
- Confidence: High | Medium | Low
- Security-sensitive: no
- Created: YYYY-MM-DD
- Updated: YYYY-MM-DD
- Source: /triage-issue <brief original report>

#### Problem

**Actual behavior:**

**Expected behavior:**

**Reproduction:**

**Impact:**

#### Evidence

- Current code/test/log evidence.
- File paths, symbols, and line numbers are allowed here when helpful, but treat them as investigation evidence, not required implementation details.

#### Root Cause Analysis

Explain the behavioral contract being violated and why the current system violates it. State uncertainty plainly.

#### TDD Fix Plan

1. **RED:** Write a public-interface test that ...
   **GREEN:** Make the smallest change that ...

2. **RED:** Write a public-interface test that ...
   **GREEN:** Make the smallest change that ...

**REFACTOR:** Optional cleanup after the behavior is covered.

#### Acceptance Criteria

- [ ] The reported behavior is fixed.
- [ ] The missing or regressed behavior is covered by public-interface tests.
- [ ] Existing related behavior still works.
- [ ] Sensitive details are not exposed in `TODO_ISSUES.md`.

#### Investigation updates

- YYYY-MM-DD: Initial triage.
```
