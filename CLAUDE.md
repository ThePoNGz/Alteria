# Workflow (parallel agents)
I sometimes will run multiple claude code instances.

Three roles, one source of truth: `origin/main`.

- **Planner** (`/planner`, on `main`): split work into numbered, **non-overlapping** plan files in `plans/` (001, 002, …) — no two touch the same files. Commit, push `origin main`. Writes no feature code.
- **Executor** (`/executor`, in worktree branch `feature/aN`): start every task by discarding obsolete branch state and syncing to main — **reset, never rebase**: `git fetch origin && git reset --hard origin/main && git clean -fd`. Execute one plan, write a matching `devlog/NNN` entry (same number as the plan), then **commit AND push** to your own branch. Never push `main`. (Unpushed = invisible = doesn't exist.)
- **Reviewer** (fresh agent on `main`): merge each `feature/aN` into main, resolve conflicts, review against the rules + Self-Review, push `main`.

**Shared-state merge hazards** (route through the Planner; keep out of unrelated plans):

- `Cargo.lock` + workspace `Cargo.toml` (`members` / `[dependencies]`) — dep additions on two branches conflict.
- `crates/alteria-core/src/lib.rs` — every new module adds a `pub mod` line, so it's a merge hotspot. The Reviewer owns the final `pub mod` list.
- **Build cost:** GPUI's first compile is huge/slow. Point all worktrees at one cache (`export CARGO_TARGET_DIR=~/.cache/alteria-target`, or `sccache`) so branches don't each rebuild GPUI.


# Alteria

Keyboard-centric, **non-modal** code editor in Rust, built on **quasimodes** — states active only while a modifier key is physically held; release returns instantly to typing, so you can never get stuck in a mode. With **no modifier held it is an ordinary editor** — type, mouse-select, `Ctrl+C/V/X/Z/S` — so anyone can use it on day one; the Alt layer is an additive power-up, never a prerequisite. Defining interaction: **hold-Alt + WASD** inverted-T navigation. **GUI app, not a terminal app** (held-modifier model is incompatible with TTYs).

**Context lives in two files only: this file + `KEYMAP.md` (the keystroke spec).** Don't create or rely on other doc files. The keymap is the product — `KEYMAP.md` is the source of truth for every binding; this file is everything else.

## Concept

- **Quasimode** (Raskin's term): a state that exists only while a modifier is *held* — not a mode you toggle into (vim Normal, Caps Lock). Release → instantly back to typing. No stuck modes, ever.
- **Typing is the default.** No Insert mode. Keys type unless a modifier is held.
- **No Alt held = an ordinary editor.** This is as core as the quasimodes. Don't touch Alt and Alteria behaves ~98% like VS Code / Notepad / Word: type to insert, click-drag (and `Shift`+arrows) to select, `Ctrl+C`/`X`/`V`, `Ctrl+Z`/`Ctrl+Y`, `Ctrl+S`, `Home`/`End`/`PageUp`/`Down` — every convention your fingers already know. The Alt-held magic is *additive*; nothing standard is taken away. You can sit down, `Ctrl+S`, and leave, never learning a single Alt binding. The anti-goal is vim: no mode to learn before you can type or select, and no way to get stuck. **Accessibility first.**
- **Hold Alt + WASD** = inverted-T navigation (the bet — ergonomic alternative to vim hjkl).
- **Bilateral workflow:** hold the modifier with the hand opposite the nav keys.
- **Anchor/Head selection:** every cursor is `Range { anchor, head }` (byte offsets); `anchor == head` ⇒ bare cursor, else the span is selected.
- **Multicursor** = `Vec<Range>`; one edit applies to all.
- **Why GUI not terminal:** TTYs have no key-up/down events and lossy modifier combos (`Ctrl+S`=XOFF), so "while held" literally can't exist there. Kitty protocol fixes modern emulators but not a bare TTY — killing the universality that was the only reason to be a terminal app. A native window gets real OS key-down/up + every modifier. Vim's modal controls exist *because* of the terminal limitation; we reject both.

## Goals / Non-goals

- **Goal:** a daily driver for the author first; good enough that others *could* adopt it.
- **Goal: accessible by default** — usable immediately by someone who never reads the keymap. The Alt layer is a power-up, not a prerequisite; standard editor conventions (mouse select, `Ctrl+C/V/X/Z/S`, …) work with no modifier held.
- **Goal:** "feels instant on the files I actually open" — **not** Zed's huge-file/120fps engineering. The latency metric that matters is keystroke→paint (single-digit ms).
- **Goal:** free & open source (GPL-3.0).
- **Non-goal (for now):** Zed-tier rendering/perf — an eventual aspiration, but today's bar is "feels instant on my files", not 120fps/huge-file engineering. Also out of scope: collaboration, terminal/TTY support, and building for hypothetical contributors (stay modular so the door stays open at zero cost).

## Architecture — the one hard rule

**`alteria-core` is pure Rust and NEVER imports `gpui`.** The engine (buffer, cursors, selections, keymap, the input→intent→action pipeline) knows nothing about rendering. The GPUI frontend only translates OS events into plain-data `InputEvent`s and renders engine state.

Why non-negotiable: GPUI ships **2–3 breaking builds/week** and periodically re-architects its platform layer. A frontend-agnostic core means GPUI churn never touches engine logic, and keeps **Floem** a low-cost fallback. Reaching for a `gpui` type inside `alteria-core` = the rule breaking.

Pipeline — every stage a pure function over plain data, unit-testable without a window:

```
InputEvent → Resolver(held_mods) → Action → Executor → Buffer (+ Selection)
```

The frontend does only the first translation (OS event → `InputEvent`) and the last render (read `Buffer` → draw).

**Module responsibilities (`alteria-core`, one concept per file):**

- `input` — `InputEvent` enum + plain-data `Key`, `Modifiers`. No GUI types.
- `action` — `Action` enum (intent, decoupled from keys) + `Direction`.
- `keymap` — `Keymap { layers: HashMap<Modifiers, Layer> }`, `Layer { bindings: HashMap<Key, Action> }`. Held modifiers select the active layer. Declarative/remappable.
- `resolver` — the state machine. Holds `held` mods (the only mutable input state), updated by modifier/focus events. Resolves `(held, key) → Action`.
- `selection` — `Range { anchor, head }` + `Selection { ranges: Vec<Range>, primary }`.
- `buffer` — ropey `Rope` + current `Selection`.
- `executor` — applies one `Action` to the `Buffer`.
- `lib` (`Editor` facade) — ties resolver + keymap + buffer; exposes `handle(InputEvent) -> bool` (redraw?), the single entry point the frontend calls.

## Stack

| Layer | Choice |
|---|---|
| Language | Rust (edition 2021, `rustup default stable`) |
| UI / windowing / input | GPUI — **pinned git rev** of `zed-industries/zed`, wgpu backend |
| GPU render + text shaping/raster | handled by GPUI (cosmic-text stack) |
| Text buffer | ropey **1.6.x** (not the 2.0 beta — it switches to byte-indexing) |
| Coordinate model | store byte offsets; move by grapheme clusters; derive visual columns |
| Undo/redo | transaction/changeset + revision-tree history, anchor-based |
| Syntax highlighting | tree-sitter |
| Config / keymap file | declarative, serialized `Keymap` |
| License | GPL-3.0-or-later |

- **Pin GPUI to a specific SHA** in `crates/alteria-gpui/Cargo.toml`. Don't float the rev — bump deliberately and rebuild. The published `gpui` crate (0.2.2) is stale; use the git rev.
- GPUI needs a **Vulkan-capable GPU**. Arch deps: `vulkan-icd-loader libxkbcommon wayland fontconfig` + your GPU's Vulkan driver (`vulkan-radeon` / `vulkan-intel` / `nvidia-utils`). Verify with `vulkaninfo | head`.
- wgpu auto-picks Vulkan/Metal/DX12; GPUI migrated Blade→wgpu (~Zed 0.12, Mar 2026), fixing the old NVIDIA/Wayland crashes.

## GPUI — read before you write

The agent's training knowledge of GPUI is thin and **stale**; the API moves weekly. Writing from memory produces hallucinated APIs that won't compile.

1. **Before writing any GPUI code**, read the current API: the pinned rev's `crates/gpui/examples/` (authoritative — especially `input.rs`, `text.rs`, `uniform_list.rs`), the GPUI book, the `gpui` crate docs, and **Context7 MCP** when it has GPUI. *Then* write against the real, current API.
2. Anything that needs live API confirmation: actually verify it against your checkout — never rely on a remembered GPUI API.
3. **Don't route quasimode logic through GPUI's Action/keymap system** — it can't express "a modifier is held with no key." Raw `on_key_down` / `on_key_up` / `on_modifiers_changed` feed the core, which owns the logic. Bare modifiers arrive via `ModifiersChangedEvent` (fires on Wayland and X11, independent of other keys) — that primitive is what the whole concept rides on.

## Licensing & using Zed as a reference

Alteria is **GPL-3.0**. Zed's repo holds two kinds of code on opposite sides of a license line:

- **GPUI usage / patterns (Apache-2.0):** `gpui` (+ `gpui_*`), `sum_tree`, `collections` — copy and adapt freely. That's what GPUI is for.
- **Zed's editor logic (GPL-3.0):** `rope`, `text`, `editor`, `language`, `multi_buffer`, `clock`, `fuzzy` — read to understand the **approach**, then write Alteria's own implementation on our crates (ropey). **Never paste Zed's editor source.**

The line runs between `sum_tree` (Apache) and `rope` (GPL). We use **ropey** regardless — the "feels-instant on my files" bar doesn't need SumTree-tier engineering. Ideas/algorithms/architecture aren't copyrightable; literal expression is. GPL-3.0 also cleanly clears GPUI's transitive-GPL link (a GPL binary linking GPL deps is compliant).

Whatever zed is doing that makes them so good, we do exactly that.


## Conventions

- **TDD for `alteria-core`:** write the failing test → run red → implement → run green → commit. The engine is fully unit-testable headless.
- `cargo fmt` + `cargo clippy` clean before committing — warnings are not OK on commit.
- One concept per module file; keep them small. Plain data + pure functions in the core (only mutable input state is `Resolver.held` + the `Buffer`).
- No `unwrap()` / `panic!` on input-reachable paths. Handle edges explicitly: buffer start/end, empty buffer, modifier release (the no-stuck-quasimode guarantee).
- **Commits:** descriptive messages; the author stays the human git user. Per-task style: `feat(core): …`, `feat(gpui): …`, `chore: …`. **Never** add `Co-Authored-By: Claude` or any AI/"Generated with" trailer.

## Self-Review

Before ending any response that produced code, genuinely re-read what you wrote and verify (don't list areas the task didn't touch):

1. **Decoupled core:** no `gpui` import anywhere in `alteria-core`; pipeline stages stay pure functions over plain data.
2. **GPUI is current, not remembered:** every GPUI API used was checked against the pinned rev's examples / docs / Context7, not written from memory.
3. **Licensing:** no Zed GPL editor source pasted; GPUI (Apache) patterns are fine to adapt.
4. **Tests:** core changes have tests; `cargo test -p alteria-core` passes; new behavior driven test-first where practical.
5. **Clean build:** `cargo fmt`, `cargo clippy`, `cargo build` clean (no warnings) before commit.
6. **Edges handled:** buffer start/end, empty buffer, modifier release (no stuck quasimode); no `unwrap()` on input-reachable paths.
7. **Frontend boundary:** OS events become `InputEvent` only inside `alteria-gpui`; the core never sees a GPUI type.
