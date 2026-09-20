//! In-memory index mapping each live key to the location of its newest
//! value on disk (the "keydir"). See `docs/bitcask-implementation-plan.md`
//! §2.

use std::collections::HashMap;
use std::sync::RwLock;

use ahash::RandomState as AHashState;

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
/// the concurrent wrapper used at the engine level.
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
#[derive(Default)]
pub struct Keydir {
    map: HashMap<Box<[u8]>, KeydirEntry, AHashState>,
}

impl Keydir {
    pub fn new() -> Self {
        Self {
            map: HashMap::default(),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<KeydirEntry> {
        self.map
            .get(key)
            .copied()
    }

    /// Insert or overwrite `key`'s entry unconditionally — last write wins.
    pub fn insert(&mut self, key: &[u8], entry: KeydirEntry) {
        match self
            .map
            .get_mut(key)
        {
            Some(slot) => *slot = entry,
            None => {
                self.map
                    .insert(Box::from(key), entry);
            }
        }
    }

    pub fn remove(&mut self, key: &[u8]) -> Option<KeydirEntry> {
        self.map
            .remove(key)
    }

    pub fn len(&self) -> usize {
        self.map
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.map
            .is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &KeydirEntry)> {
        self.map
            .iter()
            .map(|(k, v)| (k.as_ref(), v))
    }

    /// Compare-and-repoint, used by merge (plan §7.3): only overwrite
    /// `key`'s entry if it is still exactly `expected_old` — i.e. nothing
    /// has written to `key` since merge observed that entry and copied its
    /// value forward into the new, compacted file. Returns whether the
    /// repoint happened; on `false`, merge's freshly-written copy is simply
    /// left unreferenced (harmless — cleaned up by the next merge pass).
    pub fn cas_repoint(&mut self, key: &[u8], expected_old: KeydirEntry, new: KeydirEntry) -> bool {
        match self
            .map
            .get_mut(key)
        {
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
