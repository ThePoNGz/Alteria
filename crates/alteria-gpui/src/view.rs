//! The root view: owns the [`Editor`], holds focus, routes raw OS key and
//! modifier events into the engine, and tracks the vertical scroll offset.
//!
//! This is the seam where the quasimode bet is wired up. Keys arrive through the
//! **raw** `on_key_down` / `on_key_up` / `on_modifiers_changed` handlers — never
//! GPUI's Action/`KeyBinding` system, which cannot represent "a modifier is held
//! with no key" (CLAUDE.md hard rule). Each event is translated by [`event`] and
//! handed to [`Editor::handle`]; we `cx.notify()` (redraw) only when the engine
//! says state changed. Focus loss feeds [`InputEvent::FocusLost`] so a held Alt
//! can never get stuck — the same focus wiring Zed uses (`cx.on_blur` /
//! `on_focus_out` in `editor/src/editor.rs`).
//!
//! Scrolling lives here too (plan 007): [`EditorView::scroll_top`] is a plain
//! pixel offset, moved by the mouse wheel (`on_scroll_wheel`, an ordinary handler
//! — *not* a quasimode) and by autoscroll after every state change so the primary
//! cursor stays on screen. The viewport math is the gpui-free [`scroll`] module;
//! this file only converts at the `Pixels` boundary. Mirrors Zed's
//! `editor/src/element/mouse.rs` wheel handler and `scroll/autoscroll.rs`.

use alteria_core::input::InputEvent;
use alteria_core::{Editor, EditorEffect};
use gpui::{
    div, prelude::*, px, rgb, Bounds, ClipboardEntry, ClipboardItem, Context, FocusHandle,
    KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, Pixels, ScrollWheelEvent, Window,
};

use crate::event;
use crate::text_element::{row_col, TextElement};

// `scroll.rs` is a top-level frontend file (`src/scroll.rs`, per plan 007), but
// `main.rs` is off-limits for this plan, so the module is declared here with an
// explicit `#[path]` rather than at the crate root. `pub(crate)` so the renderer
// (`text_element`) can reach the same viewport math.
#[path = "scroll.rs"]
pub(crate) mod scroll;

/// The single root view for the walking skeleton: one buffer, one focus handle.
pub(crate) struct EditorView {
    pub(crate) editor: Editor,
    pub(crate) focus_handle: FocusHandle,
    /// Vertical scroll offset in pixels (`0` = top of the buffer). The renderer
    /// paints only the rows visible at this offset; the wheel handler and
    /// autoscroll move it, always clamped to `[0, max]` (see [`scroll`]).
    pub(crate) scroll_top: Pixels,
    /// The text element's last painted bounds, cached each frame by the renderer
    /// so autoscroll/wheel know the viewport height — the `last_bounds` pattern
    /// from gpui's `examples/input.rs`. `None` until the first paint.
    pub(crate) last_bounds: Option<Bounds<Pixels>>,
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
            scroll_top: px(0.),
            last_bounds: None,
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.feed(event::from_key_down(ev), window, cx);
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.feed(event::from_key_up(ev), window, cx);
    }

    fn on_modifiers_changed(
        &mut self,
        ev: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.feed(Some(event::from_modifiers_changed(ev)), window, cx);
    }

    /// Mouse wheel → vertical scroll. An ordinary handler (not a quasimode): the
    /// wheel `delta` becomes pixels via `line_height`, is subtracted from
    /// `scroll_top` (scrolling down increases the offset), then clamped to the
    /// content. Sign + clamp mirror Zed's `editor/src/element/mouse.rs`.
    fn on_scroll_wheel(
        &mut self,
        ev: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = window.line_height();
        let delta_y = ev.delta.pixel_delta(line_height).y;
        let line_count = self.line_count();
        let viewport_height = self.viewport_height(window);
        let proposed = f32::from(self.scroll_top - delta_y);
        let new_top = px(scroll::clamp_scroll_top(
            proposed,
            line_count,
            f32::from(line_height),
            f32::from(viewport_height),
        ));
        if new_top != self.scroll_top {
            self.scroll_top = new_top;
            cx.notify();
        }
    }

    /// Hand a translated event to the engine; on a state change, keep the primary
    /// cursor in view, then redraw.
    fn feed(&mut self, input: Option<InputEvent>, window: &Window, cx: &mut Context<Self>) {
        if let Some(input) = input {
            let result = self.editor.handle_result(input);
            let effect_redraw = result
                .effect
                .map(|effect| self.handle_effect(effect, window, cx))
                .unwrap_or(false);
            if result.redraw || effect_redraw {
                self.autoscroll_to_cursor(window);
                cx.notify();
            }
        }
    }

    fn handle_effect(
        &mut self,
        effect: EditorEffect,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match effect {
            EditorEffect::CopyToClipboard { text, selections } => {
                let metadata: Vec<(usize, bool)> = selections
                    .into_iter()
                    .map(|selection| (selection.len, selection.is_entire_line))
                    .collect();
                cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, metadata));
                false
            }
            EditorEffect::ReadClipboardAndPaste => {
                let Some(item) = cx.read_from_clipboard() else {
                    return false;
                };
                let clipboard_string = item.entries().iter().find_map(|entry| match entry {
                    ClipboardEntry::String(s) => Some(s),
                    _ => None,
                });
                let (text, lengths) = match clipboard_string {
                    Some(s) => (
                        s.text().to_string(),
                        s.metadata_json::<Vec<(usize, bool)>>()
                            .map(|metadata| metadata.into_iter().map(|(len, _)| len).collect()),
                    ),
                    None => {
                        let Some(text) = item.text() else {
                            return false;
                        };
                        (text, None)
                    }
                };
                self.editor.paste_clipboard(text, lengths)
            }
            EditorEffect::MovePage { direction, extend } => {
                let rows = self.page_row_count(window);
                self.editor.move_page(direction, extend, rows)
            }
        }
    }

    /// Keep the primary cursor on screen after a motion/edit — exactly as Zed
    /// autoscrolls after a movement (`scroll/autoscroll.rs`). A no-op when the
    /// cursor is already visible or the document is shorter than the viewport.
    fn autoscroll_to_cursor(&mut self, window: &Window) {
        let line_height = window.line_height();
        let text = self.editor.buffer.text();
        let (cursor_row, _) = row_col(&text, self.editor.buffer.primary_resolved().head());
        let viewport_height = self.viewport_height(window);
        self.scroll_top = px(scroll::autoscroll_top(
            cursor_row,
            f32::from(self.scroll_top),
            f32::from(line_height),
            f32::from(viewport_height),
            text.split('\n').count(),
        ));
    }

    /// Viewport height for the scroll math: the renderer's last painted height,
    /// falling back to the window's content height before the first paint.
    fn viewport_height(&self, window: &Window) -> Pixels {
        self.last_bounds
            .map(|bounds| bounds.size.height)
            .unwrap_or_else(|| window.viewport_size().height)
    }

    fn line_count(&self) -> usize {
        self.editor.buffer.text().split('\n').count()
    }

    fn page_row_count(&self, window: &Window) -> usize {
        scroll::page_row_count(
            f32::from(self.viewport_height(window)),
            f32::from(window.line_height()),
        )
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
            // The wheel is an ordinary (non-quasimode) handler.
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(TextElement { view: cx.entity() })
    }
}
