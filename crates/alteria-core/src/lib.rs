//! `alteria-core` — the pure-Rust editing engine for Alteria.
//!
//! This crate is governed by the project's one hard rule: it **never imports
//! `gpui`** (or any rendering/GUI dependency). Every stage of the
//! `InputEvent -> Resolver -> Action -> Executor -> Buffer` pipeline is a pure
//! function over plain data, fully unit-testable without a window.
//!
//! The buffer, anchors, undo/redo (Lamport clock + `UndoMap`), and selections
//! are Zed's `text` model, vendored from `zed-industries/zed`. The single entry
//! point a frontend uses is [`Editor`]: feed it raw
//! [`InputEvent`](input::InputEvent)s and read [`Editor::buffer`] to render.

pub mod action;
pub mod buffer;
pub mod char_kind;
pub mod executor;
pub mod expand;
pub mod find;
pub mod history;
pub mod input;
pub mod keymap;
pub mod resolver;
pub mod selection;

use buffer::Buffer;
use history::History;
use input::InputEvent;
use keymap::Keymap;
use resolver::Resolver;

/// The editor facade: ties the resolver, keymap, buffer, and history together.
///
/// The frontend translates OS events into [`InputEvent`]s, calls [`Editor::handle`]
/// for each, and re-reads [`Editor::buffer`] to draw when it returns `true`.
/// This is the only surface the (GPUI) frontend touches; the whole pipeline
/// behind it is headless and pure.
pub struct Editor {
    pub buffer: Buffer,
    resolver: Resolver,
    keymap: Keymap,
    history: History,
}

impl Editor {
    /// A new editor over `text`, with the default Alteria keymap and a single
    /// cursor at the start.
    pub fn new(text: &str) -> Self {
        Editor {
            buffer: Buffer::from_str(text),
            resolver: Resolver::new(),
            keymap: Keymap::default_alteria(),
            history: History::new(),
        }
    }

    /// Feed one raw input event. Returns `true` if the view should redraw
    /// (i.e. an action was executed).
    pub fn handle(&mut self, event: InputEvent) -> bool {
        match self.resolver.resolve(event, &self.keymap) {
            Some(action) => {
                executor::apply(action, &mut self.buffer, &mut self.history);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, Modifiers};

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
    const ALT_CTRL: Modifiers = Modifiers {
        alt: true,
        ctrl: true,
        shift: false,
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

    /// Small driving helpers for the end-to-end key sequences below.
    impl Editor {
        fn hold(&mut self, mods: Modifiers) {
            self.handle(InputEvent::ModifiersChanged { mods });
        }
        fn key(&mut self, ch: char, mods: Modifiers) -> bool {
            self.handle(InputEvent::KeyDown {
                key: Key::Char(ch),
                mods,
                repeat: false,
            })
        }
        fn release(&mut self) {
            self.handle(InputEvent::ModifiersChanged {
                mods: Modifiers::NONE,
            });
        }
        fn head(&self) -> usize {
            self.buffer.primary_resolved().head()
        }
        fn span(&self) -> (usize, usize) {
            let p = self.buffer.primary_resolved();
            (p.min(), p.max())
        }
    }

    #[test]
    fn quasimode_navigate_then_release_then_type() {
        // The defining property: hold Alt to navigate, release, and typing lands
        // exactly where you navigated — no mode to get stuck in.
        let mut e = Editor::new("ab");
        e.hold(ALT);
        assert!(e.key('d', ALT)); // move right: cursor 0 -> 1
        assert_eq!(e.head(), 1);
        e.release();
        assert!(e.key('x', Modifiers::NONE)); // type at the navigated spot
        assert_eq!(e.buffer.text(), "axb");
        assert_eq!(e.head(), 2);
    }

    #[test]
    fn alt_shift_extends_a_selection() {
        let mut e = Editor::new("abcde");
        e.hold(ALT_SHIFT);
        e.key('d', ALT_SHIFT);
        e.key('d', ALT_SHIFT);
        assert_eq!(e.span(), (0, 2));
        assert!(!e.buffer.primary_resolved().reversed);
    }

    #[test]
    fn count_repeats_the_next_motion() {
        let mut e = Editor::new("abcdef");
        e.hold(ALT);
        assert!(!e.key('3', ALT)); // count digit: no redraw
        assert!(e.key('d', ALT)); // move right x3
        assert_eq!(e.head(), 3);
    }

    #[test]
    fn find_then_repeat_through_the_facade() {
        // a0 ' '1 x2 ' '3 b4 ' '5 x6
        let mut e = Editor::new("a x b x");
        e.hold(ALT);
        assert!(!e.key('f', ALT)); // begin find: no redraw
        assert!(e.key('x', ALT)); // jump to first 'x'
        assert_eq!(e.head(), 2);
        assert!(e.key('d', ALT)); // repeat forward
        assert_eq!(e.head(), 6);
        assert!(e.key('a', ALT)); // repeat backward
        assert_eq!(e.head(), 2);
    }

    #[test]
    fn expansion_sequence_grows_the_selection() {
        let mut e = Editor::new("a aa b");
        // Put the cursor inside "aa" (byte 2) by navigating.
        e.hold(ALT);
        e.key('d', ALT);
        e.key('d', ALT); // cursor at 2
        assert_eq!(e.head(), 2);
        e.key('i', ALT); // expand to word "aa"
        assert_eq!(e.span(), (2, 4));
        e.key('i', ALT); // grow right to "aa b"
        assert_eq!(e.span(), (2, 6));
    }

    #[test]
    fn multicursor_spawn_then_type_hits_both_lines() {
        let mut e = Editor::new("ab\ncd");
        e.hold(ALT_CTRL);
        e.key('s', ALT_CTRL); // spawn a cursor on the line below
        e.release();
        assert_eq!(e.buffer.selection.selections.len(), 2);
        e.key('X', Modifiers::NONE); // type once -> both cursors get it
        assert_eq!(e.buffer.text(), "Xab\nXcd");
    }

    #[test]
    fn undo_walks_back_through_edits_and_expansions() {
        let mut e = Editor::new("foo");
        // Expand to the word, then type over it.
        e.hold(ALT);
        e.key('i', ALT); // select "foo"
        e.release();
        assert_eq!(e.span(), (0, 3));

        e.key('x', Modifiers::NONE); // typing replaces the selection -> "x"
        assert_eq!(e.buffer.text(), "x");
        assert_eq!(e.head(), 1);

        // Ctrl+Z undoes the edit (text + selection)...
        e.hold(CTRL);
        e.key('z', CTRL);
        assert_eq!(e.buffer.text(), "foo");
        assert_eq!(e.span(), (0, 3)); // the expansion span is restored

        // ...and again undoes the expansion step (selection only).
        e.key('z', CTRL);
        e.release();
        assert_eq!(e.buffer.text(), "foo");
        assert_eq!(e.head(), 0);
        assert!(e.buffer.primary_resolved().is_empty());
    }

    #[test]
    fn redo_round_trips_text_and_selection_on_ctrl_y() {
        // edit → undo (Ctrl+Z) → redo (Ctrl+Y) restores both text and caret.
        let mut e = Editor::new("abc");
        e.key('X', Modifiers::NONE); // "Xabc", caret 1
        assert_eq!(e.buffer.text(), "Xabc");
        e.hold(CTRL);
        assert!(e.key('z', CTRL)); // undo
        assert_eq!(e.buffer.text(), "abc");
        assert!(e.key('y', CTRL)); // redo
        assert_eq!(e.buffer.text(), "Xabc");
        assert_eq!(e.head(), 1);
        e.release();
    }

    #[test]
    fn redo_also_works_on_ctrl_shift_z() {
        let mut e = Editor::new("abc");
        e.key('X', Modifiers::NONE); // "Xabc"
        e.hold(CTRL);
        e.key('z', CTRL); // undo -> "abc"
        assert_eq!(e.buffer.text(), "abc");
        e.release();
        e.hold(CTRL_SHIFT);
        assert!(e.key('Z', CTRL_SHIFT)); // redo (shifted 'Z', case-normalized)
        assert_eq!(e.buffer.text(), "Xabc");
        e.release();
    }

    #[test]
    fn redo_at_top_of_stack_is_a_noop_but_still_dispatches() {
        // Mirrors the Undo convention: handle returns true whenever the resolver
        // produced an action, even if redo had nothing to do.
        let mut e = Editor::new("abc");
        e.hold(CTRL);
        assert!(e.key('y', CTRL));
        assert_eq!(e.buffer.text(), "abc");
        e.release();
    }

    #[test]
    fn modifier_change_alone_does_not_redraw() {
        let mut e = Editor::new("abc");
        assert!(!e.handle(InputEvent::ModifiersChanged { mods: ALT }));
        assert!(!e.handle(InputEvent::FocusLost));
    }

    // ---- motion fidelity, end-to-end (plan 003) ------------------------

    #[test]
    fn grapheme_motion_steps_over_a_cluster_through_the_facade() {
        // Hold Alt and step right over an "e" + combining-acute cluster (3 bytes)
        // in a single press, then over the following ASCII 'x'.
        let mut e = Editor::new("e\u{0301}x");
        e.hold(ALT);
        assert!(e.key('d', ALT));
        assert_eq!(e.head(), 3); // past the whole grapheme cluster
        assert!(e.key('d', ALT));
        assert_eq!(e.head(), 4); // past 'x'
    }

    #[test]
    fn goal_column_survives_a_short_line_through_the_facade() {
        // col 3 -> down through a 2-col line -> down again restores col 3.
        // "abcd\nef\nghij": a0 b1 c2 d3 \n4 e5 f6 \n7 g8 h9 i10 j11
        let mut e = Editor::new("abcd\nef\nghij");
        e.hold(ALT);
        e.key('d', ALT);
        e.key('d', ALT);
        e.key('d', ALT); // to line0 col3 (byte 3)
        assert_eq!(e.head(), 3);
        e.key('s', ALT); // down -> "ef", clamped to col 2 (byte 7)
        assert_eq!(e.head(), 7);
        e.key('s', ALT); // down -> "ghij", col 3 restored (byte 11)
        assert_eq!(e.head(), 11);
    }

    #[test]
    fn word_right_lands_on_next_word_start_through_the_facade() {
        // `E` = start of the next word (KEYMAP): from 0 in "foo bar" → "bar".
        let mut e = Editor::new("foo bar");
        e.hold(ALT);
        assert!(e.key('e', ALT));
        assert_eq!(e.head(), 4);
    }
}
