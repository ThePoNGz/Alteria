# Plan 002 — `alteria-gpui`: the interactive frontend (run & feel the editor)

**Goal:** Build the complete GPUI frontend so Alteria becomes a real, usable editor you can
**run, type in, select with keyboard *and* mouse, scroll, copy/paste, and save**. A window opens
(`cargo run -p alteria-gpui -- <file>`), it **renders** the engine's buffer + cursor(s) +
selection (Zed-grade paint path), and it feeds **real OS events** into `Editor::handle`. The
defining property goes physical: **hold Alt → WASD navigate → release → type lands exactly where
you navigated**, instant.

Plan 001 built the entire pure-Rust engine (`alteria-core`, 155 tests green, no `gpui`). This
plan builds the *only* two things a frontend may do (the one hard rule): **(1)** translate OS
events → plain-data `InputEvent`, and **(2)** read `Editor::buffer` → draw. OS side-effects that
the pure core cannot own (clipboard, disk) are handled here too, since they *must* live next to
the window.

> **Runs in parallel with Plan 003** (`alteria-syntax`, the highlighting/theme layer). The two
> plans own **disjoint files** (separate crates) and **neither touches `alteria-core/src`**. The
> only shared files are the workspace `Cargo.toml` `members` list and `Cargo.lock` — the
> planner-routed merge hotspots; the Reviewer merges them. **This plan's renderer consumes a
> highlight hook with an *empty default* (plain text); it does NOT import `alteria-syntax`.** The
> Reviewer writes the ~15-line adapter (003's spans+theme → this plan's paint colors) at merge
> (see "The 002↔003 interface" below).

> **Read first — do not contradict:**
> - `../CLAUDE.md` — the one hard rule, the **frontend boundary**, "GPUI — read before you
>   write", **licensing** (GPUI + `gpui_*`/`sum_tree`/`collections` = Apache → adapt patterns
>   freely; Zed's `editor`/`text`/`rope` = GPL → read for *approach* only, **never paste** — and
>   they're built on Zed's rope, not ropey, so they wouldn't drop in anyway), the parallel-agent
>   workflow.
> - **The engine's real public API** (the frozen contract this plan renders + drives — read the
>   actual source, it's short):
>   - `crates/alteria-core/src/lib.rs` — `Editor::new(&str)`, `Editor::handle(InputEvent) -> bool`
>     (`true` = repaint), `Editor.buffer` (public).
>   - `crates/alteria-core/src/input.rs` — `InputEvent { KeyDown{key,mods,repeat}, KeyUp{key,mods},
>     ModifiersChanged{mods}, FocusLost }`, `Key { Char(char), Backspace, Enter, Escape }`,
>     `Modifiers { alt, ctrl, shift, super_key }` (+ `NONE`).
>   - `crates/alteria-core/src/buffer.rs` — `Buffer { text: ropey::Rope, selection: Selection }`.
>   - `crates/alteria-core/src/selection.rs` — `Selection { ranges: Vec<Range>, primary: usize }`,
>     `Range { anchor, head }` (byte offsets), `primary()`.
> - `../../KEYMAP.md` (workspace parent folder) — the behavior the engine already implements. The
>   frontend re-implements **no** binding; it routes raw events in and draws state out. Two keymap
>   entries are **frontend-only** (the core emits no action) and are **deferred to Plan 003-mouse
>   or later**: `Alt+Space` (center viewport) and `Alt+.` (keymap-sheet overlay).

---

## The "like Zed" question, settled

We use ~100% of **GPUI** (Zed's Apache UI framework: window, input, GPU paint, cosmic-text
shaping) and **0% of Zed's editor** (`editor`/`text`/`rope` are GPL and fused to Zed's stack; we
have our own engine on ropey). So:

- **The rendering technique is modeled on Zed's `EditorElement`** — `layout → prepaint → paint`,
  a shaped-line cache, batched cursor/selection quads, viewport culling. **That approach is what
  makes it fast, and it's exactly what we copy** (the technique — our own code, on `alteria-core`
  + ropey + GPUI's Apache examples).
- **The editing/keyboard model is NOT Zed's** — it's already built (`alteria-core`, quasimodes),
  and GPUI's action system literally can't express held-modifier layers. So "like Zed" applies to
  **render performance and GPUI usage**, not to input semantics.

Latency bar (`../CLAUDE.md`): **"feels instant on the files I actually open"** — single-digit-ms
keystroke→paint — **not** 120fps/huge-file engineering (explicit non-goal). Render the **visible
viewport only**; no SumTree-tier virtualization.

---

## Scope

### In — the full interactive editor (everything pixels + OS that the pure core can't own)
- A **binary crate** `alteria-gpui` on a **pinned GPUI git rev** + `alteria-core` (path dep).
- A **window** (Wayland + Vulkan 1.4.335, verified present on this box).
- A custom **`EditorElement`**: render the buffer's text (monospace), every **cursor**, every
  **selection** span, multi-line; consume an (initially empty) **highlight-span hook** for colors.
- The **keyboard pipeline**: raw `on_key_down`/`on_key_up`/`on_modifiers_changed`/blur → a pure
  translator → `InputEvent` → `Editor::handle` → repaint on `true`. Not via GPUI's action system.
- **Mouse**: click → place caret (pixel→offset hit-testing), drag → select, wheel → scroll.
- **Scrolling** with cursor-follow + viewport culling.
- **Clipboard** (`Ctrl+C`/`X`/`V`) and **file open** (path from argv) / **save** (`Ctrl+S`) —
  handled in the frontend (OS side-effects; see "Frontend-owned verbs" below).
- The acceptance proof: the **quasimode is physically real** end to end.

### Out — deferred (named follow-ons; not this plan)
- **Syntax highlighting + theme + language detection** → **Plan 003** (parallel, separate crate).
- **Richer document model** (multi-file, file picker dialog, unsaved-changes prompt, encodings
  beyond UTF-8) → later. v1 file scope = the single file passed on the command line (`DESIGN §9`).
- **IME composition**, soft-wrap / horizontal scroll, gutter/line-numbers, tabs/sidebar/status
  bar, command palette, autocomplete, minimap → later.
- New **core actions** (e.g. `Insert(String)` for fast paste) — to keep this plan from touching
  `alteria-core`, **paste feeds the clipboard string as a sequence of `InsertChar` events** (v1
  simplification; a bulk-insert action is a later planner-routed core change).

---

## Frontend-owned verbs (why clipboard/file live here, not in the engine)
The pure core forbids OS side-effects, so a few `Ctrl` combos are **intercepted by the frontend
before routing to the engine**:
- `Ctrl+C` → read the primary selection's text from `buffer.text`, write to the OS clipboard.
- `Ctrl+X` → `Ctrl+C` then send a `Backspace` `InputEvent` (the engine deletes a selection on
  Backspace — see devlog 001), so cut needs no core change.
- `Ctrl+V` → read the OS clipboard, feed it as `InsertChar` events (v1).
- `Ctrl+S` → write `buffer.text` to the open file path (`std::fs`).
- `Ctrl+Z` is **not** intercepted — it routes to the engine (`Undo`), as today.

Everything else flows unchanged: translate → `Editor::handle`.

---

## The 002↔003 interface (frozen now — both plans build to it blind)
- **003 (`alteria-syntax`) owns** the highlight data types (`StyleSpan { range: Range<usize>,
  kind: HighlightKind }`, `Theme`, a plain `Rgba`) and `highlight(&Rope, lang) -> Vec<StyleSpan>`.
- **002 does NOT import `alteria-syntax`.** Its `EditorElement` paints from its **own** field:
  `highlights: Vec<(core::ops::Range<usize>, gpui::Hsla)>`, **defaulting empty** (→ plain text).
  Build and verify 002 entirely with this empty (and one hand-built test value, to prove the paint
  path colors spans).
- **The Reviewer writes the adapter at merge** (~15 lines): on buffer change, call
  `alteria_syntax::highlight`, map each `StyleSpan` through the `Theme` to a `gpui::Hsla`, set
  `highlights`. Neither executor touches the other's crate.

---

## Files this plan owns
**Does not touch `alteria-core/src` at all** (engine frozen) **nor any `alteria-syntax` file**
(that's 003). Shared hotspots are only the workspace manifest + lockfile.

```
Cargo.toml                                   # [workspace] members += alteria-gpui  (HOTSPOT, shared w/003)
Cargo.lock                                   # gpui + transitive deps               (HOTSPOT, shared w/003)
crates/alteria-gpui/Cargo.toml               # pinned gpui rev + alteria-core path dep
crates/alteria-gpui/src/main.rs              # binary entry; argv path; Application::new().run(...)
crates/alteria-gpui/src/app.rs               # root view: owns Editor + FocusHandle + file path + scroll; routes input
crates/alteria-gpui/src/input.rs             # gpui event -> alteria_core::InputEvent (PURE, unit-tested)
crates/alteria-gpui/src/element.rs           # custom EditorElement: layout + paint (text/cursors/selection/highlights)
crates/alteria-gpui/src/render.rs            # (optional) line-shaping cache + metrics + offset<->pixel mapping
crates/alteria-gpui/src/clipboard.rs         # (optional) Ctrl+C/X/V + Ctrl+S handlers (frontend verbs)
```
(Drop any file a task proves unnecessary; don't add files outside this list without the planner.)

---

## Architecture rules (do not deviate)
- **The boundary is sacred:** `gpui` types live **only** in `alteria-gpui`. The only data into
  the engine is `InputEvent`; the only data out is `&Editor::buffer`. Reaching for a `gpui` type
  to express engine logic = the rule breaking.
- **Quasimode rides on raw events, never GPUI's keymap.** `on_key_down`/`on_key_up`/
  `on_modifiers_changed` + a blur hook. `ModifiersChangedEvent` (fires on Wayland & X11
  independent of other keys) is the primitive the whole concept needs.
- **One concept per file.** Translation in `input.rs`; paint in `element.rs`; OS verbs in
  `clipboard.rs`; ownership/wiring in `app.rs`.
- **TDD where honest:** `input.rs` is a **pure function** — unit-test it test-first (table of
  gpui keystroke/modifier → expected `InputEvent`). Also unit-test the **offset↔pixel mapping**
  in `render.rs` if extracted as a pure fn. Window/paint code isn't headless-testable — verify by
  **running** (screenshot where useful). Don't fake GPU/window tests.

---

## GPUI — read before you write (the high-risk part; `../CLAUDE.md` is emphatic)
The agent's GPUI knowledge is **stale**; the API moves ~weekly. **Every GPUI API in the sketches
below is a hypothesis to verify, not gospel.** Before writing any GPUI code:
1. **Pin a SHA** (Task 1), then read **that rev's** `crates/gpui/examples/` — authoritative:
   `input.rs` (focus, key events, the character-vs-key distinction, cursor/selection paint,
   mouse hit-testing), `text.rs` (shape + paint a line), `uniform_list.rs` (viewport culling).
   Then the GPUI book, `gpui` docs, and **Context7** (`/websites/rs_gpui_gpui`). *Then* write.
2. **Confirm every signature against the checkout:** `Application`/`App`, `open_window`,
   `WindowOptions`, the `Element` trait (`request_layout`→`prepaint`→`paint` — this shape is
   confirmed current), `FocusHandle`/`focusable`/`track_focus`, the event structs (`KeyDownEvent`,
   `KeyUpEvent`, `ModifiersChangedEvent`, `Keystroke`, `Modifiers` field names, `MouseDownEvent`,
   `MouseMoveEvent`, `ScrollWheelEvent`), the text system
   (`window.text_system().shape_line(...)`, `ShapedLine::paint`, index↔x), quad/background paint
   (`window.paint_quad`/`fill`), and clipboard (`cx.write_to_clipboard`/`read_from_clipboard` or
   the rev's names). **If a name differs, the checkout wins — record the truth in the devlog.**

---

## Conventions (`../CLAUDE.md`, every task)
- TDD the pure pieces; run-and-verify the GUI. `cargo fmt` + `cargo clippy --all-targets -D
  warnings` clean before each commit.
- **No `unwrap()`/`panic!` on event-reachable paths.** Handle: event → no `InputEvent` (`None`,
  do nothing), empty buffer, caret at 0/EOF, off-screen selection, **focus lost mid-hold → send
  `FocusLost`** so no modifier sticks, a missing/unreadable file path (show empty buffer, don't
  panic), clipboard empty. (`unwrap` is OK only at `main` startup, e.g. `open_window`.)
- One concept per file. Commit `feat(gpui): …` / `chore: …`. **Never** an AI/`Co-Authored-By`
  trailer; author stays the human git user.
- **Build cost:** `export CARGO_TARGET_DIR=~/.cache/alteria-target` (shared GPUI cache) before
  the first build so worktrees don't each rebuild GPUI.

## Prerequisites (verified)
- `rustc/cargo 1.92.0` ✓, Wayland ✓, **Vulkan instance 1.4.335** ✓. **Task 1 confirms a Vulkan
  *device* enumerates** when the window opens (instance ≠ device).

---

## Tasks (serial chain within this plan — one window before render before input)

> Each task: **Files** · **What/sketch** (illustrative GPUI — verify vs the pinned rev) ·
> **Verify** (run it; proven by running, not only `cargo test`) · **Commit**.

### Task 1 — Scaffold + pin GPUI + a window opens (de-risk the biggest unknown first)
**Files:** `Cargo.toml`, `Cargo.lock`, `crates/alteria-gpui/Cargo.toml`, `…/src/main.rs`
- Choose the rev now and **record it**: `git ls-remote https://github.com/zed-industries/zed HEAD`
  (or a recent tagged release). Write the exact 40-char SHA into `Cargo.toml` + the devlog; **do
  not float**.
  ```toml
  [package]
  name = "alteria-gpui"; version = "0.0.0"; edition = "2021"; license = "GPL-3.0-or-later"
  [dependencies]
  alteria-core = { path = "../alteria-core" }
  gpui = { git = "https://github.com/zed-industries/zed", rev = "<EXACT_SHA>" }
  ```
- Workspace `Cargo.toml`: add `"crates/alteria-gpui"` to `members`. **Recommended:**
  `default-members = ["crates/alteria-core"]` so a bare `cargo build`/`test` and the core's fast
  TDD loop don't drag in the giant GPUI compile (only `-p alteria-gpui` does). Verify vs your
  cargo version. *(Note: 003 will also append to `members`/`Cargo.lock` — expected hotspot.)*
- `main.rs`: smallest real window (verify the entry API vs the rev's `examples/` first):
  ```rust
  use gpui::*;
  fn main() {
      Application::new().run(|cx: &mut App| {
          cx.open_window(WindowOptions::default(), |_w, cx| cx.new(|_| RootView)).unwrap();
      });
  }
  ```
  (`unwrap` at startup is fine — not an event path; a failed window open should fail loudly.)
- **Verify:** with the shared `CARGO_TARGET_DIR`, `cargo run -p alteria-gpui` **opens a window**
  painting a background, and does **not** crash on device/surface creation (proves a Vulkan
  *device* on Wayland). If it crashes, solve that before anything else; capture the error.
- **Commit:** `chore: scaffold alteria-gpui (pinned gpui rev, window opens)`.

### Task 2 — `EditorElement`: render text + the primary cursor (read-only)
**Files:** `…/src/element.rs`, `…/src/app.rs`, (opt) `…/src/render.rs`, `main.rs`
- `app.rs`: a root view owning an `Editor` (static `Editor::new("hello\nworld\n…")` for now),
  rendering one `EditorElement` filling the window.
- `element.rs`: implement **`Element`** (`request_layout`→`prepaint`→`paint`). In `paint`, for
  each line: shape it with the window text system in a **monospace** font, derive line height from
  metrics, paint the shaped line; paint the **primary cursor**
  (`buffer.selection.primary().head`) as a 1–2px vertical quad (byte offset → x via the shaped
  line's index→x — verify the method on the rev). **Write our own** element (adapt GPUI's
  `text.rs`/`input.rs`, Apache; Zed's `EditorElement` GPL = approach only). Byte↔char: bridge with
  `text.byte_to_char(..)` at the rope edge, exactly as the core does.
- (opt) `render.rs`: extract offset↔pixel mapping as a **pure fn** and unit-test it.
- **Verify:** the window shows the text + a caret at offset 0 (screenshot if handy).
- **Commit:** `feat(gpui): editor element renders buffer text + primary cursor`.

### Task 3 — Keyboard pipeline (the crux): OS events → `InputEvent` → `handle` → repaint
**Files:** `…/src/input.rs`, `…/src/app.rs`
- **`input.rs` — pure translator, TDD test-first:**
  ```rust
  pub fn from_key_down(ev: &gpui::KeyDownEvent) -> Option<alteria_core::input::InputEvent>;
  pub fn from_key_up(ev: &gpui::KeyUpEvent) -> Option<alteria_core::input::InputEvent>;
  pub fn from_modifiers(ev: &gpui::ModifiersChangedEvent) -> alteria_core::input::InputEvent;
  pub fn modifiers(m: &gpui::Modifiers) -> alteria_core::input::Modifiers;
  ```
  - `gpui::Modifiers { alt, control, shift, platform/command, … }` → `Modifiers { alt, ctrl,
    shift, super_key }` (**verify gpui's field names on the rev**).
  - `KeyDownEvent` → `Char(c)` for a printable keystroke (Base layer must carry shift: `Shift+a`
    ⇒ `Char('A')`; modified layers don't care — the resolver lowercases), else
    `Backspace`/`Enter`/`Escape`, else `None`. Use the rev's character/`key_char` field for the
    printable vs the `key` name for named keys — `examples/input.rs` shows exactly how this rev
    exposes "the character typed" vs "the key pressed". Pass auto-`repeat` through.
  - **Unit tests (build the gpui structs in-test, no window):** `a`→`Char('a')`,NONE;
    `Shift+a`→`Char('A')`; Alt-held `w`→`Char('w')`,{alt}; bare `Backspace`/`Enter`/`Escape`;
    modifiers→`{alt}` → `ModifiersChanged{alt}`; unmapped key → `None`; alt/ctrl/shift/super
    field mapping correct.
- **`app.rs` — wire it:** `FocusHandle`, **focus on open**, attach `on_key_down`/`on_key_up`/
  `on_modifiers_changed` (raw) + **blur**. Each: translate → `Some(ev)` → `if
  self.editor.handle(ev) { cx.notify(); }`. Blur → `editor.handle(FocusLost)` (no stuck modifier).
- **Verify (the project's acceptance moment):** type `abc` inserts; `Backspace`/`Enter`/`Esc`
  work; **hold Alt → WASD move the caret, NOT type; release → typing lands where you navigated**;
  `Alt+5 s` jumps 5 lines; `Alt+f x` then `d`/`a` finds; `Ctrl+Z` undoes.
- **Commit:** `feat(gpui): raw key/modifier pipeline into Editor::handle (quasimode live)`.

### Task 4 — Render selections + all cursors (multicursor) + the highlight hook
**Files:** `…/src/element.rs`
- Paint a **highlight quad** behind every `Range` with `min != max` (multi-line: full middle
  lines, partial first/last). Paint **every** cursor in `selection.ranges`, the **primary**
  distinct. Add the **`highlights: Vec<(Range<usize>, gpui::Hsla)>`** field (default empty) and
  paint those colored spans **behind** the glyphs (or recolor glyphs) — this is the seam 003 fills
  via the Reviewer's adapter. Optional caret-blink timer.
- **Verify:** `Alt+Shift+d` shows a growing highlight; `I/O/U/P` expansion visible; `Alt+Ctrl+s`
  then typing → **two carets + two insertions**; `Esc` → one caret. Hand-set one `highlights`
  entry in a test/build and confirm a span renders colored (proves the seam).
- **Commit:** `feat(gpui): paint selections, multiple cursors, and the highlight hook`.

### Task 5 — Scrolling + viewport culling (the "feels instant" lever)
**Files:** `…/src/element.rs`, `…/src/app.rs`
- A **vertical scroll offset** on the view. On paint, **only shape + paint lines intersecting the
  viewport** (never shape the whole file). After a `handle` returning `true`, if the **primary
  cursor** is off-screen, scroll to reveal it. Wire **mouse-wheel** (`on_scroll_wheel` or rev's
  equivalent). (Horizontal/soft-wrap deferred; long lines may clip — note it.)
- **Verify:** open a buffer taller than the window (`include_str!` a real source file, or argv in
  Task 7); `Alt+s` past the bottom keeps the caret on-screen; wheel scrolls; a long buffer stays
  smooth (only visible lines shaped).
- **Commit:** `feat(gpui): viewport scrolling with cursor-follow and line culling`.

### Task 6 — Mouse: click→caret (hit-test), drag→select, wheel
**Files:** `…/src/element.rs`, `…/src/app.rs`, (opt) `…/src/render.rs`
- **Hit-testing** (the inverse of Task 2's offset→x): pixel → nearest byte offset (which line by
  y, which column by x within the shaped line — use the rev's `ShapedLine` index-for-x). On
  `MouseDown` set a bare cursor there (synthesize the selection directly on `buffer.selection`, or
  the cleanest path that respects the boundary — note: mouse has no `InputEvent` variant, so the
  frontend sets the selection on the buffer it owns; keep it minimal and documented). `MouseMove`
  while pressed → extend (anchor = down point, head = current). This realizes "no-Alt = an
  ordinary editor: click and drag to select."
- Keep the offset↔pixel math a **pure, unit-tested fn** in `render.rs`.
- **Verify:** click places the caret where you click; drag selects; selection paints; typing
  replaces it. With Vulkan/Wayland confirm coordinates account for any DPI/scale factor.
- **Commit:** `feat(gpui): mouse caret placement and drag-select (hit-testing)`.

### Task 7 — Clipboard (`Ctrl+C/X/V`) + file open (argv) / save (`Ctrl+S`)
**Files:** `…/src/clipboard.rs` (or inline in `app.rs`), `…/src/main.rs`, `…/src/app.rs`
- `main.rs`: read an optional file path from argv; `std::fs::read_to_string` → `Editor::new(&s)`
  (no path / unreadable → empty buffer + a warning, **never panic**). Store the path on the view.
- In the key handler, **intercept before routing** (see "Frontend-owned verbs"): `Ctrl+C` copy
  selection text → OS clipboard; `Ctrl+X` copy then send `Backspace`; `Ctrl+V` read clipboard →
  feed as `InsertChar` events; `Ctrl+S` write `buffer.text` to the path (`std::fs::write`). Verify
  the gpui clipboard API name on the rev. `Ctrl+Z` still routes to the engine.
- **Verify:** open a file via argv and see it; select + `Ctrl+C`, move, `Ctrl+V` pastes; `Ctrl+X`
  cuts; `Ctrl+S` writes to disk (confirm with `cat`); reopen shows the saved text.
- **Commit:** `feat(gpui): clipboard verbs and single-file open/save`.

### Task 8 — Latency sanity, boundary audit, polish, devlog
**Files:** small touch-ups; `devlog/002-gpui-frontend.md`
- **Latency:** informally confirm keystroke→paint *feels* instant on a normal file (debug-log
  handle/frame timing or watch GPUI frame stats — no benchmark harness; it's a feel check).
- **Boundary audit (non-negotiable):** `cargo tree -p alteria-core` shows **no `gpui`**;
  `grep -rn gpui crates/alteria-core/src` only finds the rule's doc-comments. `gpui` imported
  **only** in `alteria-gpui`; `alteria-syntax` **not** imported here (that seam is the Reviewer's).
- Window title `Alteria`; sensible size; `cargo fmt` / `clippy -D warnings` / `build` clean.
- **Devlog `002`:** the **exact pinned GPUI SHA**, every API that differed from these sketches
  (so 003+ is grounded in truth), the character-vs-key handling this rev needed, the
  cursor/selection/highlight paint approach, the hit-testing approach + DPI handling, whether
  `default-members` was used, measured/felt latency, and the empty `highlights` hook's shape for
  the Reviewer's adapter.
- **Commit:** `feat(gpui): latency check, boundary audit, polish` + `docs(devlog): add devlog 002`.

---

## Done criteria
- `cargo run -p alteria-gpui -- <file>` opens a window that **renders buffer + cursor(s) +
  selection**, driven by **real OS key/modifier/mouse events**, with **clipboard + open/save**.
- The **quasimode is physically real** (hold-Alt WASD, release-to-type-in-place; extend/count/
  find/expand/multicursor/undo visibly working); mouse click/drag selects; `Ctrl+C/X/V/S` work.
- `input.rs` (and any extracted offset↔pixel fn) are pure with passing unit tests; GUI verified
  by running.
- **Boundary holds:** `gpui` nowhere in `alteria-core`; `alteria-syntax` not imported here; the
  engine saw only `InputEvent`s and exposed only `buffer`.
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo build` clean. Exact pinned
  SHA + API deviations recorded in `devlog/002`.

## Self-review (before opening for review — mirrors `../CLAUDE.md`)
1. **Decoupled core:** no `gpui` in `alteria-core` (tree + grep); OS event → `InputEvent` only in
   `alteria-gpui`; engine read for `buffer`, driven by `handle`. ✓
2. **GPUI current, not remembered:** every API checked vs the **pinned rev**; deviations in the
   devlog. ✓
3. **Licensing:** GPUI (Apache) patterns adapted; **no Zed GPL editor source pasted**. ✓
4. **Tests:** pure `input.rs` (+ offset↔pixel) unit-tested test-first & green; window/paint
   verified by running (screenshot where it helps). ✓
5. **Clean build:** `fmt` / `clippy -D warnings` / `build` warning-free. ✓
6. **Edges / no stuck quasimode:** unmapped event → `None`; empty buffer; caret 0/EOF; off-screen
   selection; **focus-lost mid-hold → `FocusLost`**; missing file; empty clipboard. ✓
7. **Frontend boundary:** only data in = `InputEvent`, only data out = `&buffer`; mouse selection
   set on the owned buffer, documented. ✓

## Executor notes
- Worktree (e.g. `alteria_a1`). **Sync first — reset, never rebase:** `git fetch origin && git
  reset --hard origin/main && git clean -fd`. Then **`export
  CARGO_TARGET_DIR=~/.cache/alteria-target`** before the first build.
- Tasks are a **serial chain** — execute in order, commit per task, **push your own branch**
  (never `main`). Highest risk is **Task 1** (does GPUI build + open a window on this Wayland/
  Vulkan box) — do it first, don't proceed until a window reliably opens; if the pinned rev won't
  build, try the latest tagged Zed release and record which SHA worked.
- When a GPUI API here doesn't match the checkout, **the checkout wins** — adapt and note it.
- **You run in parallel with Plan 003.** Do **not** touch `alteria-syntax` or `alteria-core/src`.
  Build the `highlights` hook empty; the Reviewer wires 003's colors in at merge.
