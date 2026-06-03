//! Alteria's first frontend: a GPUI window over the `alteria-core` engine.
//!
//! `main` is deliberately thin (CLAUDE.md): it loads content, opens one window
//! whose root is an [`EditorView`], focuses it, and lets the raw event handlers
//! pump the engine. All editing logic lives in the gpui-free core; this binary
//! only translates OS events in and renders engine state out. The entry point is
//! `gpui_platform::application()` (not the older `App::new()`), verified against
//! the pinned GPUI rev.

mod event;
mod text_element;
mod view;

use gpui::{px, size, App, AppContext, Bounds, WindowBounds, WindowOptions};
use gpui_platform::application;

use crate::view::EditorView;

/// Opened when no file argument is given — enough text to exercise the bet.
const SCRATCH: &str = "\
Alteria — hold Alt + WASD to move (inverted-T navigation).
Q/E word-jump · Z/C line edges · R matching bracket.
Release Alt and just type: this is an ordinary editor with no mode to learn.
Alt+Shift + a motion extends the selection; Esc drops it; Ctrl+Z undoes.

edit me.
";

fn main() {
    // Content source: argv[1] as a file, else the scratch buffer. Read-only
    // display in this slice (no save). `Buffer` normalizes line endings on load.
    let content = std::env::args()
        .nth(1)
        .and_then(|path| match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("alteria: could not read {path}: {e}; opening scratch buffer");
                None
            }
        })
        .unwrap_or_else(|| SCRATCH.to_string());

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| EditorView::new(&content, window, cx)),
        ) {
            Ok(window) => {
                // Focus the root so key events arrive, and bring the app forward.
                window
                    .update(cx, |view, window, cx| {
                        window.focus(&view.focus_handle, cx);
                        cx.activate(true);
                    })
                    .ok();
            }
            Err(e) => {
                eprintln!("alteria: failed to open window: {e}");
                cx.quit();
            }
        }
    });
}
