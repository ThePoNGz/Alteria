# Plan 002 — `alteria-gpui`: the runnable, performant frontend

**Goal:** Stand up the GPUI frontend so Alteria becomes a thing you can **run and feel**. A
window opens (`cargo run -p alteria-gpui`), it **renders** the engine's buffer + cursor(s) +
selection, and it feeds **real OS events** into `Editor::handle`. The defining property goes
from headless-tested to physically real: **hold Alt → WASD navigate → release → type lands
exactly where you navigated**, with keystroke→paint feeling instant.

Plan 001 built the entire pure-Rust engine (`alteria-core`, 155 tests green, no `gpui`
anywhere). This plan builds the *only* two things a frontend is allowed to do, per the one hard
rule: **(1)** translate OS events → plain-data `InputEvent`, and **(2)** read `Editor::buffer`
→ draw. Everything between is already done and frozen.

> **Read first — do not contradict:**
> - `../CLAUDE.md` — the one hard rule, the **frontend boundary**, "GPUI — read before you
>   write", the licensing line (GPUI = Apache, adapt freely; Zed's `editor`/`text`/`rope` =
>   GPL, read for approach only, **never paste**), and the parallel-agent workflow.
> - **The engine's real public API** (the contract this plan renders + drives — read the actual
>   source, it is short and frozen):
>   - `crates/alteria-core/src/lib.rs` — `Editor::new(&str)`, `Editor::handle(InputEvent) -> bool`
>     (`true` = repaint), `Editor.buffer` (public).
>   - `crates/alteria-core/src/input.rs` — `InputEvent { KeyDown{key,mods,repeat}, KeyUp{key,mods},
>     ModifiersChanged{mods}, FocusLost }`, `Key { Char(char), Backspace, Enter, Escape }`,
>     `Modifiers { alt, ctrl, shift, super_key }` (+ `NONE`).
>   - `crates/alteria-core/src/buffer.rs` — `Buffer { text: ropey::Rope, selection: Selection }`.
>   - `crates/alteria-core/src/selection.rs` — `Selection { ranges: Vec<Range>, primary: usize }`,
>     `Range { anchor, head }` (byte offsets), `primary()`.
> - `../../KEYMAP.md` (in the workspace's parent folder) — the behavior the engine already
>   implements. The frontend does **not** re-implement any binding; it only routes raw events in
>   and draws state out. Two entries are **frontend-only** (the core emits no action for them) and
>   are explicitly **deferred** here: `Alt+Space` (center viewport on the active line) and `Alt+.`
>   (keymap-sheet overlay).

---

## The framing correction this plan is built on

We are **not** using "98–100% of Zed." We use ~100% of **GPUI** — Zed's *Apache-licensed UI
framework* (windowing, input, the GPU render pipeline, cosmic-text shaping/raster). We use **0%
of Zed's editor** — `editor`, `text`, `rope`, `language` are GPL; we wrote our own engine
(`alteria-core`) instead. So:

- **What we inherit "like Zed":** the GPU-accelerated paint path and text shaping. That is where
  the *rendering* speed comes from, for free.
- **What we must write ourselves (this plan):** the **editor element** — read `Buffer` → shape
  lines → paint glyphs + cursors + selection — plus the input translation and scrolling. Zed's
  `EditorElement` is GPL; read it for *approach only*, and build ours on GPUI's Apache examples
  (`crates/gpui/examples/{input,text,uniform_list}.rs`).

The latency bar is `../CLAUDE.md`'s: **"feels instant on the files I actually open"** — single-
digit-ms keystroke→paint — **not** Zed's 120fps/huge-file engineering (an explicit non-goal). So
this plan renders the **visible viewport only** (cheap culling) and stops there; no SumTree-tier
virtualization.

---

## Scope

### In — the minimal frontend that makes Alteria real, end to end
- A new **binary crate** `alteria-gpui` depending on a **pinned GPUI git rev** + `alteria-core`
  (path dep). Added to the workspace.
- A **window** that opens on this machine (Wayland + Vulkan 1.4.335, already verified present).
- A custom **`EditorElement`** that renders the buffer's text (monospace), every **cursor**, and
  every **selection** span, multi-line.
- The **input pipeline**: raw `on_key_down` / `on_key_up` / `on_modifiers_changed` / focus-blur →
  a pure translator → `InputEvent` → `Editor::handle` → repaint when it returns `true`. **Not**
  routed through GPUI's action/keymap system (`../CLAUDE.md`).
- **Scrolling** that keeps the primary cursor in view, mouse-wheel scroll, and viewport culling
  (render only on-screen lines).
- The acceptance proof: the **quasimode works physically** (Alt-held WASD nav, release → type
  lands; Backspace/Enter/Esc; Alt+Shift extend; count; find; I/O/U/P expand; Alt+Ctrl spawn
  cursor; Ctrl+Z undo) — all already in the engine, now visible and driven by real keys.

### Out — explicitly deferred to named follow-on plans (each genuinely separable → parallelizable)
- **Plan 003 — mouse + view affordances:** click-to-place-caret, drag-to-select (needs glyph
  hit-testing), `Alt+Space` center-view, `Alt+.` keymap-sheet overlay. *(Honors "no-Alt = an
  ordinary editor / accessibility first" as the very next step.)*
- **Plan 004 — edit verbs needing OS/file resources + new core actions:** clipboard
  (`Ctrl+C/X/V`), redo (`Ctrl+Y`), save (`Ctrl+S`), file open. These add `Action`s to the core
  (planner-routed, since they touch `alteria-core`) and need the `DESIGN §9` file-scope decision.
- **Plan 005 — tree-sitter syntax highlighting** (grammar/ABI decisions; the deeper `O`/`P`
  expansion levels also wait on this).
- **Config / serialized keymap file**; grapheme-cluster motion + column memory (engine-side,
  later). None are in this plan.

---

## Files this plan owns
No other plan may touch these until 002 is merged. **This plan does not touch `alteria-core/src`
at all** — the engine is frozen; we only add a path dependency. The merge hotspots it *does*
touch (workspace `Cargo.toml` `members`, `Cargo.lock`) are planner-routed and 002 is the only
active plan against them.

```
Cargo.toml                                   # [workspace] members += alteria-gpui  (HOTSPOT)
Cargo.lock                                   # gpui + transitive deps               (HOTSPOT)
crates/alteria-gpui/Cargo.toml               # pinned gpui rev + alteria-core path dep
crates/alteria-gpui/src/main.rs              # binary entry — Application::new().run(...)
crates/alteria-gpui/src/app.rs               # root view: owns Editor + FocusHandle; routes input
crates/alteria-gpui/src/input.rs             # gpui event -> alteria_core::InputEvent (PURE, unit-tested)
crates/alteria-gpui/src/element.rs           # the custom EditorElement: layout + paint
crates/alteria-gpui/src/render.rs            # (optional) line-shaping/metrics helpers used by element.rs
```
(If a task proves a file unnecessary, drop it — fewer files is better. Do **not** add files
outside this list without routing through the planner.)

---

## Architecture rules the executor must follow (do not deviate)
- **The boundary is sacred:** `gpui` types appear **only** inside `alteria-gpui`. The core never
  sees one. The *only* data crossing into the engine is `alteria_core::InputEvent`; the *only*
  data crossing out is `&Editor::buffer` (read). If you reach for a `gpui` type to express engine
  logic, stop — that logic belongs in the core (a future planner-routed change), not here.
- **Quasimode rides on raw events, never GPUI's keymap.** Use `on_key_down` / `on_key_up` /
  `on_modifiers_changed` (+ a blur/focus-out hook). GPUI's Action/keymap system **cannot express
  "a modifier is held with no key"**, which is the whole concept. `ModifiersChangedEvent` fires on
  Wayland and X11 independent of other keys — that bare-modifier event is the primitive.
- **One concept per file**, mirroring the core's discipline. Plain translation in `input.rs`;
  paint in `element.rs`; ownership/wiring in `app.rs`.
- **TDD where it is honest:** the `input.rs` translator is a **pure function** and **must** be
  unit-tested test-first (a table of `gpui` keystroke/modifier inputs → expected `InputEvent`).
  The window/paint code is **not** unit-testable headlessly — verify it by **running** the app and
  visually confirming (a screenshot where useful). Don't fake a test that needs a GPU/window.

---

## GPUI — read before you write (this is the high-risk part; `../CLAUDE.md` is emphatic)
The agent's GPUI knowledge is **stale and thin**; the API moves ~weekly. **Every GPUI API in the
sketches below is illustrative — treat it as a hypothesis to verify, not gospel.** Before writing
any GPUI code:
1. **Pin a specific SHA** (Task 1) and read **that rev's** `crates/gpui/examples/` —
   authoritative — especially `input.rs` (text input: focus, key events, cursor/selection paint,
   the character-vs-key distinction), `text.rs` (shaping + painting a line), and
   `uniform_list.rs` (viewport-culled line rendering). Then the GPUI book + `gpui` docs +
   **Context7** (`/websites/rs_gpui_gpui`). *Then* write against the real, current API.
2. **Confirm every signature against the checkout** — `Application`/`App` entry, `open_window`,
   `WindowOptions`, the `Element` trait (`request_layout` → `prepaint` → `paint`),
   `FocusHandle`/`focusable`/`track_focus`, the key-event handlers and their event structs
   (`KeyDownEvent`, `KeyUpEvent`, `ModifiersChangedEvent`, `Keystroke`, `Modifiers`), the text
   system (`window.text_system().shape_line(...)`, `ShapedLine::paint`, `.x_for_index(...)` or
   the rev's equivalent), and quad/background painting (`window.paint_quad` / `fill`). If a name
   here differs from the checkout, **the checkout wins** — fix the plan's sketch in your devlog.

---

## Conventions (from `../CLAUDE.md`, enforced every task)
- TDD for the pure translator; run-and-verify for the GUI. `cargo fmt` + `cargo clippy` clean,
  no warnings, before every commit.
- **No `unwrap()`/`panic!` on event-reachable paths.** Handle: an event that maps to no
  `InputEvent` (return `None`, do nothing), an empty buffer, the cursor at byte 0 / EOF, a
  selection that spans off-screen, focus lost mid-hold (send `FocusLost` so no modifier sticks —
  the no-stuck-quasimode guarantee at the frontend).
- One concept per file. Commit style `feat(gpui): …` / `chore: …`. **Never** add a
  `Co-Authored-By`/AI/"Generated with" trailer; author stays the human git user.
- **Build cost:** GPUI's first compile is huge. Before building, point this worktree at the
  shared cache — `export CARGO_TARGET_DIR=~/.cache/alteria-target` (or `sccache`) — so branches
  don't each rebuild GPUI (`../CLAUDE.md`).

## Prerequisites (verified on this machine)
- `rustc 1.92.0` / `cargo 1.92.0` ✓. Wayland session ✓. **Vulkan instance 1.4.335 present** ✓
  (`vulkaninfo` works). Arch GPU deps per `../CLAUDE.md` assumed installed; **Task 1 confirms a
  Vulkan *device* is actually enumerated** when the window opens (instance ≠ device).

---

## Tasks

> Each task: **Files** · **What/sketch** (illustrative GPUI — verify against the pinned rev) ·
> **Verify** (run it; this milestone is proven by running, not only by `cargo test`) · **Commit**.
> The tasks are a **serial chain** — a window must exist before you can render into it, and render
> before you can see input land — so unlike a fan-out plan this is one executor, start to finish.

### Task 1 — Scaffold + pin GPUI + a window opens (de-risk the biggest unknown first)
**Files:** `Cargo.toml`, `Cargo.lock`, `crates/alteria-gpui/Cargo.toml`,
`crates/alteria-gpui/src/main.rs`
- Pick the GPUI rev **now and record it**: `git ls-remote https://github.com/zed-industries/zed HEAD`
  → use that SHA (or a recent tagged release). **Do not float the rev**; write the exact 40-char
  SHA into `crates/alteria-gpui/Cargo.toml` and the devlog.
  ```toml
  # crates/alteria-gpui/Cargo.toml
  [package]
  name = "alteria-gpui"
  version = "0.0.0"
  edition = "2021"
  license = "GPL-3.0-or-later"

  [dependencies]
  alteria-core = { path = "../alteria-core" }
  gpui = { git = "https://github.com/zed-industries/zed", rev = "<EXACT_SHA>" }
  ```
- Workspace `Cargo.toml`: add `"crates/alteria-gpui"` to `members`. **Recommended:** also set
  `default-members = ["crates/alteria-core"]` so the core's fast TDD loop and a bare `cargo
  build`/`cargo test` do **not** drag in the giant GPUI compile — GPUI builds only on an explicit
  `-p alteria-gpui`. (Verify this is the behavior you want against your cargo version.)
- `main.rs`: the smallest real window. **Verify the entry API against the pinned rev's
  `examples/` first** — sketch only:
  ```rust
  use gpui::*;
  fn main() {
      Application::new().run(|cx: &mut App| {
          cx.open_window(WindowOptions::default(), |_window, cx| {
              cx.new(|_cx| RootView) // a trivial view painting a solid background
          }).unwrap();
      });
  }
  ```
  (`unwrap()` on `open_window` at `main` startup is acceptable — it is *not* an event-reachable
  path; a failed window open should fail loudly at launch. Keep `unwrap`/`panic` out of every
  per-event path.)
- **Verify:** with `CARGO_TARGET_DIR` pointed at the shared cache, `cargo run -p alteria-gpui`
  **opens a window** and it paints a background color. Confirm it does **not** crash on
  device/surface creation (instance was verified; this proves a *device* is enumerated on Wayland/
  Vulkan). If it crashes, that is the single most important thing to solve before anything else —
  capture the error in the devlog.
- **Commit:** `chore: scaffold alteria-gpui (pinned gpui rev, window opens)`.

### Task 2 — Read-only render: the buffer's text + the primary cursor
**Files:** `crates/alteria-gpui/src/element.rs`, `crates/alteria-gpui/src/app.rs`,
(optional) `crates/alteria-gpui/src/render.rs`, `main.rs`
- `app.rs`: a root view that **owns an `Editor`** (`Editor::new("hello\nworld\n…")` static for
  now) and renders one `EditorElement` filling the window.
- `element.rs`: implement the **`Element`** trait (`request_layout` → `prepaint` → `paint`; this
  shape is confirmed current). In `paint`, for each line currently in `buffer.text`:
  shape it with the **window text system** in a **monospace** font at a fixed size, derive line
  height from font metrics, and paint the shaped line. Then paint the **primary cursor**
  (`buffer.selection.primary().head`, a byte offset) as a 1–2px vertical quad at that line/column
  — map byte offset → x via the shaped line's index→x (verify the method name on the rev).
  - Write **our own** element. GPUI's `text.rs`/`input.rs` examples (Apache) are the references to
    adapt; Zed's `EditorElement` (GPL) may be read for approach but **not pasted**.
  - Byte↔char: `Range` is byte offsets, ropey 1.6 is char-indexed — bridge with
    `text.byte_to_char(..)` exactly as the core does. Convert at the rope edge only.
- **Verify:** `cargo run -p alteria-gpui` shows the multi-line text and a caret at offset 0.
  Visual confirmation (screenshot if handy).
- **Commit:** `feat(gpui): editor element renders buffer text + primary cursor`.

### Task 3 — The input pipeline (the crux): OS events → `InputEvent` → `Editor::handle` → repaint
**Files:** `crates/alteria-gpui/src/input.rs`, `crates/alteria-gpui/src/app.rs`
- **`input.rs` — a pure, unit-tested translator** (TDD this one, test-first):
  ```rust
  // names illustrative — confirm gpui's event/Keystroke/Modifiers shape against the rev
  pub fn from_key_down(ev: &gpui::KeyDownEvent) -> Option<alteria_core::input::InputEvent>;
  pub fn from_key_up(ev: &gpui::KeyUpEvent) -> Option<alteria_core::input::InputEvent>;
  pub fn from_modifiers(ev: &gpui::ModifiersChangedEvent) -> alteria_core::input::InputEvent;
  pub fn modifiers(m: &gpui::Modifiers) -> alteria_core::input::Modifiers;
  ```
  - `gpui::Modifiers { alt, control, shift, platform/command, … }` → `alteria_core::Modifiers
    { alt, ctrl, shift, super_key }`. **Verify the gpui field names on the rev.**
  - `KeyDownEvent` → `Key::Char(c)` for a printable keystroke, else `Backspace`/`Enter`/`Escape`
    for those named keys; anything else → `None` (frontend ignores it). For the **Base layer**
    (no modifier) the typed character must carry **shift** (Shift+a ⇒ `Char('A')`); for modified
    layers case is irrelevant (the resolver lowercases). Use the rev's character/`key_char` field
    for the printable, the `key` name for the named keys — `examples/input.rs` shows exactly how
    this rev exposes "the character typed" vs "the key pressed".
  - `repeat`: pass GPUI's auto-repeat flag through to `KeyDown { repeat }`.
  - **Unit tests (no window needed — build the gpui event structs in-test):** plain `a` →
    `KeyDown{Char('a'), NONE}`; `Shift+a` → `Char('A')`; `Alt`-held `w` → `KeyDown{Char('w'),
    {alt}}`; bare `Backspace`/`Enter`/`Escape`; a modifiers-change to `{alt}` → `ModifiersChanged
    {alt}`; an unmapped key → `None`; field-name mapping alt/ctrl/shift/super correct.
- **`app.rs` — wire it:** give the root view a `FocusHandle`, **focus it on window open**, and
  attach `on_key_down` / `on_key_up` / `on_modifiers_changed` (raw — **not** GPUI actions) plus a
  **blur/focus-out** handler. Each handler: translate → if `Some(ev)`, `let redraw =
  self.editor.handle(ev); if redraw { cx.notify(); }`. Blur → `editor.handle(FocusLost)` so no
  held modifier can stick across focus loss.
- **Verify (this is the project's acceptance moment):** run it and physically confirm —
  - type `abc` → inserts; `Backspace` deletes; `Enter` newlines; `Esc` collapses a selection.
  - **hold Alt → `w/a/s/d` move the caret (do NOT type letters); release Alt → typing resumes and
    lands exactly where you navigated.** This is the whole thesis, now real.
  - `Alt`+`5`+`s` jumps 5 lines; `Alt`+`f`+`x` then `d`/`a` finds; `Ctrl+Z` undoes.
- **Commit:** `feat(gpui): raw key/modifier pipeline into Editor::handle (quasimode live)`.

### Task 4 — Render selections + all cursors (multicursor)
**Files:** `crates/alteria-gpui/src/element.rs`
- In `paint`, before the glyphs, paint a **highlight quad** behind every `Range` where
  `min != max` (split across lines: full-width middle lines, partial first/last). Paint **every**
  cursor in `selection.ranges`, with the **primary** visually distinct. Optional: a caret-blink
  timer (a repeating GPUI timer that toggles caret visibility and `notify`s) — keep it simple or
  skip for now.
- **Verify:** `Alt+Shift+d` shows a growing highlight; `I`/`O`/`U`/`P` expansion is visible;
  `Alt+Ctrl+s` then typing shows **two carets and two insertions**; `Esc` collapses to one caret.
- **Commit:** `feat(gpui): paint selections and multiple cursors`.

### Task 5 — Scrolling + viewport culling (the "feels instant" lever)
**Files:** `crates/alteria-gpui/src/element.rs`, `crates/alteria-gpui/src/app.rs`
- Keep a **vertical scroll offset** (lines or pixels) on the view. On each paint, **only shape +
  paint the lines intersecting the viewport** (never shape the whole file — this is what keeps a
  long file instant without SumTree-tier work). After an `Editor::handle` that returned `true`, if
  the **primary cursor** is outside the viewport, adjust the scroll offset to reveal it
  (cursor-follow). Wire **mouse-wheel** scroll (`on_scroll_wheel` or the rev's equivalent).
- (Horizontal scroll / soft-wrap: out of scope — defer. Long lines may clip for now; note it.)
- **Verify:** open a buffer taller than the window (e.g. read a real source file in via a static
  `include_str!`, or paste a long literal); `Alt+s` past the bottom keeps the caret on-screen;
  the wheel scrolls; scrolling a long buffer stays smooth (only visible lines are shaped).
- **Commit:** `feat(gpui): viewport scrolling with cursor-follow and line culling`.

### Task 6 — Latency sanity, boundary audit, polish, devlog
**Files:** small touch-ups across the crate; `devlog/002-gpui-frontend.md`
- **Latency:** informally confirm keystroke→paint *feels* instant on a normal file (single-digit
  ms is the bar). A quick way: log frame/handle timing behind a debug flag, or watch GPUI's frame
  stats. Don't build a benchmark harness — this is a feel check, per the non-goal.
- **Boundary audit (the non-negotiable):** `cargo tree -p alteria-core` shows **no `gpui`**;
  `grep -rn "gpui" crates/alteria-core/src` returns only the doc-comment mentions of the rule.
  `gpui` is imported **only** inside `alteria-gpui`.
- Window title `Alteria`; sensible default size; clean `cargo fmt` / `cargo clippy
  --all-targets -D warnings` / `cargo build`.
- **Devlog `002`** (same number as this plan): the **exact pinned GPUI SHA**, any place the real
  API differed from this plan's sketches (so plan 003+ is grounded in truth, not memory), the
  character-vs-key handling this rev required, the cursor/selection paint approach, whether
  `default-members` was used, the measured/felt latency, and anything that felt wrong to revisit.
- **Commit:** `feat(gpui): latency check, boundary audit, polish` + a separate
  `docs(devlog): add devlog 002 — gpui frontend`.

---

## Done criteria
- `cargo run -p alteria-gpui` opens a window that **renders the buffer + cursor(s) + selection**
  and is driven entirely by **real OS key/modifier events**.
- The **quasimode is physically real**: hold-Alt WASD navigation, release-to-type landing in
  place, extend/count/find/expand/multicursor/undo all visibly working through real keys.
- `input.rs` is a pure function with passing unit tests; GUI behavior verified by running.
- **The boundary holds:** `gpui` appears nowhere in `alteria-core`'s dependency tree or source;
  the engine saw only `InputEvent`s and exposed only `buffer`.
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo build` clean.
- The exact pinned GPUI SHA and every API deviation are recorded in `devlog/002`.

## Self-review (run before opening for review — mirrors `../CLAUDE.md`)
1. **Decoupled core:** no `gpui` in `alteria-core` (tree + grep); OS event → `InputEvent` happens
   only in `alteria-gpui`; the engine is read for `buffer`, driven by `handle`. ✓
2. **GPUI is current, not remembered:** every GPUI API used was checked against the **pinned
   rev's** examples/docs/Context7 — deviations from this plan's sketches are in the devlog. ✓
3. **Licensing:** GPUI (Apache) patterns adapted; **no Zed GPL editor source pasted**
   (`editor`/`text`/`rope` read for approach only). ✓
4. **Tests:** the pure `input.rs` translator is unit-tested test-first and green; window/paint
   verified by running (with a screenshot where it helps). ✓
5. **Clean build:** `cargo fmt`, `cargo clippy -D warnings`, `cargo build` warning-free. ✓
6. **Edges / no stuck quasimode:** unmapped event → `None`; empty buffer; caret at 0/EOF;
   off-screen selection; **focus lost mid-hold sends `FocusLost`** so no modifier sticks. ✓
7. **Frontend boundary:** the only data in is `InputEvent`, the only data out is `&buffer`. ✓

## Executor notes
- Work in your worktree (e.g. `alteria_a1`). **Sync first — reset, never rebase:**
  `git fetch origin && git reset --hard origin/main && git clean -fd`. Then **`export
  CARGO_TARGET_DIR=~/.cache/alteria-target`** (shared GPUI cache) before the first build.
- Execute the tasks **in order** (serial chain), commit per task, **push your own branch** (never
  `main`). Unpushed = invisible.
- The single highest risk is Task 1 (does GPUI build + open a window on this exact Wayland/Vulkan
  box). Do it first and do not proceed until a window reliably opens. If the pinned rev won't
  build, try the most recent tagged Zed release instead and record which SHA worked.
- When a GPUI API in this plan's sketch doesn't match the checkout, **the checkout is right** —
  adapt and note it in the devlog so plan 003 starts from reality.
- This plan deliberately stops at "runnable + feels instant." Mouse, clipboard, save/open, and
  highlighting are **named follow-on plans (003–005)**, kept out so this one stays shippable.
