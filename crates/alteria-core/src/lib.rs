//! `alteria-core` — the pure-Rust editing engine for Alteria.
//!
//! This crate is governed by the project's one hard rule: it **never imports
//! `gpui`** (or any rendering/GUI dependency). Every stage of the
//! `InputEvent -> Resolver -> Action -> Executor -> Buffer` pipeline is a pure
//! function over plain data, fully unit-testable without a window.
//!
//! Modules are added one concept per file as the engine is built (see plan 001).
