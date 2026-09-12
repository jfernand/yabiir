//! The single-writer engine tying [`crate::keydir`] and [`crate::datafile`]
//! together: `put`/`get`/`delete`, implementing the [`Bitcask`] trait. See
//! `docs/bitcask-implementation-plan.md` §4.
//!
//! Not implemented yet, deliberately out of scope for this milestone:
//! - **Recovery** (plan §6): `open` does not scan existing data/hint files
//!   to rebuild the keydir. Reopening a directory that already has data in
//!   it starts with an *empty* keydir — previously-written keys become
//!   invisible to `get`/`list_keys`/`fold` until §6 lands, even though
//!   their bytes are still safely on disk (new writes simply continue
//!   appending after them; nothing is truncated or overwritten).
//! - **Merge** (plan §7): [`Engine::merge`] returns
//!   [`crate::error::Error::NotImplemented`].
//! - **Locking** (plan §8.1): nothing stops two `read_write` handles from
//!   being opened on the same directory concurrently yet.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{Bitcask, Options};
use crate::datafile::{ActiveFile, DataFileSet};
use crate::error::{Error, Result};
use crate::format;
use crate::keydir::{Keydir, KeydirEntry, SharedKeydir};

/// Concrete single-process Bitcask engine. See the module docs above for
/// what's implemented so far.
pub struct Engine {
    dir: PathBuf,
    keydir: SharedKeydir,
    files: DataFileSet,
    /// `RwLock`, not `Mutex`: `put`/`delete`/rotation take the write guard,
    /// but reading a value out of the still-active file (`read_value`) only
    /// needs a read guard, so concurrent `get`s of recently-written keys
    /// don't serialize against each other (only against a writer). This
    /// falls short of plan §8.2's ideal of *no* lock at all during the
    /// active-file read — that needs a read handle that survives rotation
    /// without going through this lock, which is a further refinement, not
    /// implemented here.
    active: RwLock<ActiveFile>,
    next_file_id: AtomicU32,
    opts: Options,
}

impl Engine {
    fn require_write(&self) -> Result<()> {
        if self.opts.read_write {
            Ok(())
        } else {
            Err(Error::ReadOnly)
        }
    }

    /// Append `encoded` to the active file, rotating to a fresh active file
    /// if the size threshold was crossed, per plan §4.2/§3.4. Returns the
    /// `(file_id, value_pos)` the keydir entry should point at.
    fn append(&self, encoded: &format::EncodedEntry) -> Result<(u32, u64)> {
        let mut active = self.active.write().unwrap();
        let (file_id, value_pos, _total_len) = active.append(encoded)?;
        if self.opts.sync_on_put {
            active.sync()?;
        }
        if active.len() >= self.opts.max_file_size {
            active.sync()?;
            let new_id = self.next_file_id.fetch_add(1, Ordering::SeqCst);
            *active = ActiveFile::create(&self.dir, new_id)?;
        }
        Ok((file_id, value_pos))
    }

    /// Read the value bytes a keydir entry points at: value-only read (plan
    /// §4.3 strategy (a)) — exactly `value_sz` bytes starting at
    /// `value_pos`, no header re-read, no CRC re-verification on the read
    /// path. Routes to the active file or to [`DataFileSet`] depending on
    /// whether `entry.file_id` is still the active file, decided within a
    /// single lock acquisition so a concurrent rotation can't cause this to
    /// read the wrong file (see the `active` field's doc comment above).
    fn read_value(&self, entry: KeydirEntry) -> Result<Vec<u8>> {
        let active = self.active.read().unwrap();
        if entry.file_id == active.file_id() {
            return Ok(active.read_at(entry.value_pos, entry.value_sz)?);
        }
        drop(active);
        Ok(self
            .files
            .read_at(entry.file_id, entry.value_pos, entry.value_sz)?)
    }
}

impl Bitcask for Engine {
    fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        // No recovery yet (see module docs) — the keydir starts empty
        // regardless of what's already in the directory. We still compute
        // the correct starting file_id from whatever's on disk (plan
        // §3.4's "next_file_id ... initialized at open() time to
        // max(existing file_ids) + 1"), so a reopened directory's new
        // writes land in a fresh file after any existing ones rather than
        // silently resuming file_id 0 every time.
        let existing = DataFileSet::discover(&dir)?;
        let next_id = existing.last().map_or(0, |id| id + 1);
        let active = ActiveFile::create(&dir, next_id)?;

        Ok(Self {
            files: DataFileSet::new(&dir),
            dir,
            keydir: SharedKeydir::new(Keydir::new()),
            active: RwLock::new(active),
            next_file_id: AtomicU32::new(next_id + 1),
            opts,
        })
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.keydir.get(key) {
            Some(entry) => Ok(Some(self.read_value(entry)?)),
            None => Ok(None),
        }
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        if key.is_empty() {
            return Err(Error::EmptyKey);
        }
        self.require_write()?;

        let tstamp = now_unix();
        let encoded = format::encode_entry(key, value, false, tstamp);
        let (file_id, value_pos) = self.append(&encoded)?;
        self.keydir.insert(
            key,
            KeydirEntry {
                file_id,
                value_sz: value.len() as u32,
                value_pos,
                tstamp,
            },
        );
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<()> {
        if key.is_empty() {
            return Err(Error::EmptyKey);
        }
        self.require_write()?;

        if self.keydir.get(key).is_none() {
            return Ok(()); // deleting a non-existent key is a no-op
        }
        let tstamp = now_unix();
        let encoded = format::encode_entry(key, &[], true, tstamp);
        self.append(&encoded)?;
        self.keydir.remove(key);
        Ok(())
    }

    fn list_keys(&self) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .keydir
            .snapshot()
            .into_iter()
            .map(|(key, _)| Vec::from(key))
            .collect())
    }

    fn fold<A>(&self, mut f: impl FnMut(&[u8], &[u8], A) -> A, init: A) -> Result<A> {
        let mut acc = init;
        for (key, entry) in self.keydir.snapshot() {
            let value = self.read_value(entry)?;
            acc = f(&key, &value, acc);
        }
        Ok(acc)
    }

    fn merge(&self) -> Result<()> {
        Err(Error::NotImplemented("merge (plan §7)"))
    }

    fn sync(&self) -> Result<()> {
        Ok(self.active.write().unwrap().sync()?)
    }

    fn close(self) -> Result<()> {
        let mut active = self.active.into_inner().unwrap();
        Ok(active.sync()?)
    }
}

fn now_unix() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    /// Minimal self-cleaning temp directory — same pattern as
    /// `src/datafile.rs`'s tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "yabiir-engine-test-{}-{}-{}",
                std::process::id(),
                n,
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open(dir: &Path) -> Engine {
        Engine::open(dir, Options::default()).unwrap()
    }

    #[test]
    fn put_then_get_returns_value() {
        let dir = TempDir::new();
        let db = open(&dir);
        db.put(b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn put_twice_returns_second_value_only() {
        let dir = TempDir::new();
        let db = open(&dir);
        db.put(b"k", b"v1").unwrap();
        db.put(b"k", b"v2").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn delete_then_get_returns_none() {
        let dir = TempDir::new();
        let db = open(&dir);
        db.put(b"k", b"v").unwrap();
        db.delete(b"k").unwrap();
        assert_eq!(db.get(b"k").unwrap(), None);
    }

    #[test]
    fn delete_missing_key_is_a_silent_no_op() {
        let dir = TempDir::new();
        let db = open(&dir);
        db.delete(b"missing").unwrap(); // must not error
        assert_eq!(db.get(b"missing").unwrap(), None);
    }

    #[test]
    fn get_missing_key_returns_none() {
        let dir = TempDir::new();
        let db = open(&dir);
        assert_eq!(db.get(b"missing").unwrap(), None);
    }

    #[test]
    fn various_value_sizes_round_trip() {
        let dir = TempDir::new();
        let db = open(&dir);
        for (key, len) in [(b"empty".as_slice(), 0), (b"one", 1), (b"big", 3 * 1024 * 1024)] {
            let value = vec![0x5Au8; len];
            db.put(key, &value).unwrap();
            assert_eq!(db.get(key).unwrap(), Some(value));
        }
    }

    #[test]
    fn many_distinct_keys_round_trip() {
        let dir = TempDir::new();
        let db = open(&dir);
        for i in 0..10_000u32 {
            db.put(format!("key-{i}").as_bytes(), format!("value-{i}").as_bytes())
                .unwrap();
        }
        for i in 0..10_000u32 {
            assert_eq!(
                db.get(format!("key-{i}").as_bytes()).unwrap(),
                Some(format!("value-{i}").into_bytes())
            );
        }
    }

    #[test]
    fn put_rejects_empty_key() {
        let dir = TempDir::new();
        let db = open(&dir);
        assert!(matches!(db.put(b"", b"v"), Err(Error::EmptyKey)));
    }

    #[test]
    fn delete_rejects_empty_key() {
        let dir = TempDir::new();
        let db = open(&dir);
        assert!(matches!(db.delete(b""), Err(Error::EmptyKey)));
    }

    #[test]
    fn put_and_delete_are_rejected_on_a_read_only_handle() {
        let dir = TempDir::new();
        {
            let db = open(&dir); // create the directory as a writer first
            db.put(b"k", b"v").unwrap();
        }
        let ro = Engine::open(
            &*dir,
            Options {
                read_write: false,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(matches!(ro.put(b"k2", b"v2"), Err(Error::ReadOnly)));
        assert!(matches!(ro.delete(b"k"), Err(Error::ReadOnly)));
    }

    #[test]
    fn list_keys_and_fold_reflect_live_keys_only() {
        let dir = TempDir::new();
        let db = open(&dir);
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();
        db.delete(b"b").unwrap();

        let mut keys = db.list_keys().unwrap();
        keys.sort();
        assert_eq!(keys, vec![b"a".to_vec(), b"c".to_vec()]);

        let total: usize = db
            .fold(|_k, v, acc| acc + v.len(), 0usize)
            .unwrap();
        assert_eq!(total, 2); // "1".len() + "3".len()
    }

    #[test]
    fn rotation_keeps_every_key_readable_across_both_active_and_rotated_files() {
        let dir = TempDir::new();
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 300,
                ..Options::default()
            },
        )
        .unwrap();

        for i in 0..200u32 {
            db.put(format!("k{i}").as_bytes(), format!("value-{i}").as_bytes())
                .unwrap();
        }

        // Must have actually rotated for this test to be exercising what it
        // claims to.
        let file_count = DataFileSet::discover(&dir).unwrap().len();
        assert!(file_count > 1, "expected multiple data files, got {file_count}");

        for i in 0..200u32 {
            assert_eq!(
                db.get(format!("k{i}").as_bytes()).unwrap(),
                Some(format!("value-{i}").into_bytes())
            );
        }
    }

    #[test]
    fn merge_is_not_implemented_yet() {
        let dir = TempDir::new();
        let db = open(&dir);
        assert!(matches!(db.merge(), Err(Error::NotImplemented(_))));
    }

    /// Documents today's known gap (see the module docs): reopening a
    /// directory with existing data does NOT yet recover those keys into
    /// the new keydir. This test should start failing once plan §6
    /// (recovery) is implemented — at that point, delete it (or flip it
    /// into a real recovery test) rather than leaving it stale.
    #[test]
    fn reopen_does_not_yet_recover_previously_written_keys() {
        let dir = TempDir::new();
        {
            let db = open(&dir);
            db.put(b"k", b"v").unwrap();
            db.sync().unwrap();
        }
        let reopened = open(&dir);
        assert_eq!(reopened.get(b"k").unwrap(), None);
    }
}
