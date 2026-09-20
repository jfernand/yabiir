mod api;
mod commit;
// `datafile`, `format`, and `keydir` are internal building blocks, not
// stable public API — made `pub` only so `benches/*.rs` (a separate crate)
// can reach them directly instead of duplicating their code.
pub mod datafile;
mod engine;
mod error;
pub mod format;
pub mod keydir;
mod lock;
mod merge;
mod recovery;

use std::time::{SystemTime, UNIX_EPOCH};
// The actual public API: everything a consumer of this crate (e.g.
// src/bin/e.rs) needs to open and use a Bitcask datastore, re-exported flat
// at the crate root rather than requiring callers to know the internal
// module layout.
pub use api::{Bitcask, Options};
pub use engine::Engine;
pub use error::{Error, Result};

pub fn now_unix() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32
}

// This test is BS; it is just here to satisfy mutant testing.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_unix_returns_a_plausible_current_timestamp() {
        // Loosely bounded, not exact: catches a stub returning 0/1 (or any
        // other constant) without being sensitive to test execution speed.
        // 1_700_000_000 is 2023-11-14; comfortably in the past of any real
        // test run without hardcoding "now".
        let t = now_unix();
        assert!(t > 1_700_000_000, "now_unix() returned implausibly small {t}");
    }
}
