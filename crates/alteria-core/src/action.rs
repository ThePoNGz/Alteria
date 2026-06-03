//! Editor intent, fully decoupled from the keys that triggered it.
//!
//! The keymap maps `(Modifiers, Key) -> Action`; the executor applies an
//! `Action` to the buffer. Nothing here knows about specific keys, so bindings
//! stay remappable without touching execution.

/// A cardinal direction. Motions reuse it (`Up`/`Down` for vertical, `Left`/
/// `Right` for horizontal); some motions only use one axis (see [`Motion`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// A cursor motion. All motions collapse by default; the Alt+Shift layer sets
/// `extend` on the [`Action::Move`] that carries them. See `KEYMAP.md`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    /// `W`/`S`/`A`/`D` — one char/line in a direction.
    Char(Direction),
    /// `Q` = previous word start (`Left`), `E` = next word start (`Right`).
    WordStart(Direction),
    /// `Z` = line start (`Left`), `C` = line end (`Right`).
    LineEdge(Direction),
    /// `[` = previous blank line (`Up`), `]` = next blank line (`Down`).
    BlankLine(Direction),
    /// `R` — jump to the matching bracket (nesting-aware, across lines).
    MatchingBracket,
}

/// Selection-expansion variants (`I`/`U`/`O`/`P`). See `KEYMAP.md` for the
/// exact climbing semantics each one implements.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expansion {
    /// `I` — next enclosing span, climbing one level out.
    Enclosing,
    /// `U` — like `Enclosing` but biased toward the start.
    EnclosingLeft,
    /// `O` — content inside the nearest pair, climbing to the parent's content.
    BracketContent,
    /// `P` — alternating content / pair-including-delimiters as it climbs.
    BracketAlternating,
}

/// What the engine should do — the single intent type the executor consumes.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// Insert a printable char at every cursor, advance, collapse.
    InsertChar(char),
    /// Insert a newline at every cursor.
    InsertNewline,
    /// Delete the char before each cursor's head (no-op at start).
    DeleteBackward,
    /// `Esc` — collapse selection spans and drop to the primary cursor.
    CollapseSelection,
    /// Run `motion` `count` times; `extend` keeps the anchor (the resolver
    /// fills `count >= 1`).
    Move {
        motion: Motion,
        extend: bool,
        count: usize,
    },
    /// `Alt+F` then a char — jump to the next occurrence on the current line.
    FindChar { ch: char },
    /// `A`/`D` while find is active — repeat the search for the same target.
    FindRepeat { ch: char, forward: bool },
    /// `I`/`U`/`O`/`P` — expand the selection one level.
    Expand(Expansion),
    /// `Alt+Ctrl` `W`/`S` — spawn a cursor above/below (provisional spec).
    SpawnCursor(Direction),
    /// `Ctrl+Z` — step back one entry in the history.
    Undo,
    /// `Ctrl+Y` / `Ctrl+Shift+Z` — step forward one entry in the history
    /// (real redo = undo-of-undo via Zed's `UndoMap`).
    Redo,
    /// Inject external text (paste / IME) at every cursor: replace each selection
    /// with the string; an empty string deletes (the cut path). The pipeline-pure
    /// analog of Zed's `replace_text_in_range` -> `editor.insert`.
    InsertText(String),
    /// Inject one string per cursor — multicursor paste distribution. When the
    /// count matches the live cursors, the i-th cursor gets `texts[i]`; otherwise
    /// the whole `\n`-joined text goes at every cursor (Zed `do_paste`'s
    /// count-mismatch branch).
    InsertTexts(Vec<String>),
}
