# Plan 002 — Migrate the text buffer from `ropey` to Zed's vendored `rope` (Phase 1)

> **✅ EXECUTED — Phase 1 complete.** `rope` + `sum_tree` are vendored and `alteria-core` is ported off `ropey` (155 tests green; no `gpui`/`ropey` in the dep tree). This file is the **record of done work — do not re-execute it.** Next: `plans/003` (motion fidelity) and `plans/004` (vendor `text`+`clock`) run in parallel; `plans/005` (anchors + clock undo) after both land.

**Goal:** Replace `ropey` (char-indexed) with **Zed's `rope` crate** (byte-indexed, built on
`sum_tree`), **vendored verbatim** from `zed-industries/zed` into this workspace. After this
plan `alteria-core` has **zero `ropey`**, `Buffer.text` is `rope::Rope`, the entire byte↔char
bridge and its workarounds are **deleted**, and every motion is byte/`Point`-based and ported
directly from Zed. `cargo test -p alteria-core` green; no `gpui` and no `ropey` in the tree.

This is **Phase 1** of adopting Zed's text stack. **Phase 2** (full plan: `plans/005`; sketched at the
end) adds `text` + `clock` — `Anchor`s and the Lamport-clock operation-log + `UndoMap` undo —
retiring the interim Helix-style changeset. Phase 1 keeps the existing `ChangeSet`/revision-tree
undo (re-pointed at `rope`) so the editor stays working after each task.

**Why (the bug this fixes):** `ropey` is char-indexed while our `Range`/`ChangeSet` are
byte-indexed, so today *every* buffer touch round-trips `byte_to_char`/`char_to_byte`, plus a
`byte_is_char_boundary` rounding workaround (`transaction.rs`) and a `nav_line_count`
phantom-line workaround (`executor.rs`). Worse, a non-Zed primitive makes Zed's editor source
impossible to reference. Zed's `rope` is byte-indexed with a `Point`/`Anchor` model — porting
Zed's `movement.rs` etc. becomes near-mechanical and the bridge layer disappears.

> **Sources of truth:** `../CLAUDE.md` (Stack + the two hard rules) and **Zed's source at
> `../zed-main/`** — vendor from and port against:
> - `crates/sum_tree/src/{sum_tree.rs, cursor.rs, tree_map.rs}` — the COW B-tree primitive.
> - `crates/rope/src/{rope.rs, chunk.rs, point.rs, point_utf16.rs, offset_utf16.rs, unclipped.rs}`.
> - `crates/editor/src/movement.rs` — the motion semantics to mirror in `executor.rs`.

---

## Confirmed facts (from source recon — don't re-litigate)
- `rope`/`sum_tree` **library code is gpui-free** (gpui appears only in their `#[cfg(test)]`).
  The one hard rule (`alteria-core` never imports `gpui`) holds.
- **Exact pins to use** (Zed's; a mismatch will not compile):
  `heapless = "0.9.2"` — **critical**: its `ArrayVec<T, N, u8>` / `String<N, u8>` length-generic
  is why Zed's node/chunk signatures compile; a stock heapless will not build.
  `unicode-segmentation = "1.10"` (resolves 1.12), `rayon = "1.8"` (resolves 1.11),
  `smallvec = { version = "1.6", features = ["union", "const_new"] }`, `log = "0.4"`.
- Only **3** `#[ztracing::instrument]` sites exist across `rope` + `sum_tree` → stub `ztracing`
  no-op **or strip the 3 attributes** (default: strip — zero new crates).
- `util` is used by the library in **exactly two spots** → **shim it; do not vendor it**
  (`util`'s manifest drags in `gpui_util`, `smol`, `git2`, `globset`, … — gpui-adjacent).
- **Licenses:** `sum_tree` Apache-2.0, `rope` GPL-3.0-or-later. Private, single-user,
  non-distributed → no obligation triggers. Matches `CLAUDE.md`'s "no licensing concern."

---

## Files this plan owns
```
Cargo.toml                              # workspace: members += sum_tree, rope, util-shim
crates/sum_tree/**                      # vendored verbatim from zed-main (+ ztracing stripped)
crates/rope/**                          # vendored verbatim from zed-main (+ util/ztracing shims)
crates/alteria-util-shim/**             # ~15 lines: is_utf8_char_boundary + debug_panic!
crates/alteria-core/Cargo.toml          # drop ropey; depend on rope (+ sum_tree if used directly)
crates/alteria-core/src/buffer.rs
crates/alteria-core/src/transaction.rs
crates/alteria-core/src/executor.rs
crates/alteria-core/src/expand.rs
crates/alteria-core/src/find.rs
crates/alteria-core/src/history.rs      # test helpers only (Rope in #[cfg(test)])
Cargo.lock
```
Untouched (already ropey-free): `input`, `action`, `keymap`, `resolver`, `selection`, `lib`.

---

## Tasks

### T1 — Vendor `sum_tree` + `rope` + shims; prove it builds gpui-free
- Copy `../zed-main/crates/sum_tree` → `crates/sum_tree` and `../zed-main/crates/rope` →
  `crates/rope`, **verbatim** (do not "tidy" — keep them line-for-line so future Zed reads line up).
- Rewrite each vendored `Cargo.toml`: replace `*.workspace = true` with concrete values
  (`edition = "2021"`, `publish = false`, drop `[lints] workspace`), pin deps with the literal
  versions above. Remove the **dev-dependencies** (`gpui`, `criterion`, `ctor`, `zlog`, `rand`)
  and **delete the `#[cfg(test)] mod tests`** in the vendored files — we test through
  `alteria-core`, and this removes the only `gpui` references.
- **`ztracing`:** strip the 3 `#[ztracing::instrument]` attributes and their `use ztracing::…`
  lines (or, if you prefer keeping them, add a 5-line `ztracing` shim crate whose `instrument`
  is a pass-through attribute macro). Default: strip.
- **`util` shim:** new crate `alteria-util-shim` (name it `util` in its `[lib]` so rope's
  `use util::…` resolves, or repoint the imports) exporting exactly:
  - `pub const fn is_utf8_char_boundary(b: u8) -> bool { (b as i8) >= -0x40 }`
  - `macro_rules! debug_panic { … }` → `panic!` under `#[cfg(debug_assertions)]`, else `log::error!`.
- **`rayon`:** keep it (cheap; Zed's `push_large`/`par_extend` use it). To drop it instead,
  remove `from_par_iter`/`par_extend` and the parallel branch of `push_large`, then drop the dep.
- **`Bitmap` cfg:** with tests deleted you can keep the verbatim `#[cfg(test)]` `Bitmap=u16` /
  `TREE_BASE=2` blocks (harmless, never compiled) — verbatim-keep is lowest-risk.
- **Verify:** `cargo build -p rope` green; `cargo tree -p rope | grep -i gpui` → empty;
  `cargo tree -p rope | grep -i ropey` → empty.
- **Commit:** `chore(core): vendor Zed rope + sum_tree (byte-indexed buffer), gpui-free`.

### T2 — `buffer.rs`: swap the type
- `use rope::Rope;`  `pub text: Rope`. `Buffer::from_str(s)` → `Rope::from(s)` (Zed impls
  `From<&str>`); keep the `from_str` fn name and signature.
- Tests: `.len_bytes()` → `.len()` (rope `len()` is **bytes**); `from_str("abc").text.len() == 3`.
- **Commit:** `feat(core): Buffer on Zed byte-indexed rope`.

### T3 — `transaction.rs`: byte-native apply/invert; delete the boundary workaround
- `apply(&mut self, text: &mut rope::Rope)`: replace the `byte_to_char` + `remove(char..)` /
  `insert(char_idx, …)` walk with **byte-range** ops — `text.replace(start..end, s)` (mirror
  Zed `Rope::replace`). No char conversion anywhere.
- `invert(&self, original: &rope::Rope)`: recover deleted text with a **byte-range slice**
  (`original.slice(start..end).to_string()`); use `len()` for byte length.
- **Delete `byte_is_char_boundary`** and the up-front validation pass in `is_applicable` that
  existed only for ropey's silent rounding. If you want validation, use `rope::Rope::is_char_boundary`
  / `clip_offset(.., Bias)`. Drop/rewrite `apply_rejects_a_midcodepoint_boundary_untouched`.
- Keep `Op` / `ChangeSet` / `map_pos` / `Assoc` shape unchanged (interim — Phase 2 replaces them
  with clock + anchors). This task is a primitive swap, not a redesign.
- **Commit:** `feat(core): byte-native changeset on rope; drop char-boundary workaround`.

### T4 — `executor.rs`: motions on byte/`Point`, ported from Zed `movement.rs`
- Delete **every** `byte_to_char` / `char_to_byte`. Each motion takes `&rope::Rope` + a byte
  `head` and returns a byte offset, using rope's API directly (cite `crates/editor/src/movement.rs`):
  - `char_horizontal` → prev/next **grapheme** boundary (Zed `movement::left/right`; rope
    `clip_offset(.., Bias)` + `chars_at`).
  - `vertical` → `offset_to_point(head)` → adjust `row`, clamp `column` to `line_len(row)` →
    `point_to_offset` (Zed `movement::up/down`). (`goal` column lands in Phase 2.)
  - `word_right`/`word_left` → scan via `chars_at(head)` / `reversed_chars_at(head)` (byte cursor,
    no char index).
  - `line_start`/`line_end` → `point_to_offset(Point::new(row, 0))` / `…(row, line_len(row))`.
  - `matching_bracket` + `close_for`/`open_for`/`enclosing_open` → scan `chars_at` over byte offsets.
  - `delete_backward`'s "char before head" → previous grapheme boundary (`clip_offset(head-1, Left)`).
- Replace `nav_line_count` (phantom-line workaround) with rope line counting: `max_point().row`
  (+ trailing-newline check via `line_len`/`Point`), mirroring how Zed counts the final line.
  Preserve the KEYMAP "blank-line leap skips the phantom trailing line" behavior.
- `from_changes(text.len())` / `identity(text.len())`: `len()` is bytes — direct.
- **Commit:** `feat(core): byte/Point motions ported from Zed movement`.

### T5 — `expand.rs`: byte-offset scans
- Drop the entry bridge (`byte_to_char` in, `char_to_byte` out). Rewrite `char_at`, `word_span`,
  `matching_close`, `enclosing_open`, `next_word_end`, `prev_word_start`, `enclosing`,
  `bracket_content`, `bracket_alternating` to index **byte offsets** via `chars_at`. Behavior is
  unchanged — the KEYMAP `I/O/U/P` worked examples are the tests.
- **Commit:** `feat(core): expansion on byte offsets`.

### T6 — `find.rs`: byte search + de-dup
- `find_on_line` byte-native (`Point` for line bounds, `chars_at` to scan).
- **De-duplicate `visual_line_len_chars`** — it is copied verbatim in `executor.rs` and `find.rs`.
  Make one shared helper (a small `buffer`/text helper) used by both.
- **Commit:** `feat(core): inline find on byte offsets; de-dup line-len helper`.

### T7 — Drop `ropey`; green the suite
- Remove `ropey = "1.6"` from `crates/alteria-core/Cargo.toml`; fix `history.rs` test helpers
  (`use rope::Rope`).
- **Verify:** `cargo test -p alteria-core` all green; `cargo fmt --check` + `cargo clippy` clean;
  `cargo tree | grep -i ropey` → empty; `cargo tree -p alteria-core | grep -i gpui` → empty.
- **Commit:** `chore(core): remove ropey dependency`.

---

## Done criteria (Phase 1)
- No `ropey` anywhere (`cargo tree` clean); `alteria-core` dep tree has no `gpui`.
- `Buffer.text: rope::Rope`; motions/edit/find/expand all on byte offsets + `Point`; the
  `byte_to_char`/`char_to_byte` bridge, `byte_is_char_boundary`, and char-based `nav_line_count`
  are **gone**.
- `cargo test -p alteria-core` green; `cargo fmt --check` + `cargo clippy` clean. All KEYMAP
  behaviors unchanged (same e2e suite passes).
- Coordinate discipline: every offset is a UTF-8 byte boundary; cross boundaries only via
  `clip_offset`/`clip_point` + `Bias`.

---

## Phase 2 — sketch (full plan: `plans/005`)
Vendor `clock` (near-zero deps) + `text` (slim: drop `postage`, `network.rs`,
`operation_queue.rs`, deferred-ops, `wait_for_*`, the `regex` line-ending normalizer — a
single-user editor needs none). Then:
- Move `Buffer`/`Selection` to **`Anchor`** (timestamp + offset + bias) resolved against
  snapshots; add a `goal` column to `Selection` (Zed vertical-movement parity).
- Replace `ChangeSet` + revision-tree with Zed's **operation log + Lamport `clock` + `UndoMap`**;
  undo becomes a forward op; implement real **redo** (undo-of-undo).
- This unlocks line-for-line porting of Zed's selection-history, multi-cursor, folds, and
  diagnostics (all anchor-based).

---

## Executor notes
- **Vendor verbatim first**: copy, make it compile with the smallest shims, *then* port
  `alteria-core`. Do not refactor Zed's `rope`/`sum_tree` — keeping them identical is the whole
  point (future Zed reads must line up).
- Pull any extra transitive dep version from `../zed-main/Cargo.lock` if something beyond the
  pinned list above is needed.
- Merge hotspots per `CLAUDE.md`: workspace `Cargo.toml`, `Cargo.lock`, `alteria-core/src/lib.rs`
  — this plan touches the first two; route through the Planner if branches are in play.
- TDD per `CLAUDE.md`: the existing `alteria-core` tests are the safety net — keep them green
  task by task; a motion's behavior must not change, only its coordinate basis.
