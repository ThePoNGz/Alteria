//! Minimal shim of Zed's `util` crate, exposing only what the vendored `rope`
//! and `text` crates reference — all verbatim from Zed:
//!
//! - **Library surface** (`is_utf8_char_boundary`, `debug_panic!`) — what `rope`
//!   needs from the shipping code.
//! - **`test-support` surface** (`RandomCharIter`, `test::marked_text_ranges`),
//!   gated exactly as upstream — what `text`'s property-test helpers need.
//!
//! Vendoring the real `util` crate would drag in gpui-adjacent dependencies
//! (gpui_util, smol, git2, globset, ...), so we provide just these.

/// Returns whether the given byte is a UTF-8 character boundary (i.e. not a
/// continuation byte). Matches `util::is_utf8_char_boundary` in Zed verbatim.
#[inline]
pub const fn is_utf8_char_boundary(u8: u8) -> bool {
    // This is bit magic equivalent to: b < 128 || b >= 192
    (u8 as i8) >= -0x40
}

/// Panics in debug builds; logs an error (with backtrace) in release builds.
/// Matches the shape of Zed's `debug_panic!` so `rope`'s call sites are unchanged.
#[macro_export]
macro_rules! debug_panic {
    ( $($fmt_arg:tt)* ) => {
        if cfg!(debug_assertions) {
            panic!( $($fmt_arg)* );
        } else {
            let backtrace = std::backtrace::Backtrace::capture();
            log::error!("{}\n{:?}", format_args!($($fmt_arg)*), backtrace);
        }
    };
}

/// Zed's `util::RandomCharIter` (verbatim, `crates/util/src/util.rs`) — the
/// random character source the vendored `text` crate's property-test helpers
/// (`randomly_edit`, `get_random_edits`, …) build their inputs from. Gated
/// exactly as upstream so it appears only under tests / the `test-support`
/// feature; it is the one place `util-shim` pulls in `rand`.
#[cfg(any(test, feature = "test-support"))]
mod rng {
    use rand::prelude::*;

    pub struct RandomCharIter<T: Rng> {
        rng: T,
        simple_text: bool,
    }

    impl<T: Rng> RandomCharIter<T> {
        pub fn new(rng: T) -> Self {
            Self {
                rng,
                simple_text: std::env::var("SIMPLE_TEXT").is_ok_and(|v| !v.is_empty()),
            }
        }

        pub fn with_simple_text(mut self) -> Self {
            self.simple_text = true;
            self
        }
    }

    impl<T: Rng> Iterator for RandomCharIter<T> {
        type Item = char;

        fn next(&mut self) -> Option<Self::Item> {
            if self.simple_text {
                return if self.rng.random_range(0..100) < 5 {
                    Some('\n')
                } else {
                    Some(self.rng.random_range(b'a'..b'z' + 1).into())
                };
            }

            match self.rng.random_range(0..100) {
                // whitespace
                0..=19 => [' ', '\n', '\r', '\t'].choose(&mut self.rng).copied(),
                // two-byte greek letters
                20..=32 => char::from_u32(self.rng.random_range(('α' as u32)..('ω' as u32 + 1))),
                // // three-byte characters
                33..=45 => ['✋', '✅', '❌', '❎', '⭐']
                    .choose(&mut self.rng)
                    .copied(),
                // // four-byte characters
                46..=58 => ['🍐', '🏀', '🍗', '🎉'].choose(&mut self.rng).copied(),
                // ascii letters
                _ => Some(self.rng.random_range(b'a'..b'z' + 1).into()),
            }
        }
    }
}
#[cfg(any(test, feature = "test-support"))]
pub use rng::RandomCharIter;

/// Zed's `util::test` test-support helpers (verbatim subset). Only
/// `marked_text_ranges` is reproduced — the single item the vendored `text`
/// crate references (via `edit_via_marked_text`).
#[cfg(any(test, feature = "test-support"))]
pub mod test;
