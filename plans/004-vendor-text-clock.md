# Plan 004 — Vendor Zed `text` + `clock` (Phase-2 foundation)

**Goal:** Vendor Zed's `clock` and `text` crates (+ a `collections` shim) into the workspace and make them **build standalone, gpui-free** — exactly as Plan 002 did for `rope`/`sum_tree`. **Do NOT wire them into `alteria-core` yet** (that's Plan 005). This is pure crate-vendoring so it can run in parallel with Plan 003. Also tidy the rope/sum_tree lint noise so the workspace is clippy-clean.

**Parallelism:** Runs **simultaneously with Plan 003** (disjoint files). Owns only new crate dirs + the workspace `Cargo.toml` members line + the rope/sum_tree manifests (lints only). Touches **no** `alteria-core` source and **no** `alteria-core/Cargo.toml`.

> **Sources of truth:** `../CLAUDE.md`, and Zed at `../zed-main/crates/{clock,text,collections,util}`. Recon facts (verified):
> - `clock` library deps: `serde`, `smallvec` only (`parking_lot` is test-support) — **no Zed-internal deps, no gpui.** Cheapest to vendor.
> - `text` library deps: `rope`, `sum_tree`, `clock` (vendored already / here), `collections` (Zed-internal, tiny), `util` (shim already exists at `crates/util-shim`), and crates.io `anyhow`, `parking_lot`, `smallvec`, plus the **collab surface** `postage` (async channels) + `regex` (line-ending normalizer). `text` library does **not** import `gpui` (only its tests do).
> - `collections` is just `HashMap`/`HashSet`/`IndexMap` aliases over `rustc-hash`/`indexmap` — **no gpui, ~tiny.** Vendor verbatim or shim.
> - Zed dep pins (match): `smallvec = { version = "1.6", features=["union","const_new"] }`, `parking_lot = "0.12.1"`, `anyhow = "1.0.86"`, `serde` (workspace), `rustc-hash`/`indexmap` (from collections' manifest), `regex` (workspace).

## Files this plan owns
```
crates/clock/**          # NEW — vendored verbatim
crates/collections/**    # NEW — vendored verbatim (or a small shim crate named `collections`)
crates/text/**           # NEW — vendored, collab surface slimmed/gated, tests stripped
Cargo.toml               # workspace: members += clock, collections, text
crates/rope/Cargo.toml      # lints-only: silence the rust_analyzer cfg + style lints
crates/sum_tree/Cargo.toml  # lints-only: same
```
Does **not** touch `crates/alteria-core/**` at all.

## Tasks

### T1 — Vendor `clock`
- Copy `zed-main/crates/clock` → `crates/clock`, verbatim. Rewrite `Cargo.toml` self-contained (`edition = "2024"` — Zed uses let-chains; concrete `serde`/`smallvec` pins; drop `[lints] workspace`, drop `parking_lot`/test-support + `[features]`). Delete `#[cfg(test)] mod tests` and `system_clock.rs`'s fake-clock test bits if they pull test-only deps (the real `SystemClock` uses only `std::time::Instant` — keep it).
- **Verify:** `cargo build -p clock` green; `cargo tree -p clock | grep -i gpui` empty.
- **Commit:** `chore(core): vendor Zed clock (Lamport/Global), gpui-free`.

### T2 — Vendor `collections`
- Copy `zed-main/crates/collections` → `crates/collections`, verbatim (it's tiny). Self-contained `Cargo.toml` (deps `rustc-hash`, `indexmap` per Zed's manifest; drop workspace lints/test-support). Strip any `#[cfg(test)]`.
- **Verify:** `cargo build -p collections` green; gpui-free.
- **Commit:** `chore(core): vendor Zed collections (hash/index map aliases)`.

### T3 — Vendor `text`, slim the collab surface
- Copy `zed-main/crates/text` → `crates/text`. Rewrite `Cargo.toml` self-contained: deps `rope = {path="../rope"}`, `sum_tree = {path="../sum_tree"}`, `clock = {path="../clock"}`, `collections = {path="../collections"}`, `util = {path="../util-shim"}`, plus `anyhow`, `parking_lot`, `smallvec` (concrete pins). Drop dev-deps (`gpui`, `rand`, `ctor`, `zlog`).
- **Strip tests:** delete `#[cfg(test)] mod tests` / `tests.rs` (removes the only `gpui` use). Strip `#[ztracing::instrument]`/`use ztracing` if present (as in 002).
- **Collab surface — slim (single-user has no use for it):** remove or `#[cfg(feature = "collab")]`-gate `network.rs`, `operation_queue.rs`, deferred-ops (`deferred_ops`, `flush_deferred_ops`, `can_apply_op`), the `wait_for_edits/anchors/version` async APIs, and the `postage` dependency. Replace the `regex` line-ending normalizer (`LINE_SEPARATORS_REGEX`) with a hand-rolled `\r\n`/`\r` scan (drops the `regex` dep). **Lower-risk alternative if slimming fights the borrow checker:** keep the code, `#[cfg(feature="collab")]`-gate it off (default features empty) and keep `postage`/`regex` as optional deps — get it building first, slim during Plan 005. Pick whichever lands green faster; note the choice in the devlog.
- Keep the core: `Buffer`, `BufferSnapshot`, `Anchor`, `Fragment`/`Locator`/`InsertionFragment`, `UndoMap`, `History`/`Transaction`, `Patch`/`Edit`, `edits_since`, `ToOffset`/`ToPoint`/`FromAnchor`. (These are what Plan 005 builds on.)
- **Verify:** `cargo build -p text` green; `cargo tree -p text | grep -iE 'gpui|ropey'` empty.
- **Commit:** `chore(core): vendor Zed text (Buffer/Anchor/UndoMap), collab surface gated off`.

### T4 — Silence vendored rope/sum_tree lint noise
**Files:** `crates/rope/Cargo.toml`, `crates/sum_tree/Cargo.toml`
- Add a `[lints.rust]` allowing the `unexpected_cfgs` warning for `rust_analyzer` (Zed defines that cfg in its workspace; we dropped it), e.g. `unexpected_cfgs = { level = "allow", check-cfg = ['cfg(rust_analyzer)'] }`, and allow the handful of vendored-code clippy style lints (`from_over_into`, `collapsible_else_if`, `should_implement_trait`) via `[lints.clippy]`. Do **not** edit the vendored `.rs` logic.
- **Verify:** `cargo clippy --workspace --no-deps` shows **zero** warnings from `rope`/`sum_tree`/`clock`/`collections`/`text`.
- **Commit:** `chore(core): silence vendored-crate lint noise (manifests only)`.

### T5 — Register + final verify
**Files:** `Cargo.toml` (workspace)
- `members += ["crates/clock", "crates/collections", "crates/text"]`.
- **Verify:** `cargo build -p text -p clock -p collections` green; `cargo tree -e no-dev | grep -iE 'gpui|ropey'` empty across the new crates; `cargo clippy --workspace --no-deps` clean.
- **Commit:** `chore(core): register vendored text/clock/collections in workspace`.

## Done criteria
- `clock`, `collections`, `text` vendored and building green, **gpui-free** and **ropey-free**, registered in the workspace.
- Collab surface removed or gated off (not compiled by default); no `postage`/`regex` in the default build (or clearly optional).
- Whole-workspace `cargo clippy --no-deps` is warning-free.
- **`alteria-core` is untouched** — its integration with these crates is Plan 005.

## Executor notes
- Verbatim-first, like Plan 002: copy → make it compile with the smallest shims → stop. Do not refactor Zed's logic; future Zed reads must line up.
- The whole point is Plan 005 can then depend on `text`/`clock` and rebuild the edit/undo/selection layer on Zed's `Anchor` + Lamport-clock + `UndoMap` model. Leave a one-paragraph note in `devlog/004` on exactly what collab code you removed vs gated, so Plan 005 knows the starting surface.
