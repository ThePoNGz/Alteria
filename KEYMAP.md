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
| `Ctrl` | Ctrl | Standard editor chords (select-all, clipboard, undo / redo) |
| `Ctrl+Shift` | Ctrl+Shift | Redo (the second redo binding) |
| `Alt+Ctrl` | Alt+Ctrl | Multicursor spawn *(provisional)* |

---

## Base layer — ordinary editor surface

With no modifier held, Alteria behaves like any plain editor: type to insert.
The standard Shift and Ctrl chords below are part of the same no-mode editing
surface, imported from Zed's Linux `Editor` keymap.

| Key | Action |
|---|---|
| any printable char | insert at every cursor, advancing; a non-empty selection is **replaced** |
| `Enter` | insert a newline at every cursor |
| `Backspace` | delete the selection if any; else the grapheme before each cursor (no-op at buffer start) |
| `Shift+Backspace` | same as `Backspace` |
| `Delete` | delete the selection if any; else the grapheme after each cursor (no-op at buffer end) |
| `Esc` | collapse selection spans, drop secondary cursors to the primary |
| `Left` / `Right` | move one grapheme left / right, collapsing any selection |
| `Up` / `Down` | move one line up / down, preserving the vertical goal column |
| `Shift+Left` / `Shift+Right` | extend the selection one grapheme left / right |
| `Shift+Up` / `Shift+Down` | extend the selection one line up / down |
| `Home` | move to the current line beginning |
| `End` | move to the current line end (before the trailing newline) |
| `Shift+Home` | extend to the current line beginning |
| `Shift+End` | extend to the current line end |
| `PageUp` / `PageDown` | move one viewport page up / down |
| `Shift+PageUp` / `Shift+PageDown` | extend one viewport page up / down |

`Shift` alone is still the Base layer — it only capitalizes the typed character.

Mouse:

| Gesture | Action |
|---|---|
| left click | place the primary cursor at the clicked text position |
| `Shift`+left click | extend the primary selection to the clicked text position |
| left drag | select the dragged text span |
| double click | select the clicked word if the imported Zed path is cheap in this slice; otherwise deferred |

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

## Ctrl held — standard editor chords

| Key | Action |
|---|---|
| `A` | select the whole buffer |
| `C` | copy selection text to the OS clipboard; empty selections copy the current line, matching Zed |
| `X` | cut selection text to the OS clipboard; empty selections cut the current line, matching Zed |
| `V` | paste OS clipboard text, distributing per-cursor clipboard metadata when available |
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

These Zed Linux Editor bindings are deliberately outside plan 010's selected
base-editor subset or depend on later file/UI systems.

| Intended binding | Status / core capability |
|---|---|
| `Ctrl+S` save | not yet (no file I/O in core) |
| `Tab` / `Shift+Tab` | deferred indentation plan |
| `Ctrl+Backspace` / `Ctrl+Delete` | deferred word-deletion import |
| `Ctrl+Left` / `Ctrl+Right` and Shift variants | deferred word-motion import |
| `Ctrl+Home` / `Ctrl+End` and Shift variants | deferred document-edge import |
| `Ctrl+L` | deferred line-selection import |
