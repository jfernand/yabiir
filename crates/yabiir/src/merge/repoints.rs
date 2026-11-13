//! Keydir changes a merge has decided on but not made yet.

use crate::keydir::{KeydirEntry, SharedKeydir};

/// One key to move from the entry merge copied (`old`) to the copy (`new`).
struct Repoint {
    key: Vec<u8>,
    old: KeydirEntry,
    new: KeydirEntry,
}

/// The repoints for entries merge has written to its output but not flushed.
/// A key must not point at bytes a reader can't see yet, so its repoint
/// waits here until the batch it belongs to is flushed.
#[derive(Default)]
pub(super) struct PendingRepoints(Vec<Repoint>);

impl PendingRepoints {
    pub(super) fn push(&mut self, key: Vec<u8>, old: KeydirEntry, new: KeydirEntry) {
        self.0
            .push(Repoint { key, old, new });
    }

    pub(super) fn len(&self) -> usize {
        self.0
            .len()
    }

    /// Make every pending change, running `before_each` with the key first.
    /// A key is repointed only if its entry is still `old`: if a write
    /// replaced or deleted it since merge copied it, the copy is already
    /// out of date and stays unreferenced until the next merge drops it.
    pub(super) fn apply(&mut self, keydir: &SharedKeydir, before_each: &mut impl FnMut(&[u8])) {
        for Repoint { key, old, new } in self
            .0
            .drain(..)
        {
            before_each(&key);
            keydir.cas_repoint(&key, old, new);
        }
    }
}
