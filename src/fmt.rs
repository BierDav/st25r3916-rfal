//! Crate-internal logging macros: `trace!`, `debug!`, `info!`, `warn!`, `error!`.
//!
//! `lib.rs` declares this module first with `#[macro_use]`, so every other
//! module can call the macros without importing them (embassy style). They
//! are deliberately not importable by path: re-exporting a `macro_rules!`
//! named `warn` (`pub(crate) use warn;`) is ambiguous with the built-in
//! `#[warn]` attribute (E0659).
//!
//! Backend, chosen at compile time:
//!
//! | Features            | Backend |
//! |---------------------|---------|
//! | `defmt` (± `log`)   | `defmt` (with both features on, defmt wins; no `compile_error!`) |
//! | `log` only          | `log` (target = the module path, as usual) |
//! | neither             | nothing; the arguments are only borrowed, so no unused warnings |
//!
//! Rules for call sites, so every backend compiles:
//! - The first argument is a string literal, followed by plain expressions
//!   (no `target:`, no named `{name}` captures, no `{name}` inline arguments).
//! - Use only `{}` and `{:?}`. With `defmt` the arguments must implement
//!   `defmt::Format`; with `log`, `Display` (`{}`) or `Debug` (`{:?}`). The
//!   public types of this crate implement all of them, integers and `&str`
//!   too. For anything else, log a primitive derived from it.
//! - Prefix nothing: the `log`/defmt location already identifies the crate.
#![allow(unused_macros)]

macro_rules! trace {
    ($s:literal $(, $x:expr)* $(,)?) => {
        cfg_select! {
            feature = "defmt" => { ::defmt::trace!($s $(, $x)*) }
            feature = "log" => { ::log::trace!($s $(, $x)*) }
            _ => { { let _ = ($( & $x ),*); } }
        }
    };
}

macro_rules! debug {
    ($s:literal $(, $x:expr)* $(,)?) => {
        cfg_select! {
            feature = "defmt" => { ::defmt::debug!($s $(, $x)*) }
            feature = "log" => { ::log::debug!($s $(, $x)*) }
            _ => { { let _ = ($( & $x ),*); } }
        }
    };
}

macro_rules! info {
    ($s:literal $(, $x:expr)* $(,)?) => {
        cfg_select! {
            feature = "defmt" => { ::defmt::info!($s $(, $x)*) }
            feature = "log" => { ::log::info!($s $(, $x)*) }
            _ => { { let _ = ($( & $x ),*); } }
        }
    };
}

macro_rules! warn {
    ($s:literal $(, $x:expr)* $(,)?) => {
        cfg_select! {
            feature = "defmt" => { ::defmt::warn!($s $(, $x)*) }
            feature = "log" => { ::log::warn!($s $(, $x)*) }
            _ => { { let _ = ($( & $x ),*); } }
        }
    };
}

macro_rules! error {
    ($s:literal $(, $x:expr)* $(,)?) => {
        cfg_select! {
            feature = "defmt" => { ::defmt::error!($s $(, $x)*) }
            feature = "log" => { ::log::error!($s $(, $x)*) }
            _ => { { let _ = ($( & $x ),*); } }
        }
    };
}
