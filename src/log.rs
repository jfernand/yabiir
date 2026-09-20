//! Tiny internal shim so the crate's warning-level messages go through
//! `tracing::warn!` when the optional `tracing` feature is enabled, and
//! `eprintln!` otherwise — `docs/ROADMAP.md` §4's structured-logging item.
//! Centralized here so call sites (`recovery.rs`, `merge/mod.rs`) don't
//! each need their own `#[cfg(...)]` branch — they just call
//! `crate::log::warn!(...)` with the same arguments either way.

#[cfg(feature = "tracing")]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        tracing::warn!($($arg)*)
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        eprintln!($($arg)*)
    };
}

pub(crate) use log_warn as warn;
