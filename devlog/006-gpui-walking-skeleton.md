# Devlog 006 — GPUI walking skeleton: the first runnable Alteria window

**Date:** 2026-06-02
**Plan:** `plans/006-gpui-walking-skeleton.md`
**Branch:** `alteria_a1`
**Status:** ✅ Complete — **the bet holds.** `crates/alteria-gpui` is the first frontend: it
opens a real GPUI window, renders the buffer + primary cursor/selection, and pumps raw
OS key/modifier events into the existing `alteria-core` engine through `Editor::handle`.
Build/test/clippy/fmt all clean; the window opens on this Wayland session (Intel/Mesa/wgpu).
**Hands-on, all four validation steps pass: `ModifiersChangedEvent` fires on the author's
Wayland session independent of other keys, so hold-Alt is detectable — hold-Alt + WASD
navigates, releasing Alt returns to typing instantly, Alt+Shift extends, Esc drops, Ctrl+Z
undoes.** The quasimode model the whole project rides on works end-to-end on real hardware
(see §9).

**Zero `alteria-core` changes.** The plan allowed a tiny additive read-only accessor if
rendering forced one; none was needed. The existing facade (`Editor::handle`,
`Editor::buffer`, `buffer.text()`, `buffer.primary_resolved()`) was sufficient.
`cargo tree -p alteria-core` still shows **no gpui, no ropey** — the one hard rule holds.

---

## 1. The pinned GPUI rev (and why this exact SHA)

```
gpui          = { git = "https://github.com/zed-industries/zed", rev = "1dba7a28bb19ea2f3817ad7ed63a0fcb25d820d2" }
gpui_platform = { git = "…", rev = "1dba7a28bb19ea2f3817ad7ed63a0fcb25d820d2", features = ["wayland", "x11"] }
```

- **SHA `1dba7a28bb19ea2f3817ad7ed63a0fcb25d820d2`** (gpui `0.2.2`, post-Blade→wgpu).
- This SHA is **the same tree as the `../zed-main` snapshot** the API was verified against:
  diffing the cached git checkout against `../zed-main` shows `gpui/examples/input.rs`,
  `gpui/examples/window.rs`, and `gpui_platform/Cargo.toml` are byte-identical (the snapshot
  only adds a later `a11y` example + `accesskit` line to `gpui/Cargo.toml`). So every API I
  read from `../zed-main` is exactly what compiled.
- **Features:** `gpui` takes its defaults (`font-kit, wayland, x11, windows-manifest`);
  `gpui_platform` needs `wayland` + `x11` so the Linux backend (`gpui_linux`) gets a windowing
  impl. This matches what actually compiled (verified against the build fingerprints).
- The entry point is **`gpui_platform::application()`**, not the older `App::new()` — both
  crates are required at the same rev.

## 2. Toolchain: no bump needed (the plan's open question, answered)

The plan flagged that Zed pins `rust-toolchain.toml` to **1.95.0 / edition 2024** and that gpui
might force a deliberate toolchain bump. **It did not.** A git/path dependency is built with the
*outer* workspace's toolchain (a dependency's `rust-toolchain.toml` does not apply), and gpui at
this SHA compiles cleanly on our installed **stable `rustc 1.92.0`** (edition 2024 was stabilized
in 1.85, so 1.92 handles gpui's 2024-edition crates). The whole gpui tree + our frontend built
green on 1.92.0 — no nightly, no bump. (If a future SHA needs ≥1.95, that's a deliberate
`rustup update`, flagged then.)

## 3. Vulkan / driver setup that worked

- GPU: **Intel(R) Graphics (ARL)**, **Mesa 25.3.3** (`vulkan-intel`), Vulkan API 1.4.
- Session: **Wayland** (`wayland-1`; X11 `:1` also present). `XDG_SESSION_TYPE=wayland`.
- Arch deps present: `vulkan-icd-loader libxkbcommon wayland fontconfig` + `vulkan-intel`.
- wgpu (gpui's post-Blade backend) picked Vulkan and the window came up with no
  NVIDIA/Wayland-class crash. `vulkaninfo | head` confirms the ICD loads.

## 4. The build-cost win (shared cache)

Per CLAUDE.md, all builds used `CARGO_TARGET_DIR=~/.cache/alteria-target`. gpui's huge first
compile was **already paid** in that shared cache from an earlier spike (the zed git dep was also
already fetched into `~/.cargo/git`), so this slice's builds were **cache hits**: ~3s to compile
just `alteria-gpui` and link. The engine `default-members` loop (no `-p`) never touches gpui.

## 5. Module layout (what each file does)

```
crates/alteria-gpui/
  Cargo.toml        # bin crate; pinned gpui + gpui_platform; deps: alteria-core only
  src/main.rs       # application().run() + open_window(EditorView); argv[1] file or scratch
  src/event.rs      # gpui KeyDown/KeyUp/ModifiersChanged → core InputEvent (pure mapping + tests)
  src/view.rs       # root Render view: holds Editor + FocusHandle; raw handlers → Editor::handle
  src/text_element.rs # custom Element: shape lines, paint cursor + selection quads
```
Workspace `Cargo.toml`: `members += alteria-gpui`; `default-members` = the 7 engine crates, so
`cargo test`/`build`/`clippy` (no `-p`) stay GPUI-free and fast (the 005 green-fast guarantee).

## 6. The crux: `Keystroke → Key` and the modifier mapping (`event.rs`)

The subtle part of the frontend is translating a GPUI `Keystroke` into the core's `Key`, and it
is **factored into a pure fn over `&str`/plain mods** so it is unit-tested without constructing any
gpui value (9 tests; the gpui-typed wrappers just unpack `Keystroke`/`Modifiers` and call it).

- **Modifiers:** gpui `{ control, alt, shift, platform, function }` → core
  `{ ctrl, alt, shift, super_key }`. `platform` (cmd/win/**super**) maps to `super_key`;
  `function` and capslock are dropped.
- **Base layer** (no Alt/Ctrl/Super — Shift alone stays Base): prefer **`key_char`**, the character
  that would actually be typed, so `Shift+a → 'A'`, symbols, and dead keys insert correctly. Fall
  back to the physical keycap if there's no `key_char`.
- **Modifier layers:** use the physical **`key`** label (`"w"`), *ignoring* `key_char` — Alt+S types
  `"ß"` on some layouts, but the resolver dispatches on `(held_mods, Key::Char('s'))`, and Alt+digit
  must read the digit keycap, not a composed symbol. The resolver lowercases ASCII for the modified
  layers, so case is handled there.
- **Named keys** (any layer): `backspace → Backspace`, `enter`/`return` → `Enter`, `escape → Escape`.
  Unmapped/multi-char keys (`f1`, `tab`) → `None` (dropped, no redraw).

The resolver consumes the `KeyDown`'s own `mods` for layer selection (it re-sets `held = mods`
each key) and uses `ModifiersChanged`/`FocusLost` to clear Alt-gated count/find state — so the
frontend forwards **all four**: `ModifiersChanged`, `KeyDown {…, repeat: is_held}`, `KeyUp`, and
`FocusLost` (fed on `cx.on_blur`, mirroring Zed's editor focus wiring → the no-stuck-quasimode
guarantee).

## 7. Rendering (`text_element.rs`) — mirrors gpui `input.rs` / Zed `EditorElement`

A custom `Element` (`request_layout`/`prepaint`/`paint`), built exactly on gpui's own
`examples/input.rs` `TextElement` but extended from one line to N — which is how Zed's production
`editor/src/element.rs` lays out text: **shape each line** with
`window.text_system().shape_line(line, font_size, &runs, None)`, paint it at `y = row * line_height`,
place the caret with `shaped_line.x_for_index(byte_col)` as a thin `fill(...)` quad, and draw the
primary selection as one translucent quad per covered row. `(row, col)` is derived by counting
`\n`s in `buffer.text()` (rope already normalized line endings on load), so **no `rope`/`text`
type leaks into the frontend** — the engine hands over a byte offset and a `String`, nothing more.
Skeleton scope held: no scrolling/viewport, no syntax runs, primary cursor/selection only, caret
shown only while focused (Zed gates it the same way). 5 `row_col` unit tests cover start / mid-line
/ after-newline / trailing-empty-line / at-the-break.

### GPUI API notes vs. the agent's stale priors
- Entry is `gpui_platform::application().run(|cx: &mut App| …)`; `cx.new(…)` needs the
  **`AppContext`** trait in scope (the one compile error, fixed by importing it in `main.rs`).
- Raw quasimode handlers are real and fluent on `div()`:
  `on_key_down(impl Fn(&KeyDownEvent, &mut Window, &mut App))`, `on_key_up`, `on_modifiers_changed`
  — used via `cx.listener(Self::method)`. **No** gpui Action/`KeyBinding` anywhere (the documented
  quasimode exception). `ModifiersChangedEvent { modifiers, capslock }` carries the bare-modifier
  state the whole concept rides on.
- `open_window(opts, |window, cx| cx.new(|cx| EditorView::new(text, window, cx)))`; focus via
  `window.focus(&handle, cx)` + `cx.activate(true)`.

## 8. Verification

| Check | Result |
|---|---|
| `cargo build -p alteria-gpui` | clean (gpui cache hit, ~3s) |
| `cargo test -p alteria-gpui` | **14 passed** (9 event-mapping + 5 row_col) |
| `cargo test` (default-members) | **alteria-core 152 passed**, GPUI-free, <1s |
| `cargo clippy -p alteria-gpui --no-deps` | clean (no warnings) |
| `cargo fmt -p alteria-gpui -- --check` | clean |
| `cargo tree -p alteria-core \| grep -iE gpui\|ropey` | empty — **core stayed pure** |
| Runtime: window opens (Wayland/Intel/wgpu) | ✅ process stays alive, no startup crash |

(Vendored `rope`/`sum_tree` show pre-existing whole-file `fmt` deltas on `main`; those are verbatim
Zed code and are intentionally not reformatted.)

## 9. The bet — hands-on validation (the reason 006 exists)

The runtime question 006 was built to answer — *does `ModifiersChangedEvent` fire on the author's
Wayland session independent of other keys, so hold-Alt is detectable?* — is a **human-in-the-loop**
check: it needs eyes on a window and fingers on the keys, which the executor can't do. The window
**does open and render** (verified above); the interaction checklist below is the maintainer's to
run, and the result is recorded here:

Run: `CARGO_TARGET_DIR=~/.cache/alteria-target cargo run -p alteria-gpui [file]`

1. Type printable chars → they insert; `Backspace`/`Enter` work (Base layer, no modifier).
2. **Hold Alt + W/A/S/D → the cursor moves** (up/left/down/right); `Q`/`E` word-jump; `Z`/`C` line edges.
3. **Release Alt → typing resumes instantly** (no stuck mode).
4. `Alt+Shift` + a motion **extends** the selection; `Esc` drops it; `Ctrl+Z` undoes.

**Result (maintainer's run, 2026-06-02):** ✅ **All four pass — the bet holds.** Hold-Alt is
detected (so `ModifiersChangedEvent` *does* fire on this Wayland session independent of other
keys); hold-Alt + WASD navigates the cursor, `Q`/`E`/`Z`/`C` jump; releasing Alt resumes typing
instantly with no stuck mode; Alt+Shift + motion extends the selection; `Esc` collapses it; `Ctrl+Z`
undoes. The five headless plans and the held-modifier quasimode model are validated end-to-end on
real hardware — Alteria is now a thing you can run.

## 10. Non-goals kept out (later plans)

Syntax highlighting · scrolling/viewport/large-file perf · 120fps · config/keymap files ·
tabs/multiple buffers · file **save** & dirty tracking · OS clipboard · non-primary cursor
rendering · gutter/line numbers · IME/marked-text. 006 only had to prove the loop runs and stand up
the window; it does.
