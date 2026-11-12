//! In-memory index mapping each live key to the location of its newest
//! value on disk (the "keydir"). See `docs/bitcask-implementation-plan.md`
//! §2.

use std::collections::{BTreeMap, HashMap, btree_map, hash_map};
use std::ops::Bound;
use std::sync::RwLock;

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

/// The keydir itself. Not thread-safe on its own — see [`SharedKeydir`] for
/// the concurrent wrapper used at the engine level. Keeps its keys either in
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

/// Thread-safe wrapper around [`Keydir`], used at the engine level: readers
/// (`get`, `snapshot`) take a read lock, writers (`insert`, `remove`,
/// `cas_repoint`) take a write lock. A single `RwLock<HashMap<..>>` is the
/// right starting point — no sharding or lock-free map until a benchmark
/// actually shows contention (plan §2.3).
#[derive(Default)]
pub struct SharedKeydir(RwLock<Keydir>);

impl SharedKeydir {
    pub fn new(keydir: Keydir) -> Self {
        Self(RwLock::new(keydir))
    }

    pub fn get(&self, key: &[u8]) -> Option<KeydirEntry> {
        self.0
            .read()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .get(key)
    }

    pub fn insert(&self, key: &[u8], entry: KeydirEntry) {
        self.0
            .write()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .insert(key, entry);
    }

    pub fn remove(&self, key: &[u8]) -> Option<KeydirEntry> {
        self.0
            .write()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .remove(key)
    }

    pub fn len(&self) -> usize {
        self.0
            .read()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.0
            .read()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .is_empty()
    }

    pub fn cas_repoint(&self, key: &[u8], expected_old: KeydirEntry, new: KeydirEntry) -> bool {
        self.0
            .write()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .cas_repoint(key, expected_old, new)
    }

    /// Run `f` with the keydir under its read lock, and return what it
    /// returns. For a reader that has to act on an entry while the entry is
    /// still current — `Engine::get` and `Engine::fold` take the read handle
    /// for an entry's file here, so merge can't remove the file first (see
    /// `DataFileSet::handle`). `f` must not do disk I/O: the lock blocks
    /// every writer while it runs (plan §8.2).
    pub fn with_read<R>(&self, f: impl FnOnce(&Keydir) -> R) -> R {
        f(&self
            .0
            .read()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting"))
    }

    /// Point-in-time snapshot of every `(key, entry)` pair, for `fold` and
    /// `list_keys`. The read lock is held only long enough to clone the
    /// map's contents out, never across the disk I/O those callers do
    /// afterward — see the "never hold a lock across disk I/O" rule in plan
    /// §8.2.
    pub fn snapshot(&self) -> Vec<(Box<[u8]>, KeydirEntry)> {
        self.0
            .read()
            .expect("Key directory data structure no longer safe (RW lock poisoned); exiting")
            .iter()
            .map(|(k, v)| (Box::<[u8]>::from(k), *v))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn entry(n: u32) -> KeydirEntry {
        KeydirEntry {
            file_id: n,
            value_size: n,
            value_pos: n as u64,
            timestamp: n,
        }
    }

    fn sorted_with(keys: &[&str]) -> Keydir {
        let mut kd = Keydir::with_order(KeyOrder::Sorted);
        for (i, k) in keys
            .iter()
            .enumerate()
        {
            kd.insert(k.as_bytes(), entry(i as u32));
        }
        kd
    }

    fn keys_of<'a>(it: impl Iterator<Item = (&'a [u8], &'a KeydirEntry)>) -> Vec<&'a [u8]> {
        it.map(|(k, _)| k)
            .collect()
    }

    #[test]
    fn both_orders_agree_on_every_point_operation() {
        for order in [KeyOrder::Hashed, KeyOrder::Sorted] {
            let mut kd = Keydir::with_order(order);
            assert_eq!(kd.order(), order);
            kd.insert(b"a", entry(1));
            kd.insert(b"b", entry(2));
            kd.insert(b"a", entry(3)); // overwrite
            assert_eq!(kd.get(b"a"), Some(entry(3)), "{order:?}");
            assert_eq!(kd.len(), 2, "{order:?}");
            assert!(!kd.cas_repoint(b"a", entry(1), entry(9)), "{order:?}");
            assert!(kd.cas_repoint(b"a", entry(3), entry(9)), "{order:?}");
            assert_eq!(kd.get(b"a"), Some(entry(9)), "{order:?}");
            assert_eq!(kd.remove(b"b"), Some(entry(2)), "{order:?}");
            assert_eq!(kd.get(b"b"), None, "{order:?}");
            assert_eq!(kd.len(), 1, "{order:?}");
        }
    }

    #[test]
    fn a_sorted_keydir_iterates_in_key_order() {
        let kd = sorted_with(&["c64", "apple-ii", "zx-spectrum", "bbc-micro", "ibm-pc"]);
        assert_eq!(
            keys_of(kd.iter()),
            vec![
                &b"apple-ii"[..],
                b"bbc-micro",
                b"c64",
                b"ibm-pc",
                b"zx-spectrum"
            ]
        );
    }

    #[test]
    fn range_honours_each_kind_of_bound_in_key_order() {
        let kd = sorted_with(&["a", "b", "c", "d", "e"]);
        let r = |s: Bound<&[u8]>, e: Bound<&[u8]>| {
            keys_of(
                kd.range(s, e)
                    .unwrap(),
            )
        };
        use Bound::{Excluded, Included, Unbounded};
        assert_eq!(
            r(Included(b"b"), Included(b"d")),
            vec![&b"b"[..], b"c", b"d"]
        );
        assert_eq!(r(Included(b"b"), Excluded(b"d")), vec![&b"b"[..], b"c"]);
        assert_eq!(r(Excluded(b"b"), Included(b"d")), vec![&b"c"[..], b"d"]);
        assert_eq!(r(Unbounded, Excluded(b"c")), vec![&b"a"[..], b"b"]);
        assert_eq!(r(Included(b"d"), Unbounded), vec![&b"d"[..], b"e"]);
        // Bounds between stored keys still select the keys between them.
        assert_eq!(r(Included(b"bb"), Included(b"dd")), vec![&b"c"[..], b"d"]);
    }

    #[test]
    fn range_with_bounds_that_select_nothing_is_empty_rather_than_a_panic() {
        // BTreeMap::range itself panics on these.
        let kd = sorted_with(&["a", "b", "c"]);
        use Bound::{Excluded, Included};
        for (s, e) in [
            (Included(&b"c"[..]), Included(&b"a"[..])),
            (Excluded(&b"b"[..]), Excluded(&b"b"[..])),
            (Included(&b"b"[..]), Excluded(&b"b"[..])),
            (Excluded(&b"b"[..]), Included(&b"b"[..])),
        ] {
            assert_eq!(
                kd.range(s, e)
                    .unwrap()
                    .count(),
                0
            );
        }
        assert_eq!(
            kd.range(Included(b"b"), Included(b"b"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn a_hashed_keydir_has_no_range() {
        let mut kd = Keydir::new();
        kd.insert(b"a", entry(1));
        assert!(
            kd.range(Bound::Unbounded, Bound::Unbounded)
                .is_none()
        );
    }

    #[test]
    fn insert_then_get_returns_same_entry() {
        let mut kd = Keydir::new();
        kd.insert(b"k", entry(1));
        assert_eq!(kd.get(b"k"), Some(entry(1)));
    }

    #[test]
    fn insert_twice_is_last_write_wins() {
        let mut kd = Keydir::new();
        kd.insert(b"k", entry(1));
        kd.insert(b"k", entry(2));
        assert_eq!(kd.get(b"k"), Some(entry(2)));
        assert_eq!(kd.len(), 1);
    }

    #[test]
    fn remove_then_get_returns_none() {
        let mut kd = Keydir::new();
        kd.insert(b"k", entry(1));
        assert_eq!(kd.remove(b"k"), Some(entry(1)));
        assert_eq!(kd.get(b"k"), None);
        assert!(kd.is_empty());
    }

    #[test]
    fn remove_missing_key_returns_none() {
        let mut kd = Keydir::new();
        assert_eq!(kd.remove(b"missing"), None);
    }

    #[test]
    fn cas_repoint_succeeds_when_expectation_matches() {
        let mut kd = Keydir::new();
        kd.insert(b"k", entry(1));
        assert!(kd.cas_repoint(b"k", entry(1), entry(2)));
        assert_eq!(kd.get(b"k"), Some(entry(2)));
    }

    #[test]
    fn cas_repoint_fails_when_expectation_is_stale() {
        let mut kd = Keydir::new();
        kd.insert(b"k", entry(1));
        kd.insert(b"k", entry(2)); // a "concurrent write" raced ahead
        assert!(!kd.cas_repoint(b"k", entry(1), entry(3)));
        // the stale CAS must not have mutated anything
        assert_eq!(kd.get(b"k"), Some(entry(2)));
    }

    #[test]
    fn cas_repoint_fails_when_key_is_absent() {
        let mut kd = Keydir::new();
        assert!(!kd.cas_repoint(b"missing", entry(1), entry(2)));
        assert_eq!(kd.get(b"missing"), None);
    }

    #[test]
    fn iter_yields_all_entries() {
        let mut kd = Keydir::new();
        kd.insert(b"a", entry(1));
        kd.insert(b"b", entry(2));
        let mut seen: Vec<_> = kd
            .iter()
            .map(|(k, v)| (k.to_vec(), *v))
            .collect();
        seen.sort_by(|a, b| {
            a.0.cmp(&b.0)
        });
        assert_eq!(
            seen,
            vec![(b"a".to_vec(), entry(1)), (b"b".to_vec(), entry(2))]
        );
    }

    #[test]
    fn snapshot_matches_iter() {
        let mut kd = Keydir::new();
        kd.insert(b"a", entry(1));
        kd.insert(b"b", entry(2));
        let shared = SharedKeydir::new(kd);

        let mut snap = shared.snapshot();
        snap.sort_by(|a, b| {
            a.0.cmp(&b.0)
        });
        assert_eq!(
            snap,
            vec![
                (Box::<[u8]>::from(b"a".as_slice()), entry(1)),
                (Box::<[u8]>::from(b"b".as_slice()), entry(2)),
            ]
        );
    }

    /// N reader threads calling `get` in a loop while 1 writer thread
    /// inserts/removes the same key for a fixed duration. Every value a
    /// reader observes must be exactly the sentinel entry the writer always
    /// uses — never a torn/partial/garbage struct — and nothing should
    /// panic or deadlock.
    #[test]
    fn concurrent_reads_and_writes_stay_consistent() {
        let keydir = Arc::new(SharedKeydir::new(Keydir::new()));
        let key: &'static [u8] = b"shared-key";
        let sentinel = entry(42);
        let deadline = Instant::now() + Duration::from_millis(200);

        let writer = {
            let keydir = Arc::clone(&keydir);
            thread::spawn(move || {
                while Instant::now() < deadline {
                    keydir.insert(key, sentinel);
                    keydir.remove(key);
                }
            })
        };

        let readers: Vec<_> = (0..4)
            .map(|_| {
                let keydir = Arc::clone(&keydir);
                thread::spawn(move || {
                    while Instant::now() < deadline {
                        if let Some(observed) = keydir.get(key) {
                            assert_eq!(observed, sentinel);
                        }
                    }
                })
            })
            .collect();

        writer
            .join()
            .unwrap();
        for reader in readers {
            reader
                .join()
                .unwrap();
        }
    }

    #[test]
    fn shared_keydir_len_and_is_empty_reflect_contents() {
        let shared = SharedKeydir::new(Keydir::new());
        assert_eq!(shared.len(), 0);
        assert!(shared.is_empty());

        shared.insert(b"k", entry(1));
        assert_eq!(shared.len(), 1);
        assert!(!shared.is_empty());

        shared.remove(b"k");
        assert_eq!(shared.len(), 0);
        assert!(shared.is_empty());
    }
}
