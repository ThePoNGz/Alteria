//! `alteria-core` — the pure-Rust editing engine for Alteria.
//!
//! This crate is governed by the project's one hard rule: it **never imports
//! `gpui`** (or any rendering/GUI dependency). Every stage of the
//! `InputEvent -> Resolver -> Action -> Executor -> Buffer` pipeline is a pure
//! function over plain data, fully unit-testable without a window.
//!
//! The single entry point a frontend uses is [`Editor`]: feed it raw
//! [`InputEvent`](input::InputEvent)s and read [`Editor::buffer`] to render.

pub mod action;
pub mod buffer;
pub mod executor;
pub mod expand;
pub mod find;
pub mod history;
pub mod input;
pub mod keymap;
pub mod resolver;
pub mod selection;
pub mod transaction;

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
    use crate::selection::Range;

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
            self.buffer.selection.primary().head
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
        assert_eq!(e.buffer.text, "axb");
        assert_eq!(e.head(), 2);
    }

    #[test]
    fn alt_shift_extends_a_selection() {
        let mut e = Editor::new("abcde");
        e.hold(ALT_SHIFT);
        e.key('d', ALT_SHIFT);
        e.key('d', ALT_SHIFT);
        assert_eq!(e.buffer.selection.primary(), Range { anchor: 0, head: 2 });
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
        let p = e.buffer.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 4));
        e.key('i', ALT); // grow right to "aa b"
        let p = e.buffer.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 6));
    }

    #[test]
    fn multicursor_spawn_then_type_hits_both_lines() {
        let mut e = Editor::new("ab\ncd");
        e.hold(ALT_CTRL);
        e.key('s', ALT_CTRL); // spawn a cursor on the line below
        e.release();
        assert_eq!(e.buffer.selection.ranges.len(), 2);
        e.key('X', Modifiers::NONE); // type once -> both cursors get it
        assert_eq!(e.buffer.text, "Xab\nXcd");
    }

    #[test]
    fn undo_walks_back_through_edits_and_expansions() {
        let mut e = Editor::new("foo");
        // Expand to the word, then type over it.
        e.hold(ALT);
        e.key('i', ALT); // select "foo"
        e.release();
        assert_eq!(
            {
                let p = e.buffer.selection.primary();
                (p.min(), p.max())
            },
            (0, 3)
        );
        e.key('x', Modifiers::NONE); // typing replaces the selection -> "x"
        assert_eq!(e.buffer.text, "x");
        assert_eq!(e.head(), 1);

        // Ctrl+Z undoes the edit (text + selection)...
        e.hold(CTRL);
        e.key('z', CTRL);
        assert_eq!(e.buffer.text, "foo");
        let p = e.buffer.selection.primary();
        assert_eq!((p.min(), p.max()), (0, 3)); // the expansion span is restored

        // ...and again undoes the expansion step (selection only).
        e.key('z', CTRL);
        e.release();
        assert_eq!(e.buffer.text, "foo");
        assert_eq!(e.buffer.selection.primary(), Range::cursor(0));
    }

    #[test]
    fn modifier_change_alone_does_not_redraw() {
        let mut e = Editor::new("abc");
        assert!(!e.handle(InputEvent::ModifiersChanged { mods: ALT }));
        assert!(!e.handle(InputEvent::FocusLost));
    }
}
