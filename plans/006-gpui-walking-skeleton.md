# Plan 006 — GPUI walking skeleton: the first runnable Alteria window

**Goal:** Stand up `crates/alteria-gpui` — the **first frontend** — as a minimal vertical slice that
proves the whole concept end-to-end on real hardware: a window that renders the buffer + primary
cursor, and feeds raw OS key/modifier events into the existing `alteria-core` engine. The one bet
that has never been tested — **quasimodes driven by `ModifiersChangedEvent`, hold-Alt + WASD
navigation** — gets validated here. After five headless plans, this is where Alteria becomes a thing
you can *run*.

**This is a walking skeleton, not an editor.** The bar is "I can open it, type, hold Alt and move
with WASD, release Alt and type again, and `Ctrl+Z`." Polish is explicitly out of scope (see Non-goals).

**Status / deps:** Plans 001–005 are MERGED on `main`. The engine is feature-complete against the
*bound* `KEYMAP.md` layers (Base / Alt / Alt+Shift / Ctrl+Z), with the public facade ready:
`alteria_core::Editor::handle(InputEvent) -> bool` (redraw?) and `Editor::buffer` for reads. This
plan **consumes the core read-only through that facade** and aims for **zero `alteria-core` changes**.
006 is unblocked and runs **alone** (it touches the workspace manifest + `Cargo.lock`, the shared-state
merge hazards — no parallel sibling).

> **Sources of truth:** `../CLAUDE.md` (esp. "GPUI — read before you write" and "Architecture — the
> one hard rule"), `../KEYMAP.md`, and the **GPUI source the API must be verified against**:
> `../zed-main/crates/gpui/examples/` — authoritative. Read **before writing any GPUI code**:
> - `input.rs` — focus, custom text `Element` (`request_layout`/`prepaint`/`paint`),
>   `window.text_system().shape_line(...)`, `line.x_for_index(offset)`, `window.paint_quad(fill(...))`.
> - `hello_world.rs` / `window.rs` — minimal `application().run()` + `open_window` boilerplate.
> - `text.rs`, `uniform_list.rs`, `text_layout.rs` — multi-line text / shaping references.
>
> The agent's GPUI memory is **stale**; gpui here is `0.2.2`-era (toolchain `1.95.0`, post-Blade→wgpu).
> Confirm every API against the checkout — do not write gpui from memory.

> **⮕ Zed is the source of truth for GPUI usage too — not just the API.** The examples show *what
> compiles*; **Zed's own production code shows *how to use GPUI correctly* for exactly our problem.**
> Before (and while) writing the render/input layer, study how Zed's real editor does it and mirror its
> approach — this is the "Zed is the source of truth" hard rule applied to the frontend:
> - **`../zed-main/crates/editor/src/element.rs`** — `EditorElement`, the production text/cursor/selection
>   renderer. The closest analog to what T2/T3 build. Read: `layout_lines` (line shaping, ~2989),
>   `paint_text` (~5438), `paint_cursors` (~5674), and its `shape_line` / `paint_quad` call sites. **How
>   Zed shapes per-line text and positions cursors/selection quads is the pattern to reproduce** — don't
>   reinvent text layout.
> - **`../zed-main/crates/editor/src/editor.rs`** — focus/blur wiring (`cx.on_focus` / `on_focus_in` /
>   `on_focus_out` / `on_blur`, ~2047–2053) and how the editor view is structured around a `FocusHandle`.
>   Mirror this for our focus + `FocusLost` handling.
> - For later plans (not this slice): `editor/src/clipboard.rs`, `editor/src/input.rs`, `editor/src/movement.rs`.
>
> Whatever Zed does to make its editor render and feel right, we do that. Cite the Zed file you mirrored
> in `devlog/006` for each non-trivial GPUI decision.

### Verified API facts (confirmed against `../zed-main` while planning — still re-check at execution)
- **Entry point moved:** `use gpui_platform::application;` then `application().run(|cx: &mut App| { … })`.
  It is **not** the remembered `App::new().run()`. `application()` returns `gpui::Application`.
  So depend on **both** `gpui` and `gpui_platform` (same pinned rev).
- **Raw quasimode handlers exist** on interactive elements (`div()`):
  `on_key_down(|&KeyDownEvent, &mut Window, &mut App|)`, `on_key_up(|&KeyUpEvent, …|)`,
  `on_modifiers_changed(|&ModifiersChangedEvent, …|)`. **Use these — never gpui Actions/`KeyBinding`**
  (the Action system can't express "a modifier is held with no key"; CLAUDE.md hard rule).
- **Event structs** (`gpui::{KeyDownEvent, KeyUpEvent, ModifiersChangedEvent}`):
  - `KeyDownEvent { keystroke: Keystroke, is_held: bool, prefer_character_input: bool }` — `is_held` ⇒ core's `repeat`.
  - `KeyUpEvent { keystroke: Keystroke }`.
  - `ModifiersChangedEvent { modifiers: Modifiers, capslock: Capslock }`.
- **`Keystroke { modifiers: Modifiers, key: String, key_char: Option<String> }`** — `key` is the physical
  key label (e.g. `"w"`, `"backspace"`, `"escape"`); `key_char` is the character that would be typed
  (e.g. `"ß"` for alt-s, `None` for cmd-s).
- **`Modifiers { control, alt, shift, platform, function }`** → core `Modifiers { ctrl, alt, shift, super_key }`
  (`platform`→`super_key`; ignore `function`/`capslock`).
- Focus is required to receive key events: a root `div().track_focus(&handle)` + `window.focus(&handle, cx)`.

## Files this plan owns
```
crates/alteria-gpui/Cargo.toml          # new bin crate; pin gpui + gpui_platform git rev
crates/alteria-gpui/src/main.rs         # application().run() + open_window + root view; argv file load
crates/alteria-gpui/src/event.rs        # gpui KeyDown/KeyUp/ModifiersChanged/focus → core InputEvent
crates/alteria-gpui/src/view.rs         # the root Render view: holds Editor, wires raw handlers → handle()
crates/alteria-gpui/src/text_element.rs # custom Element: paint buffer lines + primary cursor/selection
Cargo.toml                              # [workspace] members += alteria-gpui; add default-members (engine only)
Cargo.lock                              # gpui + transitive deps (shared-state hazard — this plan only)
devlog/006-gpui-walking-skeleton.md     # the validation record (incl. the pinned SHA + Vulkan setup)
```
(Module split is a suggestion — keep `main.rs` thin. **No `alteria-core` files.** If a tiny **additive,
read-only** facade accessor turns out to be unavoidable for rendering, this plan may add it to
`alteria-core/src/{buffer.rs,lib.rs}` — additive only, no logic change — and must note it; but the
target is zero engine changes.)

## Tasks

> **Before writing any GPUI code (all tasks): read both the gpui *examples* AND how Zed's production
> `editor` crate uses GPUI** (see the "Zed is the source of truth for GPUI usage too" block above).
> Reproduce Zed's approach; verify every API against the pinned rev. This is the highest-risk plan for
> hallucinated/stale APIs — grounding in Zed's real usage is the mitigation.

### T0 — Crate scaffold + workspace wiring + GPUI pinned; an empty window opens
*(Do the environment setup first — GPUI's first compile is huge; get it building before any logic.)*
- **Before building:** `export CARGO_TARGET_DIR=~/.cache/alteria-target` (one shared cache, per CLAUDE.md
  — don't let this branch rebuild GPUI from scratch). Verify Vulkan: `vulkaninfo | head`. Arch deps:
  `vulkan-icd-loader libxkbcommon wayland fontconfig` + your GPU's Vulkan driver
  (`vulkan-radeon`/`vulkan-intel`/`nvidia-utils`).
- New `crates/alteria-gpui` **binary** crate. `Cargo.toml`:
  - `alteria-core = { path = "../alteria-core" }`.
  - `gpui = { git = "https://github.com/zed-industries/zed", rev = "<SHA>" }` and
    `gpui_platform = { git = "…", rev = "<same SHA>" }`. **Pin a concrete recent `main` SHA** (the
    `../zed-main` snapshot is gpui `0.2.2` / toolchain `1.95.0` / post-wgpu, ~2026-Q2). Confirm the
    **required gpui features** against `../zed-main/crates/gpui/Cargo.toml` (Linux windowing/wgpu) and
    enable what's needed — do not guess. **Record the exact SHA in `devlog/006`.** Cross-check the
    built API against `../zed-main`'s examples; if they drift, pin the SHA whose tree matches the snapshot.
  - Match Zed's `rust-toolchain` expectations (1.95.0); if our stable is older and gpui needs edition
    2024 / a newer rustc, note it (this may force a toolchain bump — flag, don't silently pin nightly).
- **Workspace manifest** (`Cargo.toml`): add `crates/alteria-gpui` to `members`, and add
  `default-members = [ <the 7 engine crates> ]` (everything **except** alteria-gpui). Rationale: the
  engine TDD loop (`cargo test`, `cargo build`, `cargo clippy` with no `-p`) must stay GPUI-free and
  fast — the green-fast guarantee from 005's T0. `cargo run -p alteria-gpui` builds the frontend
  explicitly. (`cargo test --workspace` will *compile* alteria-gpui; that's acceptable — it has no/min tests.)
- `main.rs`: the minimal `application().run(|cx| { cx.open_window(WindowOptions::default(), |_, cx| { … }) })`
  from `hello_world.rs`/`window.rs`, rendering a placeholder. **Verify it opens a window.**
- **Verify:** `cargo run -p alteria-gpui` opens a window; `cargo test` (default-members) still green & fast;
  `cargo build -p alteria-gpui` clean.
- **Commit:** `feat(gpui): scaffold alteria-gpui crate; pin GPUI rev; empty window opens`.

### T1 — Event translation: gpui events → core `InputEvent` (`event.rs`)
- Translate, per the verified facts above:
  - `ModifiersChangedEvent` → `InputEvent::ModifiersChanged { mods }` — **the primitive the whole concept
    rides on.** Map `Modifiers { control, alt, shift, platform }` → core `{ ctrl, alt, shift, super_key }`.
  - `KeyDownEvent` → `InputEvent::KeyDown { key, mods, repeat: is_held }`.
  - `KeyUpEvent` → `InputEvent::KeyUp { key, mods }`.
  - window blur / focus loss → `InputEvent::FocusLost` (so the resolver clears `held` — no stuck quasimode).
- **`Keystroke` → core `Key` (the subtle bit — get this right, it's the crux of the layering):**
  - **Base layer (no ctrl/alt/super):** prefer `key_char` for the actual character to insert (handles
    shift→uppercase, symbols, dead keys) → `Key::Char(c)`.
  - **Modifier layers:** use `keystroke.key` (the physical label, e.g. `"w"`) → `Key::Char('w')`, because
    the resolver maps `(held_mods, Key::Char('w'))` for Alt-WASD/QE/etc. — read `keymap.rs` to see exactly
    which `Key`s each layer expects, and match them.
  - Named keys from `keystroke.key`: `"backspace"`→`Key::Backspace`, `"enter"`/`"return"`→`Key::Enter`,
    `"escape"`→`Key::Escape`. Unmapped keys → drop (return `None`).
- **Factor the pure mapping out** so it's unit-testable without constructing gpui types: a private fn over
  plain inputs (`key: &str`, `key_char: Option<&str>`, a plain mods struct) → `Option<(Key, core::Modifiers)>`.
  Test it directly. (The gpui-typed wrapper just unpacks `Keystroke`/`Modifiers` and calls it.)
- **Tests:** `'a'` Base → `Char('a')`; Shift+`a` → `Char('A')` via key_char; Alt+`w` → `Char('w')` via key;
  `"backspace"`/`"enter"`/`"escape"` → their variants; modifier mapping (`platform`→`super_key`).
- **Commit:** `feat(gpui): translate gpui key/modifier/focus events into core InputEvent`.

### T2 — Render the buffer + primary cursor (`text_element.rs`)
- **First, study `../zed-main/crates/editor/src/element.rs`** (`EditorElement::layout_lines` /
  `paint_text` / `paint_cursors`) — this is Zed's production answer to exactly this task. Mirror its
  line-shaping and cursor/selection-quad approach; the `input.rs` example is the *minimal* form of the
  same pattern. Don't invent a text-layout scheme.
- A custom `Element` (mirror `input.rs`'s `TextElement`: `request_layout`/`prepaint`/`paint`), extended to
  **multiple lines**. For each visible line: `window.text_system().shape_line(line_text, font_size, &runs, None)`,
  paint at `y = row * line_height`. Place the **primary cursor** with `shaped_line.x_for_index(byte_col)`
  and a thin `fill(Bounds…)` quad (as in `input.rs`). Paint the primary **selection** span as a translucent
  quad if it's easy; multi-line selection rectangles can be a follow-up.
- Read engine state read-only via the facade: `editor.buffer.text()` (split on `'\n'`) for lines, and
  `editor.buffer.primary_resolved()` (a `Selection<usize>` with byte offsets) for the cursor/selection;
  derive `(row, col)` by counting `'\n'` before the offset (skeleton-simple — no need to pull in
  `rope`/`text`). Monospace font; fixed size. **No scrolling/viewport** (paint all lines; the test files are tiny).
- **Verify:** the window shows the buffer text with a visible cursor at offset 0.
- **Commit:** `feat(gpui): paint buffer lines + primary cursor via shaped lines`.

### T3 — Wire the loop: raw handlers → `Editor::handle` → redraw (`view.rs` + `main.rs`)
- **Reference `../zed-main/crates/editor/src/editor.rs` focus wiring** (`cx.on_focus`/`on_focus_out`/
  `on_blur`, ~2047–2053) for how Zed structures a focused editor view and reacts to focus loss — mirror
  it for our `FocusLost` path. (We diverge only in routing keys through raw `on_key_*` to the core
  instead of Zed's Action system — the documented quasimode exception.)
- The root view holds `Editor` (`alteria_core::Editor::new(text)`), a `FocusHandle`, and the
  `TextElement`. In `render`: `div().track_focus(&self.focus).on_key_down(cx.listener(…)).on_key_up(…)
  .on_modifiers_changed(…)`, each translating (T1) and calling `let redraw = self.editor.handle(ev);
  if redraw { cx.notify(); }`. On window blur, feed `FocusLost`.
- Focus the root on open (`window.focus(&handle, cx)`) so keys arrive; `cx.activate(true)`.
- **Content source:** load `argv[1]` if present (read the file to a `String`; `Buffer`/`text` already
  normalizes line endings on load), else a built-in scratch string. Read-only display is fine — no save.
- **Verify (the validation moment):** `cargo run -p alteria-gpui some.txt` →
  - type printable chars → they insert; `Backspace`/`Enter` work (Base layer, no modifier).
  - **hold Alt + W/A/S/D → the cursor moves** (up/down/left/right); `Q`/`E` word-jump; `Z`/`C` line edges.
  - **release Alt → typing resumes instantly** (the no-stuck-mode guarantee).
  - `Alt+Shift` + motion **extends** the selection; `Esc` drops it; `Ctrl+Z` undoes (text *and* expansions).
- **Commit:** `feat(gpui): feed raw key/modifier events to the engine; live redraw`.

### T4 — Validate the bet + `devlog/006`
- **The core question this plan exists to answer:** does `ModifiersChangedEvent` actually fire on the
  author's session (Wayland and/or X11) independent of other keys, so hold-Alt is detectable? **Record the
  answer** in `devlog/006` — this de-risks (or redirects) everything downstream.
- Devlog must capture: the pinned gpui SHA + enabled features; the Vulkan/driver setup that worked; the
  `key`/`key_char`/modifier mapping decisions; whether quasimode hold-Alt+WASD worked end-to-end; any GPUI
  API surprises vs. the agent's priors; and any unavoidable additive core accessor (there should be none).
- **Verify:** `cargo test` (default-members) green & fast; `cargo build -p alteria-gpui` + `cargo clippy
  -p alteria-gpui` clean; `cargo tree -p alteria-core` still shows no `gpui`/`ropey` (the core stayed pure).
- **Commit:** `docs(devlog): record 006 — first runnable window + quasimode validation`.

## Done criteria
- `crates/alteria-gpui` exists, pinned to a recorded GPUI SHA, and `cargo run -p alteria-gpui [file]`
  **opens a window** rendering the buffer + cursor.
- Base typing + `Backspace`/`Enter`; **hold-Alt + WASD navigation works**; `Alt+Shift` extends; `Esc`
  drops; `Ctrl+Z` undoes; **releasing Alt returns to typing with no stuck mode.**
- All GPUI lives in `alteria-gpui`; `alteria-core` saw **zero** `gpui` types (`cargo tree` clean) — the
  one hard rule intact. Quasimode logic uses raw `on_key_*`/`on_modifiers_changed`, **never** gpui Actions.
- Engine commands (`cargo test`/`build`/`clippy` on default-members) stay GPUI-free, fast, and green.
- `devlog/006` records the `ModifiersChangedEvent` validation result + the SHA + the Vulkan setup.

## Design decisions to confirm with the maintainer (before/while executing)
1. **Workspace layout:** alteria-gpui as a `members` entry with `default-members` = engine crates only,
   so day-to-day engine commands stay GPUI-free/fast. (Recommended.)
2. **Content source:** `argv[1]` file with a built-in scratch-string fallback; **read-only** (no save in
   this slice). (Recommended.)
3. **GPUI rev:** pin a concrete recent `zed-industries/zed` `main` SHA, verified against `../zed-main`;
   record it in the devlog. If gpui requires edition 2024 / rustc 1.95, we bump the toolchain deliberately
   (flag it — don't reach for nightly silently). (Recommended.)
4. **Render scope:** custom per-line `Element` via `shape_line`, paint all lines (no scroll/viewport),
   primary cursor always, selection quad if cheap. Syntax highlighting, scrolling, multi-cursor rendering,
   and mouse selection are deferred. (Recommended.)

## Non-goals (explicitly out of scope — keep the slice minimal)
Syntax highlighting · scrolling / viewport / large-file perf · 120fps · config / keymap files · multiple
buffers / tabs · file **save** & dirty tracking · mouse selection & clipboard (OS clipboard) · rendering
of non-primary cursors · gutter / line numbers · IME/marked-text. These come in later plans; this one only
has to **prove the loop runs and the quasimode bet holds.**

## Notes
- **Read the real GPUI API first** (`../zed-main/crates/gpui/examples/{input,hello_world,window,text,uniform_list}.rs`).
  The entry is `gpui_platform::application()`, not `App::new()`. Verify everything; the API churns weekly.
- **Quasimodes never go through gpui's Action/`KeyBinding` system** — raw `on_key_down`/`on_key_up`/
  `on_modifiers_changed` only. Bare-modifier detection rides `ModifiersChangedEvent`.
- **One shared target cache** (`CARGO_TARGET_DIR=~/.cache/alteria-target`) before the first build — GPUI's
  initial compile is huge; don't pay it twice.
- The decoupled core is the safety net: if a GPUI API broke or the rev won't build, it's contained to this
  crate — the engine is untouched, and Floem stays a viable fallback frontend.
