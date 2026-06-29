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

use std::ops::Range;

use alteria_core::buffer::ClipboardSelection;
use alteria_core::input::InputEvent;
use alteria_core::{Editor, EditorEffect};
use gpui::{
    div, prelude::*, px, rgb, Bounds, ClipboardEntry, ClipboardItem, Context, EntityInputHandler,
    FocusHandle, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, UTF16Selection, Window,
};

use crate::event;
use crate::text_element::{line_at_row, offset_for_point, row_col, shape_plain_line, TextElement};

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
    /// Drag anchor in buffer byte offsets while a left-button text selection is
    /// in progress.
    mouse_anchor: Option<usize>,
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
            mouse_anchor: None,
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

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let Some(offset) = self.offset_for_mouse(ev.position, window) else {
            return;
        };

        let changed = if ev.modifiers.shift {
            self.mouse_anchor = Some(self.editor.buffer.primary_resolved().tail());
            self.editor.extend_primary_to(offset)
        } else {
            self.mouse_anchor = Some(offset);
            self.editor.set_cursor(offset)
        };

        if changed {
            self.autoscroll_to_cursor(window);
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(anchor) = self.mouse_anchor else {
            return;
        };
        let Some(offset) = self.offset_for_mouse(ev.position, window) else {
            return;
        };
        if self.editor.set_primary_range(anchor, offset) {
            self.autoscroll_to_cursor(window);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _ev: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.mouse_anchor = None;
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
                let (text, metadata) = match clipboard_string {
                    Some(s) => (
                        s.text().to_string(),
                        s.metadata_json::<Vec<(usize, bool)>>().map(|metadata| {
                            metadata
                                .into_iter()
                                .map(|(len, is_entire_line)| ClipboardSelection {
                                    len,
                                    is_entire_line,
                                })
                                .collect()
                        }),
                    ),
                    None => {
                        let Some(text) = item.text() else {
                            return false;
                        };
                        (text, None)
                    }
                };
                self.editor.paste_clipboard(text, metadata)
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

    fn offset_for_mouse(
        &self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        Some(offset_for_point(
            &self.editor.buffer.text(),
            position,
            bounds,
            self.scroll_top,
            window.line_height(),
            window,
        ))
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { view: cx.entity() })
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.editor.buffer.text();
        let range = utf16_range_to_byte_range(&text, range_utf16);
        actual_range.replace(byte_range_to_utf16_range(&text, range.clone()));
        Some(text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.editor.buffer.text();
        let selection = self.editor.buffer.primary_resolved();
        Some(UTF16Selection {
            range: byte_range_to_utf16_range(&text, selection.min()..selection.max()),
            reversed: selection.reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(range_utf16) = range_utf16 {
            let buffer_text = self.editor.buffer.text();
            let range = utf16_range_to_byte_range(&buffer_text, range_utf16);
            self.editor.set_primary_range(range.start, range.end);
        }
        if self.editor.handle(InputEvent::InsertText(text.to_string())) {
            self.autoscroll_to_cursor(window);
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // Full marked-text preedit is deferred. Committed text enters through
        // `replace_text_in_range`, keeping the real edit path unified.
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let text = self.editor.buffer.text();
        let range = utf16_range_to_byte_range(&text, range_utf16);
        let (row, start_col) = row_col(&text, range.start);
        let (_, end_col) = row_col(&text, range.end);
        let (_, line) = line_at_row(&text, row);
        let shaped = shape_plain_line(line, window);
        let line_height = window.line_height();
        let y = element_bounds.top() + line_height * row as f32 - self.scroll_top;
        Some(Bounds::from_corners(
            gpui::point(element_bounds.left() + shaped.x_for_index(start_col), y),
            gpui::point(
                element_bounds.left() + shaped.x_for_index(end_col),
                y + line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let text = self.editor.buffer.text();
        let offset = offset_for_point(
            &text,
            point,
            bounds,
            self.scroll_top,
            window.line_height(),
            window,
        );
        Some(byte_offset_to_utf16_offset(&text, offset))
    }
}

fn utf16_range_to_byte_range(text: &str, range: Range<usize>) -> Range<usize> {
    utf16_offset_to_byte_offset(text, range.start)..utf16_offset_to_byte_offset(text, range.end)
}

fn byte_range_to_utf16_range(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_to_utf16_offset(text, range.start)..byte_offset_to_utf16_offset(text, range.end)
}

fn utf16_offset_to_byte_offset(text: &str, target: usize) -> usize {
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        if utf16 >= target {
            return byte;
        }
        utf16 += ch.len_utf16();
        if utf16 > target {
            return byte;
        }
    }
    text.len()
}

fn byte_offset_to_utf16_offset(text: &str, target: usize) -> usize {
    let target = target.min(text.len());
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        if target <= byte {
            return utf16;
        }
        if target < byte + ch.len_utf8() {
            return utf16;
        }
        utf16 += ch.len_utf16();
    }
    utf16
}

#[cfg(test)]
mod tests {
    use super::{byte_offset_to_utf16_offset, utf16_offset_to_byte_offset};

    #[test]
    fn utf16_to_byte_handles_bmp_and_astral_chars() {
        let text = "a💙b";
        assert_eq!(utf16_offset_to_byte_offset(text, 0), 0);
        assert_eq!(utf16_offset_to_byte_offset(text, 1), 1);
        assert_eq!(utf16_offset_to_byte_offset(text, 2), 1);
        assert_eq!(utf16_offset_to_byte_offset(text, 3), 5);
        assert_eq!(utf16_offset_to_byte_offset(text, 4), 6);
    }

    #[test]
    fn byte_to_utf16_counts_code_units() {
        let text = "a💙b";
        assert_eq!(byte_offset_to_utf16_offset(text, 0), 0);
        assert_eq!(byte_offset_to_utf16_offset(text, 1), 1);
        assert_eq!(byte_offset_to_utf16_offset(text, 2), 1);
        assert_eq!(byte_offset_to_utf16_offset(text, 5), 3);
        assert_eq!(byte_offset_to_utf16_offset(text, 6), 4);
    }
}
