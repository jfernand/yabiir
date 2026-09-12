mod api;
// `datafile`, `format`, and `keydir` are internal building blocks, not
// stable public API — made `pub` only so `benches/*.rs` (a separate crate)
// can reach them directly instead of duplicating their code.
pub mod datafile;
mod engine;
mod error;
pub mod format;
pub mod keydir;
mod merge;
mod recovery;

// The actual public API: everything a consumer of this crate (e.g.
// src/bin/e.rs) needs to open and use a Bitcask datastore, re-exported flat
// at the crate root rather than requiring callers to know the internal
// module layout.
pub use api::{Bitcask, Options};
pub use engine::Engine;
pub use error::{Error, Result};
