# KEYMAP.md — Alteria keystroke spec

The **source of truth for every binding**. Concept, architecture, and workflow
live in `CLAUDE.md`; this file is *only* the keystrokes. It mirrors the engine's
declarative keymap (`crates/alteria-core/src/keymap.rs`) and the resolver state
machine (`crates/alteria-core/src/resolver.rs`) — when a binding changes there,
change it here in the same commit.

Held modifiers select a **layer**; the key selects the action within it. A
binding is **case-insensitive** for ASCII letters (Shift selects the layer, not
the letter), so `Alt+W` and `Alt+Shift+W` look up the same `w` keycap in
different layers. Releasing all modifiers returns instantly to the Base layer —
there is no mode to get stuck in (the quasimode guarantee).

| Modifier held | Layer | Purpose |
|---|---|---|
| *(none)* | Base | Ordinary editor — typing & standard edits |
| `Alt` | Alt | Inverted-T navigation, motions, find, count, expansion |
| `Alt+Shift` | Alt+Shift | The Alt motions, **extending** the selection |
| `Ctrl` | Ctrl | Undo / redo |
| `Ctrl+Shift` | Ctrl+Shift | Redo (the second redo binding) |
| `Alt+Ctrl` | Alt+Ctrl | Multicursor spawn *(provisional)* |

---

## Base layer — no modifier held (an ordinary editor)

With no modifier held, Alteria behaves like any plain editor: type to insert.

| Key | Action |
|---|---|
| any printable char | insert at every cursor, advancing; a non-empty selection is **replaced** |
| `Enter` | insert a newline at every cursor |
| `Backspace` | delete the selection if any; else the grapheme before each cursor (no-op at buffer start) |
| `Esc` | collapse selection spans, drop secondary cursors to the primary |

`Shift` alone is still the Base layer — it only capitalizes the typed character.

> Standard mouse-drag / `Shift`+arrow selection and the `Ctrl+C/X/V`, `Ctrl+S`
> conventions are part of the "ordinary editor by default" goal but are **not
> yet bound** in the core keymap — see [Not yet bound](#not-yet-bound).

---

## Alt held — navigation & selection quasimode

Hold `Alt` (with the opposite hand) to turn the home keys into navigation. All
Alt motions **collapse** the selection to a bare cursor unless `Shift` is also
held (see [Alt+Shift](#altshift-held--extend-the-selection)).

### Inverted-T + character motions

| Key | Motion |
|---|---|
| `W` | up one line |
| `A` | left one grapheme |
| `S` | down one line |
| `D` | right one grapheme |

Vertical motion keeps a **goal column** across short lines. Horizontal motion
steps by whole grapheme cluster.

### Word / line / bracket motions

| Key | Motion |
|---|---|
| `Q` | start of the previous word |
| `E` | start of the next word |
| `Z` | line start |
| `C` | line end (stops before the trailing newline) |
| `R` | jump to the matching bracket — on a delimiter jumps to its partner, inside a pair jumps to the enclosing close; nesting-aware, across lines (ignores the repeat count) |
| `[` | previous blank line |
| `]` | next blank line |

### Selection expansion

Expansion grows the **primary** selection one level and records a selection-only
history step, so `Ctrl+Z` walks back through expansions too. Scope is brackets
`()[]{}` only (string/quote pairing is deferred to tree-sitter).

| Key | Expansion |
|---|---|
| `I` | next enclosing span, climbing one level out |
| `U` | like `I`, biased toward the start |
| `O` | content inside the nearest pair, climbing to the parent's content |
| `P` | alternating content / pair-including-delimiters as it climbs |

### Find on the current line

| Keys | Action |
|---|---|
| `F` then `<char>` | move every cursor's head to the next occurrence of `<char>` on its line |
| `D` | repeat the find **forward** (while Alt stays held) |
| `A` | repeat the find **backward** |

The target may be any character (even a digit). Any non-`A`/`D` key ends find and
is then handled normally. Releasing `Alt` ends find.

### Repeat count

Holding `Alt` and pressing digits accumulates a count applied to the **next**
motion, then clears (`Alt+3` `D` = move right 3). A leading `0` is not a count;
a trailing `0` continues one (`10`, `200`, …). The count saturates rather than
overflowing. Entering find or releasing `Alt` clears a pending count.

---

## Alt+Shift held — extend the selection

Every motion in the [Alt layer](#alt-held--navigation--selection-quasimode)
behaves identically, except the **anchor stays put and the head moves**, so the
span grows or shrinks (Anchor/Head model). The expansion keys `I`/`U`/`O`/`P`
are not motions and behave exactly as in the Alt layer.

---

## Ctrl held — undo / redo

| Key | Action |
|---|---|
| `Z` | undo — step back one entry (text edits **and** selection-only expansions share one timeline) |
| `Y` | redo — step forward one entry (real redo = undo-of-undo via Zed's `UndoMap`) |

## Ctrl+Shift held — redo

| Key | Action |
|---|---|
| `Z` | redo (the second binding, mirroring the Linux convention of `Ctrl+Y` **and** `Ctrl+Shift+Z`) |

---

## Alt+Ctrl held — multicursor *(provisional)*

Spec not yet finalized; the current provisional bindings:

| Key | Action |
|---|---|
| `W` | spawn a cursor one line above the primary (same column), making it primary |
| `S` | spawn a cursor one line below the primary (same column), making it primary |

---

## Not yet bound

These are deliberately unbound today; the listed core capability already exists
where noted, so binding them is a small future step (mostly wave-2 frontend).

| Intended binding | Status / core capability |
|---|---|
| `Ctrl+C` copy | **wave 2** (frontend OS-clipboard write). Core gather: `Buffer::selected_texts()` |
| `Ctrl+X` cut | **wave 2**. Core: `selected_texts()` to gather + `InsertText("")` to delete |
| `Ctrl+V` paste | **wave 2** (frontend reads the OS clipboard). Core injection: `InsertText` / `InsertTexts` (per-cursor distribution) |
| `Ctrl+S` save | not yet (no file I/O in core) |
| forward `Delete` | not yet |
| mouse / `Shift`+arrow selection | frontend; not yet in the core keymap |
