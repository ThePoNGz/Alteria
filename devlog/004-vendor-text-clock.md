# Devlog 004 — Vendor Zed `text` + `clock` (Phase-2 foundation)

**Date:** 2026-06-01
**Plan:** `plans/004-vendor-text-clock.md`
**Branch:** `alteria_a2`
**Status:** ✅ Complete. `clock`, `collections`, and `text` are vendored, build standalone, are **gpui-free and ropey-free**, and are registered in the workspace. `cargo build --workspace` and `cargo clippy --workspace --no-deps` are clean (0 warnings). **`alteria-core` is untouched** — wiring these in is Plan 005.

---

## 1. What this chunk is

Pure crate-vendoring, exactly as Plan 002 did for `rope`/`sum_tree`: copy Zed's `clock`, `collections`, and `text` crates into the workspace and make them compile on their own, with no `gpui` and no `ropey`, and with the collaboration/async surface pruned so a single-user editor doesn't drag in `postage`/`regex`/`rand`. Nothing is wired into `alteria-core` yet; this just lays the foundation so Plan 005 can rebuild the edit/undo/selection layer on Zed's `Anchor` + Lamport-`clock` + `UndoMap` model.

## 2. Crates vendored

| Crate | Source | Notes |
|---|---|---|
| `clock` | `zed-main/crates/clock` | `ReplicaId`, `Lamport`, `Global` (version vector), `SystemClock`/`RealSystemClock`. Deps: `serde`, `smallvec` only. |
| `collections` | `zed-main/crates/collections` | `HashMap`/`HashSet`/`IndexMap`/`IndexSet` aliases over `rustc-hash`/`indexmap`, plus `vecmap`. |
| `text` | `zed-main/crates/text` | `Buffer`, `BufferSnapshot`, `Anchor`, `Fragment`/`Locator`/`InsertionFragment`, `UndoMap`, `History`/`Transaction`, `Patch`/`Edit`, `edits_since`, `ToOffset`/`ToPoint`/`FromAnchor`, `Subscription`. The Phase-2 spine. |

Dependency pins match Zed's workspace: `serde 1.0.221` (`derive`,`rc`), `smallvec 1.6` (`union`,`const_new`), `parking_lot 0.12.1`, `anyhow 1.0.86`, `rustc-hash 2.1.0`, `indexmap 2.7.0` (`serde`). All four new crates use `edition = "2024"` (matches the existing vendored crates; `text` uses let-chain-era code).

## 3. Verbatim-first, smallest change

Following the Plan-002 discipline and the Executor notes ("do not refactor Zed's logic; future Zed reads must line up"), the only `.rs` edits are deletions of dep-bearing code plus one hand-rolled helper. No algorithm was rewritten.

- **`clock`** — `clock.rs` verbatim. From `system_clock.rs` only the `test-support`-gated `FakeSystemClock`/`FakeSystemClockState` were dropped (their `parking_lot` was the crate's sole non-`std` dep); `RealSystemClock` (std `Instant`) kept.
- **`collections`** — `collections.rs` + `vecmap.rs` verbatim; dropped the `#[cfg(test)] mod vecmap_tests;` declaration and did not copy `vecmap_tests.rs`.
- **`text`** — see §4.

## 4. `text`: what was removed vs kept (read this before Plan 005)

The collab/async surface splits into "pulls a forbidden dependency" (**removed**) and "pure Rust, woven into the apply path" (**kept verbatim**). This is the starting surface Plan 005 inherits.

**Removed** (and the dependency each removal sheds):
- **Async coordination APIs → drops `postage`.** Deleted `Buffer::wait_for_edits`, `wait_for_anchors`, `wait_for_version`, `give_up_waiting`, and the private `resolve_edit` helper; the two `Buffer` fields they drove (`edit_id_resolvers: HashMap<Lamport, Vec<oneshot::Sender<()>>>` and `wait_for_version_txs: Vec<(Global, oneshot::Sender<()>)>`); their `Default::default()` inits in both constructors (`new`-path and `branch`); and inside `apply_op`, the `self.resolve_edit(edit.timestamp)` call plus the trailing `wait_for_version_txs.retain_mut(...)` block. These only existed to await remote ops over `postage` oneshot channels — nothing for a single-user buffer to wait on.
- **Line-ending normalizer → drops `regex`.** Replaced the `LINE_SEPARATORS_REGEX` (`\r\n|\r`) `LazyLock<Regex>` with a hand-rolled `fn normalize_line_separators(&str) -> Option<String>` that scans for `\r`, collapsing `\r\n` and lone `\r` to `\n`, returning `None` on the no-`\r` fast path (mirrors the old `Cow::Borrowed` path). The three `LineEnding::normalize*` call sites now use it.
- **`network.rs` → drops `rand` from the surface.** The in-memory collab test harness (`Network<T, R: rand::Rng>`) was `#[cfg(any(test, feature = "test-support"))]` already; deleted the file and its gated `pub mod network;` declaration.
- **Tests → drops `gpui`/`ctor`/`rand`/`zlog` dev-deps.** Did not copy `tests.rs`; deleted the inline `#[cfg(test)] mod tests` blocks in `locator.rs`, `operation_queue.rs`, and `patch.rs` (the `#[gpui::test]` attributes there were the crate's only `gpui` use).

**Kept verbatim** (pure Rust, no forbidden dep — left untouched to avoid risky surgery on the op-apply spine Plan 005 builds on):
- **The deferred-operation machinery**: `Buffer::deferred_ops: OperationQueue<Operation>`, `deferred_replicas: HashSet<ReplicaId>`, and `apply_ops`/`apply_op`/`can_apply_op`/`flush_deferred_ops`/`has_deferred_ops`/`deferred_ops_len`, plus the whole `operation_queue` module. This is collaboration-oriented (out-of-order remote-op application) but carries **no external dependency** and is interleaved with the core apply path, so removing it now would mean refactoring the spine for no dep win. It compiles clean and gpui-free. **This is the one piece of "collab surface" still compiled by default** — flagged here for the Reviewer; gate or remove it in Plan 005 if desired.
- **`subscription` (`Topic`/`Subscription`)**: kept — it is the edit-notification mechanism (`Buffer::subscribe`, `edits_since`), not collab-specific, and is useful to the frontend.
- **The `#[cfg(any(test, feature = "test-support"))]` surface inside `text.rs`** (e.g. `edit_via_marked_text`, `random_byte_range`, `get_random_edits`, the `RandomCharIter` import) and `selection::from_offset`: left verbatim. **Not compiled in the default build**, so they don't affect the gpui-free / dep-free / clippy guarantees. They do reference `rand`/`util::RandomCharIter`, which were **not** vendored, so `cargo test -p text` / `--features test-support` will not compile — acceptable, since `text` is wired into nothing until Plan 005 and this plan's bar is the default lib build + `clippy --no-deps`.

## 5. Manifest & lint decisions

- `text` declares an **empty** `[features] test-support = []` so the vendored `#[cfg(feature = "test-support")]` attributes are a recognized cfg (no `unexpected_cfgs` noise) without wiring the feature to any dep.
- `text` `[lints.clippy]` allows `init_numbered_fields`, `should_implement_trait`, `too_many_arguments`, `wrong_self_convention` — style lints that fire on vendored code; silenced at the manifest rather than editing Zed's `.rs`.
- `rope/Cargo.toml` gained `[lints.rust] unexpected_cfgs = { level = "warn", check-cfg = ['cfg(rust_analyzer)'] }` (declares the Zed-only `rust_analyzer` cfg as expected) and `[lints.clippy]` allowing `from_over_into`, `collapsible_else_if`, `should_implement_trait`.

## 6. Verification

- `cargo build -p clock -p collections -p text` and `cargo build --workspace` — green.
- `cargo tree -p {clock,collections,text} -e no-dev | grep -iE 'gpui|ropey|postage|regex'` — **empty** for all three.
- `cargo clippy --workspace --no-deps` — **0 warnings**.
- New crates are `cargo fmt`-clean.
- `git diff --name-only origin/main` confirms **no `alteria-core` change**.

## 7. Deviations from the plan

- **Workspace members registered incrementally** (folded into the `clock`/`collections`/`text` commits) instead of a separate final "register" commit, so **every commit builds green**. The plan's T5 thus collapsed into the final verification above — no separate registration commit.
- **`sum_tree/Cargo.toml` left untouched.** The plan listed it for lint-silencing, but `sum_tree` is already clippy-clean (verified with `--no-deps` and `--all-targets`); it has no `rust_analyzer` cfgs and triggers no style lints. Adding no-op `allow`s would be noise, so it was left verbatim.
- **Pre-existing `cargo fmt --all --check` diffs remain in vendored `rope`/`sum_tree` `.rs`** (from Plan 002; present on `origin/main`, confirmed by running `rustfmt --check` against the committed `origin/main` copy). They are verbatim Zed source and out of this plan's scope ("do not edit the vendored `.rs` logic"); not touched. The crates this plan added are fmt-clean.

## 8. Commits (on `alteria_a2`)

```
chore(core): vendor Zed clock (Lamport/Global), gpui-free
chore(core): vendor Zed collections (hash/index map aliases)
chore(core): vendor Zed text (Buffer/Anchor/UndoMap), slim collab surface
chore(core): silence vendored-crate lint noise (manifests only)
```
(plus this devlog.)
