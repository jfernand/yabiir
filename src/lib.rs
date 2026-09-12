mod api;
// `datafile`, `format`, and `keydir` are internal building blocks, not
// stable public API — made `pub` only so `benches/*.rs` (a separate crate)
// can reach them directly instead of duplicating their code.
pub mod datafile;
mod engine;
mod error;
pub mod format;
pub mod keydir;
