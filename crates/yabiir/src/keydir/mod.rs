//! The keydir: an in-memory index from each live key to where its newest
//! value is on disk. See `docs/bitcask-implementation-plan.md` §2.
//!
//! [`Keydir`] is the map; [`SharedKeydir`] puts it behind a lock for the
//! engine.

mod shared;
#[cfg(test)]
mod tests;

pub use shared::SharedKeydir;

use std::collections::{BTreeMap, HashMap, btree_map, hash_map};
use std::ops::Bound;

use ahash::RandomState as AHashState;

use crate::api::KeyOrder;

/// Location of one key's newest value: which data file, and where in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeydirEntry {
    pub file_id: u32,
    pub value_size: u32,
    /// Position of the VALUE bytes (past the entry header), not the entry
    /// header itself — this is exactly what a read seeks/preads from.
    pub value_pos: u64,
    pub timestamp: u32,
}

/// The keydir itself. Not thread-safe on its own: see [`SharedKeydir`] for
/// the wrapper the engine uses. Keeps its keys either in
/// an aHash `HashMap` or in a `BTreeMap`, chosen at construction (see
/// [`KeyOrder`]); the notes below are about the hashed one.
///
/// Uses `aHash` instead of the default SipHash: profiling showed a real
/// cost in `get`'s keydir lookup, and aHash is meaningfully cheaper while
/// still keeping good bit diffusion for byte-string keys. `rustc-hash`
/// (`FxHash`) was tried first and rejected: it has essentially no avalanche
/// step, and for sequential/structured string keys (e.g. `key-0000000042`)
/// that collapses almost all keys into a single hash bucket — verified
/// directly (10,000 sequential keys landed in 1 of 16,384 buckets),
/// silently turning every keydir operation into an O(n) scan for a very
/// plausible real workload (auto-incrementing IDs, timestamps, zero-padded
/// counters). aHash's default `RandomState` (what `HashMap::default()`
/// uses) also randomizes its keys on every construction — not just once
/// per process, but a fresh seed for every `RandomState`/`HashMap`
/// instance, sourced from OS randomness (`getrandom`) mixed with a
/// per-instance counter (see `ahash::RandomState::new`'s own docs) — unlike
/// raw, unseeded `FxHash`, so it keeps reasonable hash-flooding resistance
/// too. That's not purely a durability-neutral, zero-downside change like
/// the read-path fix alongside it, so if keys ever come from a genuinely
/// adversarial/untrusted source, re-evaluate. The same per-instance
/// randomization is also why this hasher can't be used as-is for
/// deterministic simulation testing — see `docs/ROADMAP.md` §1.
pub struct Keydir {
    map: Map,
}

/// The two ways the keydir can keep its keys — see [`KeyOrder`]. Every
/// operation is the same `match` on this, so the only cost of supporting both
/// is one branch per call.
enum Map {
    Hashed(HashMap<Box<[u8]>, KeydirEntry, AHashState>),
    Sorted(BTreeMap<Box<[u8]>, KeydirEntry>),
}

/// Iterator over a keydir's `(key, entry)` pairs: in no particular order for a
/// hashed keydir, in key order for a sorted one.
enum Iter<'a> {
    Hashed(hash_map::Iter<'a, Box<[u8]>, KeydirEntry>),
    Sorted(btree_map::Iter<'a, Box<[u8]>, KeydirEntry>),
    Range(btree_map::Range<'a, Box<[u8]>, KeydirEntry>),
    Empty,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a [u8], &'a KeydirEntry);

    fn next(&mut self) -> Option<Self::Item> {
        let (k, v) = match self {
            Iter::Hashed(it) => it.next()?,
            Iter::Sorted(it) => it.next()?,
            Iter::Range(it) => it.next()?,
            Iter::Empty => return None,
        };
        Some((k.as_ref(), v))
    }
}

impl Default for Keydir {
    fn default() -> Self {
        Self::new()
    }
}

impl Keydir {
    /// A hashed keydir — see [`Keydir::with_order`].
    pub fn new() -> Self {
        Self::with_order(KeyOrder::Hashed)
    }

    pub fn with_order(order: KeyOrder) -> Self {
        Self {
            map: match order {
                KeyOrder::Hashed => Map::Hashed(HashMap::default()),
                KeyOrder::Sorted => Map::Sorted(BTreeMap::new()),
            },
        }
    }

    pub fn order(&self) -> KeyOrder {
        match self.map {
            Map::Hashed(_) => KeyOrder::Hashed,
            Map::Sorted(_) => KeyOrder::Sorted,
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<KeydirEntry> {
        match &self.map {
            Map::Hashed(m) => m.get(key),
            Map::Sorted(m) => m.get(key),
        }
        .copied()
    }

    fn get_mut(&mut self, key: &[u8]) -> Option<&mut KeydirEntry> {
        match &mut self.map {
            Map::Hashed(m) => m.get_mut(key),
            Map::Sorted(m) => m.get_mut(key),
        }
    }

    /// Insert or overwrite `key`'s entry unconditionally — last write wins.
    pub fn insert(&mut self, key: &[u8], entry: KeydirEntry) {
        if let Some(slot) = self.get_mut(key) {
            *slot = entry;
            return;
        }
        match &mut self.map {
            Map::Hashed(m) => m.insert(Box::from(key), entry),
            Map::Sorted(m) => m.insert(Box::from(key), entry),
        };
    }

    pub fn remove(&mut self, key: &[u8]) -> Option<KeydirEntry> {
        match &mut self.map {
            Map::Hashed(m) => m.remove(key),
            Map::Sorted(m) => m.remove(key),
        }
    }

    pub fn len(&self) -> usize {
        match &self.map {
            Map::Hashed(m) => m.len(),
            Map::Sorted(m) => m.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every `(key, entry)` pair: in key order for a sorted keydir, in no
    /// particular order for a hashed one.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &KeydirEntry)> {
        match &self.map {
            Map::Hashed(m) => Iter::Hashed(m.iter()),
            Map::Sorted(m) => Iter::Sorted(m.iter()),
        }
    }

    /// The `(key, entry)` pairs whose keys lie between `start` and `end`, in
    /// key order — or `None` for a hashed keydir, which can't answer a range
    /// without visiting every key. Bounds that describe no keys at all (start
    /// after end, or equal with either side excluded) give an empty range
    /// rather than a panic.
    pub fn range(
        &self,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Option<impl Iterator<Item = (&[u8], &KeydirEntry)>> {
        let Map::Sorted(m) = &self.map else {
            return None;
        };
        let empty = match (start, end) {
            (Bound::Included(s), Bound::Included(e)) => s > e,
            (Bound::Included(s) | Bound::Excluded(s), Bound::Excluded(e))
            | (Bound::Excluded(s), Bound::Included(e)) => s >= e,
            _ => false,
        };
        Some(if empty {
            Iter::Empty
        } else {
            Iter::Range(m.range::<[u8], _>((start, end)))
        })
    }

    /// Compare-and-repoint, used by merge (plan §7.3): only overwrite
    /// `key`'s entry if it is still exactly `expected_old` — i.e. nothing
    /// has written to `key` since merge observed that entry and copied its
    /// value forward into the new, compacted file. Returns whether the
    /// repoint happened; on `false`, merge's freshly-written copy is simply
    /// left unreferenced (harmless — cleaned up by the next merge pass).
    pub fn cas_repoint(&mut self, key: &[u8], expected_old: KeydirEntry, new: KeydirEntry) -> bool {
        match self.get_mut(key) {
            Some(slot) if *slot == expected_old => {
                *slot = new;
                true
            }
            _ => false,
        }
    }
}
