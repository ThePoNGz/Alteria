//! The declarative keymap: held modifiers select a [`Layer`]; the key selects
//! the [`Action`] within it.
//!
//! Layers are keyed by the exact [`Modifiers`] held, so the same keycap means
//! different things depending on which modifier is down — that is the whole
//! quasimode idea expressed as data. Keys are stored as the **lowercase
//! keycap** (`Char('w')`, not `Char('W')`); the resolver normalizes ASCII
//! letter case before looking up a modified layer, and the frontend (plan 002)
//! owns physical-key → keycap translation for shifted symbols.
//!
//! Not every behavior is a binding: the repeat count (`Alt+1..9`), the find
//! sub-mode (`Alt+F` then a char), and base-layer typing are resolver state,
//! not table entries (see `resolver`).

use std::collections::HashMap;

use crate::action::{Action, Direction, Expansion, Motion};
use crate::input::{Key, Modifiers};

/// One modifier layer: a flat map from key to the action it triggers.
#[derive(Clone, Default, Debug)]
pub struct Layer {
    pub bindings: HashMap<Key, Action>,
}

impl Layer {
    fn new() -> Self {
        Layer {
            bindings: HashMap::new(),
        }
    }

    fn bind(&mut self, key: Key, action: Action) {
        self.bindings.insert(key, action);
    }
}

/// The full keymap: one [`Layer`] per held-modifier combination.
#[derive(Clone, Default, Debug)]
pub struct Keymap {
    pub layers: HashMap<Modifiers, Layer>,
}

const ALT: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: false,
    super_key: false,
};
const ALT_SHIFT: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: true,
    super_key: false,
};
const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    super_key: false,
};
const CTRL_SHIFT: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: true,
    super_key: false,
};
const ALT_CTRL: Modifiers = Modifiers {
    alt: true,
    ctrl: true,
    shift: false,
    super_key: false,
};

impl Keymap {
    /// Look up the action bound to `key` while `mods` are held.
    pub fn lookup(&self, mods: Modifiers, key: Key) -> Option<Action> {
        self.layers.get(&mods)?.bindings.get(&key).cloned()
    }

    /// The built-in Alteria keymap (see `KEYMAP.md`).
    pub fn default_alteria() -> Self {
        let mut layers = HashMap::new();

        let mut alt = motion_layer(false);
        add_expansions(&mut alt);
        layers.insert(ALT, alt);

        // Alt+Shift: the same motions, extending; expansions are not motions,
        // so they are identical to the Alt layer.
        let mut alt_shift = motion_layer(true);
        add_expansions(&mut alt_shift);
        layers.insert(ALT_SHIFT, alt_shift);

        let mut ctrl = Layer::new();
        ctrl.bind(Key::Char('z'), Action::Undo);
        ctrl.bind(Key::Char('y'), Action::Redo);
        layers.insert(CTRL, ctrl);

        // Ctrl+Shift: the second redo binding (mirrors Zed-Linux, which binds
        // `editor::Redo` to both `ctrl-y` and `ctrl-shift-z`).
        let mut ctrl_shift = Layer::new();
        ctrl_shift.bind(Key::Char('z'), Action::Redo);
        layers.insert(CTRL_SHIFT, ctrl_shift);

        // Provisional multicursor spawn (KEYMAP.md "not yet specified").
        let mut alt_ctrl = Layer::new();
        alt_ctrl.bind(Key::Char('w'), Action::SpawnCursor(Direction::Up));
        alt_ctrl.bind(Key::Char('s'), Action::SpawnCursor(Direction::Down));
        layers.insert(ALT_CTRL, alt_ctrl);

        Keymap { layers }
    }
}

/// Build the collapsing/extending motion bindings shared by Alt and Alt+Shift.
fn motion_layer(extend: bool) -> Layer {
    let mut l = Layer::new();
    let mv = |motion| Action::Move {
        motion,
        extend,
        count: 1,
    };
    // Inverted-T navigation.
    l.bind(Key::Char('w'), mv(Motion::Char(Direction::Up)));
    l.bind(Key::Char('s'), mv(Motion::Char(Direction::Down)));
    l.bind(Key::Char('a'), mv(Motion::Char(Direction::Left)));
    l.bind(Key::Char('d'), mv(Motion::Char(Direction::Right)));
    // Word / line / bracket.
    l.bind(Key::Char('q'), mv(Motion::WordStart(Direction::Left)));
    l.bind(Key::Char('e'), mv(Motion::WordStart(Direction::Right)));
    l.bind(Key::Char('z'), mv(Motion::LineEdge(Direction::Left)));
    l.bind(Key::Char('c'), mv(Motion::LineEdge(Direction::Right)));
    l.bind(Key::Char('r'), mv(Motion::MatchingBracket));
    l.bind(Key::Char('['), mv(Motion::BlankLine(Direction::Up)));
    l.bind(Key::Char(']'), mv(Motion::BlankLine(Direction::Down)));
    l
}

/// Add the `I`/`U`/`O`/`P` expansion bindings (identical across Alt layers).
fn add_expansions(l: &mut Layer) {
    l.bind(Key::Char('i'), Action::Expand(Expansion::Enclosing));
    l.bind(Key::Char('u'), Action::Expand(Expansion::EnclosingLeft));
    l.bind(Key::Char('o'), Action::Expand(Expansion::BracketContent));
    l.bind(
        Key::Char('p'),
        Action::Expand(Expansion::BracketAlternating),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn km() -> Keymap {
        Keymap::default_alteria()
    }

    #[test]
    fn alt_w_moves_up_collapsing() {
        assert_eq!(
            km().lookup(ALT, Key::Char('w')),
            Some(Action::Move {
                motion: Motion::Char(Direction::Up),
                extend: false,
                count: 1,
            })
        );
    }

    #[test]
    fn alt_shift_w_moves_up_extending() {
        assert_eq!(
            km().lookup(ALT_SHIFT, Key::Char('w')),
            Some(Action::Move {
                motion: Motion::Char(Direction::Up),
                extend: true,
                count: 1,
            })
        );
    }

    #[test]
    fn alt_layer_full_motion_set() {
        let k = km();
        use Direction::*;
        let cases = [
            ('a', Motion::Char(Left)),
            ('d', Motion::Char(Right)),
            ('s', Motion::Char(Down)),
            ('q', Motion::WordStart(Left)),
            ('e', Motion::WordStart(Right)),
            ('z', Motion::LineEdge(Left)),
            ('c', Motion::LineEdge(Right)),
            ('r', Motion::MatchingBracket),
            ('[', Motion::BlankLine(Up)),
            (']', Motion::BlankLine(Down)),
        ];
        for (ch, motion) in cases {
            assert_eq!(
                k.lookup(ALT, Key::Char(ch)),
                Some(Action::Move {
                    motion,
                    extend: false,
                    count: 1,
                }),
                "binding for Alt+{ch}",
            );
        }
    }

    #[test]
    fn expansions_are_unaffected_by_shift() {
        let k = km();
        for mods in [ALT, ALT_SHIFT] {
            assert_eq!(
                k.lookup(mods, Key::Char('i')),
                Some(Action::Expand(Expansion::Enclosing))
            );
            assert_eq!(
                k.lookup(mods, Key::Char('u')),
                Some(Action::Expand(Expansion::EnclosingLeft))
            );
            assert_eq!(
                k.lookup(mods, Key::Char('o')),
                Some(Action::Expand(Expansion::BracketContent))
            );
            assert_eq!(
                k.lookup(mods, Key::Char('p')),
                Some(Action::Expand(Expansion::BracketAlternating))
            );
        }
    }

    #[test]
    fn ctrl_z_is_undo() {
        assert_eq!(km().lookup(CTRL, Key::Char('z')), Some(Action::Undo));
    }

    #[test]
    fn ctrl_y_is_redo() {
        assert_eq!(km().lookup(CTRL, Key::Char('y')), Some(Action::Redo));
    }

    #[test]
    fn ctrl_shift_z_is_redo() {
        // Mirror Zed-Linux: redo binds to both Ctrl+Y and Ctrl+Shift+Z.
        assert_eq!(km().lookup(CTRL_SHIFT, Key::Char('z')), Some(Action::Redo));
    }

    #[test]
    fn alt_ctrl_spawns_cursors() {
        let k = km();
        assert_eq!(
            k.lookup(ALT_CTRL, Key::Char('w')),
            Some(Action::SpawnCursor(Direction::Up))
        );
        assert_eq!(
            k.lookup(ALT_CTRL, Key::Char('s')),
            Some(Action::SpawnCursor(Direction::Down))
        );
    }

    #[test]
    fn base_layer_has_no_bindings() {
        // Typing is resolver fallback, not a keymap layer.
        assert_eq!(km().lookup(Modifiers::NONE, Key::Char('w')), None);
        assert_eq!(km().lookup(Modifiers::NONE, Key::Char('a')), None);
    }

    #[test]
    fn unbound_key_in_a_layer_is_none() {
        // 'j' is not bound in the Alt layer.
        assert_eq!(km().lookup(ALT, Key::Char('j')), None);
    }
}
