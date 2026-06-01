//! Minimal shim of Zed's `util` crate, exposing only the two items the vendored
//! `rope` crate references from the library code: `is_utf8_char_boundary` and the
//! `debug_panic!` macro. Vendoring the real `util` crate would drag in gpui-adjacent
//! dependencies (gpui_util, smol, git2, globset, ...), so we provide just these.

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
