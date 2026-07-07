# Plan 012: Hoist the shared tray contract out of the three platform tray files

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 701ed3f..HEAD -- crates/codex-voice-ui/src`
> On drift, compare the "Current state" listings against the live code; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW on Linux (verifiable); MED on macOS/Windows (compile-only verification available)
- **Depends on**: none
- **Category**: tech-debt
- **Planned at**: commit `701ed3f`, 2026-07-07

## Why this matters

The three tray implementations each redeclare the identical public contract: a 6-variant `UiCommand` enum, the `StatusTray` API (`start`, `update`, `try_recv_command`, `status_sender`), and the icon-cache helpers (`build_icon_cache`, `icon_for_state`, `build_icon_for_state`). Adding a tray menu item or command variant currently means three identical edits — or silent drift (the files' line offsets already differ, only discipline keeps the contracts aligned). `lib.rs` re-exports per-`cfg`, so callers won't notice a shared definition.

## Current state

- `crates/codex-voice-ui/src/lib.rs:80-95` — per-`cfg` `mod` + `pub use` of `{LinuxUiConfig|WindowsUiConfig|MacOSUiConfig, StatusTray, UiCommand}` from each tray module.
- Verified-identical `UiCommand` (linux_tray.rs:27, macos_tray.rs:27, windows_tray.rs:42):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    StartTestRecording,
    SpeakText(String),
    PlayLastSpeech,
    OpenLogs,
    RunDiagnostics,
    Quit,
}
```

- Same-shaped `StatusTray` struct + methods (linux 48/68/72/77, macos 48/68/72/76, windows 63/83/87/91): `start(initial: UiStatus, config: <Platform>UiConfig) -> Result<Self, String>`, `update(&self, UiStatus)`, `try_recv_command(&self) -> Option<UiCommand>`, `status_sender(&self) -> std::sync::mpsc::Sender<UiStatus>`.
- Icon helpers in each file: `build_icon_cache() -> Result<HashMap<DictationState, Icon>, String>` (linux 395, macos 263, windows 406), `icon_for_state(&cache, state) -> Icon` (411/279/422), `build_icon_for_state(state) -> Result<Icon, String>` (423/291/434). `Icon` is `tray_icon::Icon` — confirm all three files use the same `Icon` type by reading their imports; if Windows uses a different icon type, the icon helpers stay per-platform (see Step 3).
- Menu-ID constants (`MENU_SETTINGS`, `MENU_LOGS`, ... at linux_tray.rs:20-24) — check whether the ID strings are identical across files; hoist only if identical.
- The genuinely platform-specific parts (event loops, gtk/winit/NSApplication plumbing, thread structure) stay put.
- Note: `Result<_, String>` error types are upgraded in plan 014 — do NOT change error types here; hoist as-is to keep this diff mechanical.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile (Linux, native) | `cargo check -p codex-voice-ui` | exit 0 |
| Cross-check Windows | `cargo check -p codex-voice-ui --target x86_64-pc-windows-msvc` (only if the target + toolchain are installed; else skip and note) | exit 0 |
| Tests | `cargo test -p codex-voice-ui` | all pass (3 at planning) |
| Lint | `cargo clippy -p codex-voice-ui --all-targets -- -D warnings` | exit 0 |
| Workspace | `cargo test --workspace` | all pass |

## Scope

**In scope**:
- `crates/codex-voice-ui/src/lib.rs`
- `crates/codex-voice-ui/src/tray_common.rs` (create)
- `crates/codex-voice-ui/src/{linux,macos,windows}_tray.rs` (deletions of hoisted items + imports)

**Out of scope** (do NOT touch):
- `crates/codex-voice-app/` — the re-export surface must keep every existing import path working unchanged (`use codex_voice_ui::{StatusTray, UiCommand}` in main.rs must not need edits).
- Tray behavior, menu structure, icon rendering logic — pure code motion.
- Error types (plan 014).

## Git workflow

- Branch: `advisor/012-dedupe-tray-implementations`
- One commit, e.g. `Hoist shared UiCommand and icon helpers into tray_common`.

## Steps

### Step 1: Create `tray_common.rs` with the shared contract

Move into it (defined once, not cfg-gated):
- `UiCommand` (the enum above, verbatim).
- The menu-ID constants IF identical across the three files (verify first; if any differ, leave all per-platform and note it).
- `ICON_SIZE` if identical.

Declare `mod tray_common;` in `lib.rs` unconditionally, and in each tray module replace the local definitions with `use crate::tray_common::UiCommand;` (plus consts as applicable). Update `lib.rs`'s `pub use` lines: `UiCommand` now re-exports from `tray_common` (one unconditional `pub use tray_common::UiCommand;` replaces the three per-cfg mentions — remove `UiCommand` from the per-platform `pub use` lists).

**Verify**: `cargo check -p codex-voice-ui && cargo check -p codex-voice-app` → exit 0 (proves app-side imports still resolve).

### Step 2: Hoist the icon helpers

If (per Current state) all three files build `tray_icon::Icon` the same way: move `build_icon_cache`, `icon_for_state`, `build_icon_for_state` into `tray_common.rs` once, delete the three copies, import in each tray file. First diff the three implementations (`diff <(sed -n '395,450p' src/linux_tray.rs) <(sed -n '263,310p' src/macos_tray.rs)` etc. after locating exact ranges) — hoist only if the bodies are identical modulo whitespace; if they differ materially (e.g. different pixel rendering per platform), hoist only the truly identical subset and report the divergence.

**Verify**: `cargo check -p codex-voice-ui` → exit 0; `cargo test -p codex-voice-ui` → 3 tests pass.

### Step 3: Do NOT hoist StatusTray itself

The `StatusTray` structs have identical method surfaces but platform-specific internals (thread + event-loop plumbing). Unifying them behind a trait or generic would be a redesign, not deduplication — explicitly out of this plan. Instead, add a compile-time contract test in `tray_common.rs`:

```rust
#[cfg(test)]
mod contract {
    // Fails to compile if the platform StatusTray drops or changes a method.
    #[test]
    fn status_tray_has_the_shared_surface() {
        fn _assert(tray: &crate::StatusTray) {
            let _: Option<crate::tray_common::UiCommand> = tray.try_recv_command();
            let _: std::sync::mpsc::Sender<crate::UiStatus> = tray.status_sender();
        }
    }
}
```

(Adapt to what actually compiles — the goal is a cheap tripwire that the per-platform surfaces stay aligned.)

**Verify**: `cargo test -p codex-voice-ui` → passes.

### Step 4: Full gates

**Verify**: the four AGENTS.md gates → exit 0. Then manual smoke on the operator's machine: `cargo run -p codex-voice-app --bin codex-voice -- run` → tray appears, menu items work. Note "manual smoke pending" if you can't run a session.

## Test plan

The contract tripwire test (Step 3) plus the existing 3 ui tests. macOS/Windows verification is compile-only unless targets are installed — say so in the report.

## Done criteria

- [ ] `grep -c "enum UiCommand" crates/codex-voice-ui/src/*.rs` → exactly 1 (in tray_common.rs)
- [ ] `grep -c "fn build_icon_cache" crates/codex-voice-ui/src/*.rs` → 1 (or documented divergence)
- [ ] `cargo check -p codex-voice-app` exits 0 with zero changes to the app crate
- [ ] All four gates pass
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report back if:

- The three `UiCommand` enums are NOT byte-identical at execution time (drift since planning) — reconcile is a product decision.
- The icon helpers differ materially between platforms.
- Hoisting requires touching `crates/codex-voice-app` imports.
- macOS/Windows cfg code fails to compile and you cannot check those targets — note which targets were verified.

## Maintenance notes

- New tray commands are now added in one place; the per-platform files only wire menu-item IDs to `UiCommand` variants.
- Plan 014 (typed errors) will change the hoisted signatures' `Result<_, String>` — this plan deliberately preserves them.
- Reviewer should scrutinize: the per-cfg `pub use` surgery in lib.rs (easy to break the non-Linux cfgs without noticing on a Linux dev box).
