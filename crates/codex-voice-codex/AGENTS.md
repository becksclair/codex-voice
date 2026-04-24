# codex-voice-codex

## Package Identity

`codex-voice-codex` isolates compatibility with local Codex auth and the private ChatGPT transcription endpoint. It implements the core `TranscriptionClient` trait.

## Setup & Run

```bash
cargo check -p codex-voice-codex
cargo test -p codex-voice-codex
cargo clippy -p codex-voice-codex --all-targets -- -D warnings
cargo run -p codex-voice-app --bin codex-voice -- doctor codex-auth
cargo run -p codex-voice-app --bin codex-voice -- doctor transcribe --file /tmp/sample.wav
```

## Patterns & Conventions

- Auth reading and refresh live in `CodexAuthService` in `src/lib.rs`.
- Transcription HTTP lives in `CodexTranscriptionClient` in `src/lib.rs`.
- ✅ DO: Read `~/.codex/auth.json`; never write it directly.
- ✅ DO: Refresh auth by spawning `codex app-server --listen stdio://` and sending JSON-RPC lines, as `refresh()` does.
- ✅ DO: Kill/reap spawned Codex helpers on timeout, success, and setup/read failures; see `terminate_child()`.
- ✅ DO: Keep response parsing strict; see `is_account_read_response()` and `parse_transcript()` tests.
- ✅ DO: Redact HTTP error bodies with `redact()` before surfacing them.
- ❌ DON'T: Log or print `access_token` or full `account_id`; app diagnostics only report redacted values in `crates/codex-voice-app/src/main.rs`.
- ❌ DON'T: Move official OpenAI API support into this private-backend client; add a separate client behind the core trait later.

## Touch Points / Key Files

- Auth reader/refresh: `src/lib.rs`
- Transcription client: `src/lib.rs`
- Transcription trait: `crates/codex-voice-core/src/transcription.rs`
- CLI diagnostics: `crates/codex-voice-app/src/main.rs`
- Auth/transcription plan notes: `docs/execplan-rust-native-cross-platform.md`

## JIT Index Hints

```bash
rg -n "CodexAuthService|read_or_refresh|refresh|wait_for_account_read|terminate_child" src/lib.rs
rg -n "CodexTranscriptionClient|TRANSCRIBE_URL|multipart|parse_transcript" src/lib.rs
rg -n "access_token|account_id|redact|ChatGPT-Account-Id" src/lib.rs ../codex-voice-app/src/main.rs
rg -n "#\\[test\\]|is_account_read_response|parse_transcript" src/lib.rs
```

## Common Gotchas

- `codex app-server` can be long-lived; never use unbounded `wait_with_output()` for refresh.
- JSON-RPC success requires `id: 2`, a `result`, and no `error`.
- JSON transcription responses must contain `text` or `transcript`; raw JSON should not become inserted text.
- `doctor transcribe` may send real audio and print a preview; avoid full transcript logging.

## Pre-PR Checks

```bash
cargo fmt --check && cargo test -p codex-voice-codex && cargo clippy -p codex-voice-codex --all-targets -- -D warnings
```
