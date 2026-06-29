//! Raw input as plain data — the only thing the frontend hands to the engine.
//!
//! No GUI types live here. The GPUI frontend translates OS events into these
//! values; the core never sees a `gpui` type (the one hard rule).

/// The modifier keys currently held. Plain data — held state lives in the
/// [`crate::resolver::Resolver`], not here.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub super_key: bool,
}

impl Modifiers {
    /// No modifier held — the Base layer (typing).
    pub const NONE: Modifiers = Modifiers {
        alt: false,
        ctrl: false,
        shift: false,
        super_key: false,
    };

    /// True when no modifier is held (the Base layer is active).
    pub fn is_none(self) -> bool {
        self == Modifiers::NONE
    }
}

/// A single logical key. Printable input is `Char(c)`; the rest are the
/// non-printing keys the engine reacts to in the Base layer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Enter,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

/// One raw input event from the frontend.
///
/// `ModifiersChanged` is the primitive the whole quasimode concept rides on:
/// it fires whenever the held-modifier set changes, independent of any other
/// key, so the resolver always knows which layer is active.
///
/// Not `Copy`: `InsertText` carries an owned `String` (external text from a
/// paste or IME commit — the pipeline-pure analog of a platform `InputHandler`'s
/// `replace_text_in_range`). The frontend moves each event into
/// [`Editor::handle`](crate::Editor::handle), so it never needs to copy one.
#[derive(Clone, PartialEq, Debug)]
pub enum InputEvent {
    KeyDown {
        key: Key,
        mods: Modifiers,
        repeat: bool,
    },
    KeyUp {
        key: Key,
        mods: Modifiers,
    },
    ModifiersChanged {
        mods: Modifiers,
    },
    /// External text entering the buffer at every cursor (paste, later IME),
    /// not a keystroke — the resolver passes it straight to the executor.
    InsertText(String),
    FocusLost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_none() {
        assert!(Modifiers::NONE.is_none());
    }

    #[test]
    fn any_modifier_is_not_none() {
        assert!(!Modifiers {
            alt: true,
            ..Modifiers::NONE
        }
        .is_none());
        assert!(!Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        }
        .is_none());
    }

    #[test]
    fn default_equals_none() {
        assert_eq!(Modifiers::default(), Modifiers::NONE);
    }
}
