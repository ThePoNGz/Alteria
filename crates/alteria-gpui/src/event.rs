//! Translate GPUI's raw OS events into the core's plain-data [`InputEvent`]s.
//!
//! This is the only place a `gpui` type meets an `alteria_core` type — the
//! frontend boundary. The core never sees a GPUI event; the GPUI layer never
//! reaches into the engine's logic. Quasimodes ride raw `on_key_down` /
//! `on_key_up` / `on_modifiers_changed` (see `view.rs`), never GPUI's Action
//! system — that system cannot express "a modifier is held with no key", which
//! is the whole concept (CLAUDE.md hard rule).
//!
//! The subtle part is `Keystroke -> Key`, and it is factored into the pure
//! [`translate_key`] (over plain `&str`/`bool`s) so it is unit-testable without
//! constructing any GPUI value. The gpui-typed wrappers below just unpack a
//! `Keystroke`/`Modifiers` and call into it.

use alteria_core::input::{InputEvent, Key, Modifiers};
use gpui::{KeyDownEvent, KeyUpEvent, Modifiers as GpuiModifiers, ModifiersChangedEvent};

/// Map GPUI's modifier flags onto the core's. `platform` (cmd/win/super) becomes
/// `super_key`; `function` and capslock are ignored — the engine has no use for
/// them. The two structs are kept deliberately separate so the core stays free
/// of any GPUI type.
pub(crate) fn map_modifiers(m: &GpuiModifiers) -> Modifiers {
    Modifiers {
        alt: m.alt,
        ctrl: m.control,
        shift: m.shift,
        super_key: m.platform,
    }
}

/// True while a layer-selecting modifier is held. Shift alone is *not* one of
/// these: it stays on the Base layer and only changes which character is typed
/// (handled by `key_char`), mirroring the resolver's own Base-layer test.
fn on_modifier_layer(mods: Modifiers) -> bool {
    mods.alt || mods.ctrl || mods.super_key
}

/// Pure `Keystroke -> Key` translation — the crux of the layering.
///
/// * Named non-printing keys (`backspace`/`enter`/`return`/`escape`) map to
///   their variants regardless of layer.
/// * **Base layer** (no Alt/Ctrl/Super): prefer `key_char`, the character that
///   would actually be typed, so Shift→uppercase, symbols, and dead keys insert
///   correctly. Fall back to the physical `key` label when there is no
///   `key_char` (it is a single char).
/// * **Modifier layers**: use the physical `key` label (`"w"`), because the
///   keymap/resolver dispatch on `(held_mods, Key::Char('w'))` — the printed
///   character under e.g. Alt+S (`"ß"`) must be ignored.
///
/// Unmapped keys return `None` (dropped).
fn translate_key(key: &str, key_char: Option<&str>, mods: Modifiers) -> Option<Key> {
    match key {
        "backspace" => return Some(Key::Backspace),
        "delete" => return Some(Key::Delete),
        "enter" | "return" => return Some(Key::Enter),
        "escape" => return Some(Key::Escape),
        "up" => return Some(Key::ArrowUp),
        "down" => return Some(Key::ArrowDown),
        "left" => return Some(Key::ArrowLeft),
        "right" => return Some(Key::ArrowRight),
        "home" => return Some(Key::Home),
        "end" => return Some(Key::End),
        "pageup" => return Some(Key::PageUp),
        "pagedown" => return Some(Key::PageDown),
        _ => {}
    }

    if !on_modifier_layer(mods) {
        // Base layer: the character that would be typed (carries Shift/symbols).
        if let Some(c) = key_char.and_then(single_char) {
            return Some(Key::Char(c));
        }
    }
    // Modifier layers (and Base-layer fallback): the physical keycap label.
    single_char(key).map(Key::Char)
}

/// A `&str` that is exactly one `char`, else `None`. Named keys like `"backspace"`
/// or layout strings are rejected; only a lone keycap/character passes.
fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// A `KeyDownEvent` becomes a core `KeyDown`; `is_held` is the engine's `repeat`.
/// Returns `None` for a key the engine has no mapping for (dropped, no redraw).
pub(crate) fn from_key_down(ev: &KeyDownEvent) -> Option<InputEvent> {
    let mods = map_modifiers(&ev.keystroke.modifiers);
    if !on_modifier_layer(mods)
        && ev
            .keystroke
            .key_char
            .as_deref()
            .and_then(single_char)
            .is_some()
    {
        return None;
    }
    let key = translate_key(&ev.keystroke.key, ev.keystroke.key_char.as_deref(), mods)?;
    Some(InputEvent::KeyDown {
        key,
        mods,
        repeat: ev.is_held,
    })
}

/// A `KeyUpEvent` becomes a core `KeyUp`. The resolver currently ignores key-up
/// (it is a no-op that never redraws), but the event is forwarded for symmetry
/// and so future bindings can observe releases.
pub(crate) fn from_key_up(ev: &KeyUpEvent) -> Option<InputEvent> {
    let mods = map_modifiers(&ev.keystroke.modifiers);
    let key = translate_key(&ev.keystroke.key, ev.keystroke.key_char.as_deref(), mods)?;
    Some(InputEvent::KeyUp { key, mods })
}

/// A `ModifiersChangedEvent` becomes a core `ModifiersChanged` — **the primitive
/// the whole quasimode concept rides on.** It fires whenever the held-modifier
/// set changes, independent of any other key, so the resolver always knows which
/// layer is active and can clear Alt-gated state the instant Alt is released.
pub(crate) fn from_modifiers_changed(ev: &ModifiersChangedEvent) -> InputEvent {
    InputEvent::ModifiersChanged {
        mods: map_modifiers(&ev.modifiers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;
    const ALT: Modifiers = Modifiers {
        alt: true,
        ..Modifiers::NONE
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    // ---- the pure key translation (no GPUI types) ----------------------

    #[test]
    fn base_layer_prefers_the_typed_character() {
        // 'a' typed: physical key "a", key_char "a" -> Char('a').
        assert_eq!(translate_key("a", Some("a"), NONE), Some(Key::Char('a')));
    }

    #[test]
    fn shift_capitalizes_via_key_char_and_stays_base_layer() {
        // Shift+a: the typed char is "A"; Shift is not a layer modifier, so the
        // uppercase character is what inserts.
        assert_eq!(translate_key("a", Some("A"), SHIFT), Some(Key::Char('A')));
    }

    #[test]
    fn alt_layer_uses_the_physical_keycap_not_key_char() {
        // Alt+S on some layouts types "ß"; the resolver wants the keycap 's'.
        assert_eq!(translate_key("s", Some("ß"), ALT), Some(Key::Char('s')));
        // Alt+W navigates: keycap 'w' regardless of any composed key_char.
        assert_eq!(translate_key("w", Some("w"), ALT), Some(Key::Char('w')));
    }

    #[test]
    fn named_keys_map_in_any_layer() {
        for mods in [NONE, ALT] {
            assert_eq!(translate_key("backspace", None, mods), Some(Key::Backspace));
            assert_eq!(translate_key("delete", None, mods), Some(Key::Delete));
            assert_eq!(translate_key("enter", None, mods), Some(Key::Enter));
            assert_eq!(translate_key("return", None, mods), Some(Key::Enter));
            assert_eq!(translate_key("escape", None, mods), Some(Key::Escape));
            assert_eq!(translate_key("up", None, mods), Some(Key::ArrowUp));
            assert_eq!(translate_key("down", None, mods), Some(Key::ArrowDown));
            assert_eq!(translate_key("left", None, mods), Some(Key::ArrowLeft));
            assert_eq!(translate_key("right", None, mods), Some(Key::ArrowRight));
            assert_eq!(translate_key("home", None, mods), Some(Key::Home));
            assert_eq!(translate_key("end", None, mods), Some(Key::End));
            assert_eq!(translate_key("pageup", None, mods), Some(Key::PageUp));
            assert_eq!(translate_key("pagedown", None, mods), Some(Key::PageDown));
        }
    }

    #[test]
    fn base_layer_falls_back_to_keycap_when_no_key_char() {
        // No reported character (e.g. some platform quirk): use the keycap.
        assert_eq!(translate_key("x", None, NONE), Some(Key::Char('x')));
    }

    #[test]
    fn base_layer_printable_keydown_is_left_to_platform_text_input() {
        let ev = KeyDownEvent {
            keystroke: gpui::Keystroke {
                key: "x".into(),
                key_char: Some("x".into()),
                ..Default::default()
            },
            is_held: false,
            prefer_character_input: false,
        };
        assert_eq!(from_key_down(&ev), None);
    }

    #[test]
    fn modifier_layer_printable_keydown_still_reaches_the_resolver() {
        let ev = KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers: GpuiModifiers {
                    alt: true,
                    ..Default::default()
                },
                key: "w".into(),
                key_char: Some("w".into()),
            },
            is_held: false,
            prefer_character_input: false,
        };
        assert_eq!(
            from_key_down(&ev),
            Some(InputEvent::KeyDown {
                key: Key::Char('w'),
                mods: ALT,
                repeat: false,
            })
        );
    }

    #[test]
    fn unmapped_multichar_key_is_dropped() {
        assert_eq!(translate_key("f1", None, NONE), None);
        assert_eq!(translate_key("tab", None, ALT), None);
    }

    #[test]
    fn digits_pass_through_the_modifier_layer_as_keycaps() {
        // Alt+3 (a repeat count): keycap '3' even if the layout composes a symbol.
        assert_eq!(translate_key("3", Some("£"), ALT), Some(Key::Char('3')));
    }

    // ---- the modifier mapping (platform -> super_key, function ignored) -

    #[test]
    fn modifier_mapping_matches_core_fields() {
        let m = GpuiModifiers {
            control: true,
            alt: true,
            shift: true,
            platform: true,
            function: true, // ignored
        };
        assert_eq!(
            map_modifiers(&m),
            Modifiers {
                alt: true,
                ctrl: true,
                shift: true,
                super_key: true,
            }
        );
    }

    #[test]
    fn platform_becomes_super_key() {
        let m = GpuiModifiers {
            platform: true,
            ..Default::default()
        };
        assert_eq!(map_modifiers(&m).super_key, true);
        assert!(map_modifiers(&m).alt == false);
    }
}
