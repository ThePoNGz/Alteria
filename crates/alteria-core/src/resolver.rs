//! The resolver: the only place input *state* lives.
//!
//! It owns the held modifiers, a pending repeat count, and the find sub-mode,
//! and turns each raw [`InputEvent`] into at most one [`Action`]. Everything
//! else in the pipeline is a pure function; this is the single small state
//! machine the quasimode model needs.
//!
//! Held modifiers select a keymap layer. Releasing all modifiers returns to the
//! Base layer (typing) instantly — there is no mode to get stuck in, so a
//! modifier-release or focus-loss always clears Alt-gated state.

use crate::action::{Action, Direction, Motion};
use crate::input::{InputEvent, Key, Modifiers};
use crate::keymap::Keymap;

/// The `Alt+F` find sub-mode (see `KEYMAP.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FindState {
    /// Not finding.
    Inactive,
    /// `Alt+F` pressed; the next char is the search target.
    Pending,
    /// A target is set; `A`/`D` repeat the search while Alt stays held.
    Active { ch: char },
}

/// Holds the mutable input state and resolves events to actions.
#[derive(Clone, Debug)]
pub struct Resolver {
    held: Modifiers,
    count: usize,
    find: FindState,
}

impl Default for Resolver {
    fn default() -> Self {
        Resolver::new()
    }
}

impl Resolver {
    pub fn new() -> Self {
        Resolver {
            held: Modifiers::NONE,
            count: 0,
            find: FindState::Inactive,
        }
    }

    /// The modifiers currently held.
    pub fn held(&self) -> Modifiers {
        self.held
    }

    /// The accumulated repeat count (0 = none). Exposed for tests.
    pub fn pending_count(&self) -> usize {
        self.count
    }

    /// The find sub-mode. Exposed for tests.
    pub fn find_state(&self) -> FindState {
        self.find
    }

    /// Resolve one raw input event into at most one action, updating state.
    pub fn resolve(&mut self, event: InputEvent, keymap: &Keymap) -> Option<Action> {
        match event {
            InputEvent::ModifiersChanged { mods } => {
                self.held = mods;
                if !mods.alt {
                    self.clear_alt_state();
                }
                None
            }
            InputEvent::FocusLost => {
                self.held = Modifiers::NONE;
                self.clear_alt_state();
                None
            }
            InputEvent::KeyUp { .. } => None,
            // External text (paste / IME) is not a keystroke: pass it straight
            // through, untouched by held-modifier or count/find state.
            InputEvent::InsertText(text) => Some(Action::InsertText(text)),
            InputEvent::KeyDown { key, mods, .. } => self.resolve_key_down(key, mods, keymap),
        }
    }

    /// Clear the Alt-gated state (repeat count and find sub-mode). Releasing Alt
    /// or losing focus must never leave a quasimode stuck.
    fn clear_alt_state(&mut self) {
        self.count = 0;
        self.find = FindState::Inactive;
    }

    fn resolve_key_down(&mut self, key: Key, mods: Modifiers, keymap: &Keymap) -> Option<Action> {
        self.held = mods;
        if !mods.alt {
            // Alt-gated state cannot persist into a key pressed without Alt.
            self.clear_alt_state();
        }

        // 1. Find target capture: the next key after Alt+F is the literal target.
        if mods.alt && self.find == FindState::Pending {
            if let Key::Char(c) = key {
                self.find = FindState::Active { ch: c };
                return Some(Action::FindChar { ch: c });
            }
            // A non-char ends the pending find; fall through to handle the key.
            self.find = FindState::Inactive;
        }

        // 2. Find repeat: A/D step while find is active; any other key ends find
        //    and is then handled normally.
        if mods.alt {
            if let FindState::Active { ch } = self.find {
                match command_char(key) {
                    Key::Char('d') => return Some(Action::FindRepeat { ch, forward: true }),
                    Key::Char('a') => return Some(Action::FindRepeat { ch, forward: false }),
                    _ => self.find = FindState::Inactive,
                }
            }
        }

        // 3. Begin find. Entering the find sub-mode consumes any pending count
        //    (a count is for the next motion, not for a find that intervenes).
        if mods.alt && command_char(key) == Key::Char('f') {
            self.count = 0;
            self.find = FindState::Pending;
            return None;
        }

        // 4. Repeat count: Alt + digit accumulates. A leading `0` is not a count
        //    (there is no zero-times motion), but `0` continues an in-progress
        //    count, so 10, 20, 200, ... are reachable.
        if mods.alt {
            if let Key::Char(c) = key {
                if let Some(d) = c.to_digit(10) {
                    if d != 0 || self.count > 0 {
                        // Saturate: a held/auto-repeated digit must never overflow
                        // usize (which panics in debug builds). A clamped count
                        // just drives a motion that stops at the buffer edge.
                        self.count = self.count.saturating_mul(10).saturating_add(d as usize);
                        return None;
                    }
                }
            }
        }

        // 5. Keymap hit. A motion is stamped with the pending count (then it
        //    clears); any other bound action consumes the count too.
        if let Some(action) = keymap.lookup(self.held, command_char(key)) {
            let count = self.count;
            self.count = 0;
            if let Action::Move { motion, extend, .. } = action {
                return Some(Action::Move {
                    motion,
                    extend,
                    count: count.max(1),
                });
            }
            return Some(action);
        }

        // 6. Base layer (no Alt/Ctrl/Super): typing. Shift only capitalizes, and
        //    the reported char already carries that.
        if !self.held.alt && !self.held.ctrl && !self.held.super_key {
            return Some(match key {
                Key::Char(c) => Action::InsertChar(c),
                Key::Enter => Action::InsertNewline,
                Key::Backspace => Action::DeleteBackward,
                Key::Delete => Action::DeleteForward,
                Key::Escape => Action::CollapseSelection,
                Key::ArrowLeft => Action::Move {
                    motion: Motion::Char(Direction::Left),
                    extend: self.held.shift,
                    count: 1,
                },
                Key::ArrowRight => Action::Move {
                    motion: Motion::Char(Direction::Right),
                    extend: self.held.shift,
                    count: 1,
                },
                Key::ArrowUp => Action::Move {
                    motion: Motion::Char(Direction::Up),
                    extend: self.held.shift,
                    count: 1,
                },
                Key::ArrowDown => Action::Move {
                    motion: Motion::Char(Direction::Down),
                    extend: self.held.shift,
                    count: 1,
                },
                Key::Home => Action::Move {
                    motion: Motion::LineEdge(Direction::Left),
                    extend: self.held.shift,
                    count: 1,
                },
                Key::End => Action::Move {
                    motion: Motion::LineEdge(Direction::Right),
                    extend: self.held.shift,
                    count: 1,
                },
                Key::PageUp => Action::MovePage {
                    direction: Direction::Up,
                    extend: self.held.shift,
                    rows: 0,
                },
                Key::PageDown => Action::MovePage {
                    direction: Direction::Down,
                    extend: self.held.shift,
                    rows: 0,
                },
            });
        }

        // 7. A modifier is held but the key is unbound: cancel any pending count.
        self.count = 0;
        None
    }
}

/// Lowercase an ASCII-letter keycap for command dispatch — commands are
/// case-insensitive (Shift selects the layer, not the letter). Non-letters and
/// the find *target* are matched verbatim.
fn command_char(key: Key) -> Key {
    match key {
        Key::Char(c) => Key::Char(c.to_ascii_lowercase()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    const SHIFT: Modifiers = Modifiers {
        alt: false,
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

    fn km() -> Keymap {
        Keymap::default_alteria()
    }
    fn down(key: Key, mods: Modifiers) -> InputEvent {
        InputEvent::KeyDown {
            key,
            mods,
            repeat: false,
        }
    }
    fn mods_changed(mods: Modifiers) -> InputEvent {
        InputEvent::ModifiersChanged { mods }
    }
    fn mv(motion: Motion, extend: bool, count: usize) -> Option<Action> {
        Some(Action::Move {
            motion,
            extend,
            count,
        })
    }

    #[test]
    fn plain_char_inserts() {
        let mut r = Resolver::new();
        assert_eq!(
            r.resolve(down(Key::Char('a'), Modifiers::NONE), &km()),
            Some(Action::InsertChar('a'))
        );
    }

    #[test]
    fn shift_alone_still_types_uppercase() {
        let mut r = Resolver::new();
        // Shift alone is Base-layer capitalization, not a command layer.
        assert_eq!(
            r.resolve(down(Key::Char('A'), SHIFT), &km()),
            Some(Action::InsertChar('A'))
        );
    }

    #[test]
    fn enter_backspace_esc_in_base_layer() {
        let mut r = Resolver::new();
        let k = km();
        assert_eq!(
            r.resolve(down(Key::Enter, Modifiers::NONE), &k),
            Some(Action::InsertNewline)
        );
        assert_eq!(
            r.resolve(down(Key::Backspace, Modifiers::NONE), &k),
            Some(Action::DeleteBackward)
        );
        assert_eq!(
            r.resolve(down(Key::Delete, Modifiers::NONE), &k),
            Some(Action::DeleteForward)
        );
        assert_eq!(
            r.resolve(down(Key::Escape, Modifiers::NONE), &k),
            Some(Action::CollapseSelection)
        );
    }

    #[test]
    fn arrow_keys_are_base_motions() {
        let mut r = Resolver::new();
        let k = km();
        assert_eq!(
            r.resolve(down(Key::ArrowLeft, Modifiers::NONE), &k),
            mv(Motion::Char(Direction::Left), false, 1)
        );
        assert_eq!(
            r.resolve(down(Key::ArrowRight, Modifiers::NONE), &k),
            mv(Motion::Char(Direction::Right), false, 1)
        );
        assert_eq!(
            r.resolve(down(Key::ArrowUp, Modifiers::NONE), &k),
            mv(Motion::Char(Direction::Up), false, 1)
        );
        assert_eq!(
            r.resolve(down(Key::ArrowDown, Modifiers::NONE), &k),
            mv(Motion::Char(Direction::Down), false, 1)
        );
    }

    #[test]
    fn shift_arrow_keys_extend_base_motions() {
        let mut r = Resolver::new();
        let k = km();
        assert_eq!(
            r.resolve(down(Key::ArrowRight, SHIFT), &k),
            mv(Motion::Char(Direction::Right), true, 1)
        );
        assert_eq!(
            r.resolve(down(Key::ArrowDown, SHIFT), &k),
            mv(Motion::Char(Direction::Down), true, 1)
        );
    }

    #[test]
    fn home_end_are_base_line_edge_motions() {
        let mut r = Resolver::new();
        let k = km();
        assert_eq!(
            r.resolve(down(Key::Home, Modifiers::NONE), &k),
            mv(Motion::LineEdge(Direction::Left), false, 1)
        );
        assert_eq!(
            r.resolve(down(Key::End, SHIFT), &k),
            mv(Motion::LineEdge(Direction::Right), true, 1)
        );
    }

    #[test]
    fn page_keys_request_frontend_page_rows() {
        let mut r = Resolver::new();
        let k = km();
        assert_eq!(
            r.resolve(down(Key::PageUp, Modifiers::NONE), &k),
            Some(Action::MovePage {
                direction: Direction::Up,
                extend: false,
                rows: 0,
            })
        );
        assert_eq!(
            r.resolve(down(Key::PageDown, SHIFT), &k),
            Some(Action::MovePage {
                direction: Direction::Down,
                extend: true,
                rows: 0,
            })
        );
    }

    #[test]
    fn alt_w_moves_up_not_types() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        assert_eq!(
            r.resolve(down(Key::Char('w'), ALT), &k),
            mv(Motion::Char(Direction::Up), false, 1)
        );
    }

    #[test]
    fn alt_shift_w_extends_and_normalizes_case() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT_SHIFT), &k);
        // The frontend may report the shifted 'W'; commands are case-insensitive.
        assert_eq!(
            r.resolve(down(Key::Char('W'), ALT_SHIFT), &k),
            mv(Motion::Char(Direction::Up), true, 1)
        );
    }

    #[test]
    fn releasing_alt_returns_to_typing_instantly() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('w'), ALT), &k); // moved up
        r.resolve(mods_changed(Modifiers::NONE), &k);
        assert_eq!(
            r.resolve(down(Key::Char('w'), Modifiers::NONE), &k),
            Some(Action::InsertChar('w'))
        );
    }

    #[test]
    fn focus_lost_clears_all_input_state() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('5'), ALT), &k);
        assert_eq!(r.resolve(InputEvent::FocusLost, &k), None);
        assert_eq!(r.held(), Modifiers::NONE);
        assert_eq!(r.pending_count(), 0);
        assert_eq!(r.find_state(), FindState::Inactive);
    }

    #[test]
    fn key_up_is_ignored() {
        let mut r = Resolver::new();
        assert_eq!(
            r.resolve(
                InputEvent::KeyUp {
                    key: Key::Char('a'),
                    mods: Modifiers::NONE
                },
                &km()
            ),
            None
        );
    }

    #[test]
    fn count_repeats_next_motion_then_clears() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        assert_eq!(r.resolve(down(Key::Char('5'), ALT), &k), None);
        assert_eq!(r.pending_count(), 5);
        assert_eq!(
            r.resolve(down(Key::Char('s'), ALT), &k),
            mv(Motion::Char(Direction::Down), false, 5)
        );
        assert_eq!(r.pending_count(), 0);
    }

    #[test]
    fn count_accumulates_digits() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('1'), ALT), &k);
        r.resolve(down(Key::Char('2'), ALT), &k);
        assert_eq!(r.pending_count(), 12);
        assert_eq!(
            r.resolve(down(Key::Char('w'), ALT), &k),
            mv(Motion::Char(Direction::Up), false, 12)
        );
    }

    #[test]
    fn count_accumulates_a_trailing_zero() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('1'), ALT), &k);
        r.resolve(down(Key::Char('0'), ALT), &k);
        assert_eq!(r.pending_count(), 10);
        assert_eq!(
            r.resolve(down(Key::Char('w'), ALT), &k),
            mv(Motion::Char(Direction::Up), false, 10)
        );
    }

    #[test]
    fn count_accumulates_two_hundred() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        for d in ['2', '0', '0'] {
            r.resolve(down(Key::Char(d), ALT), &k);
        }
        assert_eq!(r.pending_count(), 200);
    }

    #[test]
    fn a_leading_zero_is_not_a_count_digit() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        assert_eq!(r.resolve(down(Key::Char('0'), ALT), &k), None);
        assert_eq!(r.pending_count(), 0);
    }

    #[test]
    fn entering_find_clears_a_pending_count() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('5'), ALT), &k); // count 5
        r.resolve(down(Key::Char('f'), ALT), &k); // begin find -> clears count
        assert_eq!(r.pending_count(), 0);
        r.resolve(down(Key::Char('x'), ALT), &k); // find target
                                                  // A later motion uses count 1, not the leaked 5.
        assert_eq!(
            r.resolve(down(Key::Char('w'), ALT), &k),
            mv(Motion::Char(Direction::Up), false, 1)
        );
    }

    #[test]
    fn a_long_digit_run_saturates_instead_of_overflowing() {
        // A held/auto-repeated digit feeds many Alt+digit events. The count must
        // clamp instead of overflowing usize (which panics in debug builds — an
        // input-reachable panic violates the no-panic rule).
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        for _ in 0..40 {
            assert_eq!(r.resolve(down(Key::Char('9'), ALT), &k), None);
        }
        // It saturated rather than wrapping to a small/garbage value or panicking.
        assert_eq!(r.pending_count(), usize::MAX);
        // The clamped count still drives a motion that simply stops at the edge.
        assert_eq!(
            r.resolve(down(Key::Char('d'), ALT), &k),
            mv(Motion::Char(Direction::Right), false, usize::MAX)
        );
    }

    #[test]
    fn releasing_alt_clears_count() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('5'), ALT), &k);
        r.resolve(mods_changed(Modifiers::NONE), &k);
        assert_eq!(r.pending_count(), 0);
    }

    #[test]
    fn find_then_repeat_next_and_prev() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        assert_eq!(r.resolve(down(Key::Char('f'), ALT), &k), None);
        assert_eq!(r.find_state(), FindState::Pending);
        assert_eq!(
            r.resolve(down(Key::Char('x'), ALT), &k),
            Some(Action::FindChar { ch: 'x' })
        );
        assert_eq!(r.find_state(), FindState::Active { ch: 'x' });
        assert_eq!(
            r.resolve(down(Key::Char('d'), ALT), &k),
            Some(Action::FindRepeat {
                ch: 'x',
                forward: true
            })
        );
        assert_eq!(
            r.resolve(down(Key::Char('a'), ALT), &k),
            Some(Action::FindRepeat {
                ch: 'x',
                forward: false
            })
        );
    }

    #[test]
    fn find_target_can_be_any_char_even_a_digit() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('f'), ALT), &k);
        // After Alt+F the next key is the literal target, not a count.
        assert_eq!(
            r.resolve(down(Key::Char('5'), ALT), &k),
            Some(Action::FindChar { ch: '5' })
        );
        assert_eq!(r.pending_count(), 0);
    }

    #[test]
    fn find_ends_on_alt_release() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('f'), ALT), &k);
        r.resolve(down(Key::Char('x'), ALT), &k);
        r.resolve(mods_changed(Modifiers::NONE), &k);
        assert_eq!(r.find_state(), FindState::Inactive);
    }

    #[test]
    fn find_ends_on_other_key_which_is_then_handled() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('f'), ALT), &k);
        r.resolve(down(Key::Char('x'), ALT), &k); // active
                                                  // 'w' is non-A/D: it ends find AND is handled as a motion.
        assert_eq!(
            r.resolve(down(Key::Char('w'), ALT), &k),
            mv(Motion::Char(Direction::Up), false, 1)
        );
        assert_eq!(r.find_state(), FindState::Inactive);
    }

    #[test]
    fn ctrl_z_resolves_to_undo() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(CTRL), &k);
        assert_eq!(
            r.resolve(down(Key::Char('z'), CTRL), &k),
            Some(Action::Undo)
        );
    }

    #[test]
    fn ctrl_a_c_x_v_resolve_to_standard_editor_actions() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(CTRL), &k);
        assert_eq!(
            r.resolve(down(Key::Char('a'), CTRL), &k),
            Some(Action::SelectAll)
        );
        assert_eq!(
            r.resolve(down(Key::Char('c'), CTRL), &k),
            Some(Action::Copy)
        );
        assert_eq!(r.resolve(down(Key::Char('x'), CTRL), &k), Some(Action::Cut));
        assert_eq!(
            r.resolve(down(Key::Char('v'), CTRL), &k),
            Some(Action::Paste)
        );
    }

    #[test]
    fn ctrl_y_resolves_to_redo() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(CTRL), &k);
        assert_eq!(
            r.resolve(down(Key::Char('y'), CTRL), &k),
            Some(Action::Redo)
        );
    }

    #[test]
    fn ctrl_shift_z_resolves_to_redo() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(CTRL_SHIFT), &k);
        // The frontend may report the shifted 'Z'; commands are case-insensitive,
        // and the held ctrl+shift selects the redo layer.
        assert_eq!(
            r.resolve(down(Key::Char('Z'), CTRL_SHIFT), &k),
            Some(Action::Redo)
        );
    }

    #[test]
    fn insert_text_event_resolves_to_insert_text_action() {
        // InsertText is not a keystroke: it carries external text (paste / IME)
        // straight through to the executor, with no modifier/layer logic.
        let mut r = Resolver::new();
        assert_eq!(
            r.resolve(InputEvent::InsertText("hi".to_string()), &km()),
            Some(Action::InsertText("hi".to_string()))
        );
    }

    #[test]
    fn unbound_modified_key_is_none_and_clears_count() {
        let mut r = Resolver::new();
        let k = km();
        r.resolve(mods_changed(ALT), &k);
        r.resolve(down(Key::Char('3'), ALT), &k); // count = 3
                                                  // 'j' is unbound in the Alt layer.
        assert_eq!(r.resolve(down(Key::Char('j'), ALT), &k), None);
        assert_eq!(r.pending_count(), 0);
    }
}
