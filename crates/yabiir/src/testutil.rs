//! Shared by the unit tests.

use std::path::Path;

pub(crate) use yabiir_testutil::TempDir;

use crate::datafile::{Scanned, scan};
use crate::format::Entry;

/// Every intact entry of the data file at `path`, with the offset it starts
/// at. Corrupt and truncated entries are left out.
pub(crate) fn entries_of(path: &Path) -> Vec<(u64, Entry)> {
    scan(path)
        .unwrap()
        .filter_map(|scanned| match scanned.unwrap() {
            Scanned::Entry { offset, entry } => Some((offset, entry)),
            _ => None,
        })
        .collect()
}
