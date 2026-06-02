//! The root view: owns the [`Editor`], holds focus, and routes raw OS key and
//! modifier events into the engine.
//!
//! This is the seam where the quasimode bet is wired up. Keys arrive through the
//! **raw** `on_key_down` / `on_key_up` / `on_modifiers_changed` handlers — never
//! GPUI's Action/`KeyBinding` system, which cannot represent "a modifier is held
//! with no key" (CLAUDE.md hard rule). Each event is translated by [`event`] and
//! handed to [`Editor::handle`]; we `cx.notify()` (redraw) only when the engine
//! says state changed. Focus loss feeds [`InputEvent::FocusLost`] so a held Alt
//! can never get stuck — the same focus wiring Zed uses (`cx.on_blur` /
//! `on_focus_out` in `editor/src/editor.rs`).

use alteria_core::input::InputEvent;
use alteria_core::Editor;
use gpui::{
    div, prelude::*, px, rgb, Context, FocusHandle, KeyDownEvent, KeyUpEvent,
    ModifiersChangedEvent, Window,
};

use crate::event;
use crate::text_element::TextElement;

/// The single root view for the walking skeleton: one buffer, one focus handle.
pub(crate) struct EditorView {
    pub(crate) editor: Editor,
    pub(crate) focus_handle: FocusHandle,
}

impl EditorView {
    pub(crate) fn new(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // The no-stuck-quasimode guarantee at the frontend boundary: if focus
        // leaves the editor, tell the engine so the resolver clears `held` and
        // any Alt-gated count/find state.
        cx.on_blur(&focus_handle, window, |this, _window, cx| {
            if this.editor.handle(InputEvent::FocusLost) {
                cx.notify();
            }
        })
        .detach();
        EditorView {
            editor: Editor::new(text),
            focus_handle,
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.feed(event::from_key_down(ev), cx);
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.feed(event::from_key_up(ev), cx);
    }

    fn on_modifiers_changed(
        &mut self,
        ev: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.feed(Some(event::from_modifiers_changed(ev)), cx);
    }

    /// Hand a translated event to the engine and redraw only if it changed state.
    fn feed(&mut self, input: Option<InputEvent>, cx: &mut Context<Self>) {
        if let Some(input) = input {
            if self.editor.handle(input) {
                cx.notify();
            }
        }
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .font_family("monospace")
            .text_size(px(16.))
            // Quasimodes ride these RAW handlers — see the module docs.
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .child(TextElement { view: cx.entity() })
    }
}
