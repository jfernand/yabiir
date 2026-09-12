//! The single-writer engine tying [`crate::keydir`] and [`crate::datafile`]
//! together: `put`/`get`/`delete`, implementing the [`Bitcask`] trait. See
//! `docs/bitcask-implementation-plan.md` §4. `open` recovers the keydir
//! from any existing data/hint files via [`crate::recovery`] (plan §6);
//! [`Engine::merge`] compacts non-active files via [`crate::merge`] (plan
//! §7); `open` also acquires the process-level single-writer lock via
//! [`crate::lock`] when `read_write` is set (plan §8.1).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{Bitcask, Options};
use crate::datafile::{ActiveFile, DataFileSet};
use crate::error::{Error, Result};
use crate::format;
use crate::keydir::{Keydir, KeydirEntry, SharedKeydir};
use crate::lock::DirLock;
use crate::merge;
use crate::recovery;

/// Concrete single-process Bitcask engine. See the module docs above for
/// what's implemented so far.
pub struct Engine {
    dir: PathBuf,
    keydir: SharedKeydir,
    files: DataFileSet,
    /// `None` for a `read_write: false` handle — a read-only handle never
    /// appends, so it never opens (or needs) a writable active file; every
    /// read goes through `files` instead (see `read_value`). Only ever
    /// locked for writing (`append`/rotation/merge's force-rotate), never
    /// read — `get`/`fold` don't touch this lock at all (see `read_value`),
    /// so a plain `Mutex` is all that's needed; there's no reader side left
    /// for `RwLock` to buy anything over `Mutex`.
    active: Option<Mutex<ActiveFile>>,
    next_file_id: AtomicU32,
    opts: Options,
    /// Held for the lifetime of a `read_write` handle; releases the OS
    /// `flock` automatically on drop. `None` for a read-only handle, which
    /// never takes this lock at all (plan §8.1).
    _write_lock: Option<DirLock>,
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
    ///
    /// Only ever called from `put`/`delete`/`merge`, all of which already
    /// call `require_write` first — `self.active` being `None` here would
    /// mean one of them didn't, so this treats that as the same
    /// [`Error::ReadOnly`] rather than panicking, defensively.
    fn append(&self, encoded: &format::EncodedEntry) -> Result<(u32, u64)> {
        let active_lock = self.active.as_ref().ok_or(Error::ReadOnly)?;
        let mut active = active_lock.lock().unwrap();
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
    /// path. Always goes through [`DataFileSet`], regardless of whether
    /// `entry.file_id` happens to be *this* handle's current active file or
    /// even *another* process's — `ActiveFile::append` always flushes
    /// before returning, so a keydir entry is never published until its
    /// bytes are already visible to a fresh, independent `File::open`.
    /// That's what lets this skip the active-file lock entirely: no read
    /// ever needs to touch it (plan §8.2's ideal of no lock at all during
    /// an active-file read).
    fn read_value(&self, entry: KeydirEntry) -> Result<Vec<u8>> {
        Ok(self
            .files
            .read_at(entry.file_id, entry.value_pos, entry.value_sz)?)
    }

    /// Test-only entry point into [`merge::merge_with_hook`], exposed here
    /// because it needs direct access to this struct's private fields —
    /// `merge.rs` can't reach them from outside this module. Lets tests
    /// deterministically pause merge right before it repoints a specific
    /// key's keydir entry, to exercise the plan §7.3 race without relying
    /// on timing.
    #[cfg(test)]
    fn merge_with_hook(&self, hook: impl FnMut(&[u8])) -> Result<()> {
        self.require_write()?;
        let active_lock = self.active.as_ref().ok_or(Error::ReadOnly)?;
        merge::merge_with_hook(
            &self.dir,
            &self.keydir,
            &self.files,
            active_lock,
            &self.next_file_id,
            self.opts.max_file_size,
            hook,
        )
    }
}

impl Bitcask for Engine {
    fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        // Acquired before anything else, matching plan §6.1's ordering —
        // fail fast if another read_write handle already holds it, before
        // doing any recovery work. Read-only opens never take this lock at
        // all, so they can coexist with each other and with the single
        // writer (plan §8.1).
        let write_lock = if opts.read_write {
            Some(DirLock::acquire(&dir)?)
        } else {
            None
        };

        let file_ids = DataFileSet::discover(&dir)?;
        let mut keydir = Keydir::new();
        recovery::recover(&dir, &file_ids, &mut keydir)?;

        // next_file_id starts above whatever's already on disk (plan
        // §3.4/§6.4) — closed files are never reopened for writing, even
        // if the process crashed while one was still active: it's scanned
        // as an ordinary (possibly torn) file by recovery above, and a
        // brand new file is started here instead of resuming it.
        let next_id = file_ids.last().map_or(0, |id| id + 1);
        // A read-only handle never appends, so it never opens a writable
        // active file at all (plan §6.1's `ActiveFile::none_for_read_only`)
        // — every read for it goes through `files` (see `read_value`).
        let active = if opts.read_write {
            Some(Mutex::new(ActiveFile::create(&dir, next_id)?))
        } else {
            None
        };

        Ok(Self {
            files: DataFileSet::new(&dir),
            dir,
            keydir: SharedKeydir::new(keydir),
            active,
            next_file_id: AtomicU32::new(next_id + 1),
            opts,
            _write_lock: write_lock,
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
        self.require_write()?;
        let active_lock = self.active.as_ref().ok_or(Error::ReadOnly)?;
        merge::merge(
            &self.dir,
            &self.keydir,
            &self.files,
            active_lock,
            &self.next_file_id,
            self.opts.max_file_size,
        )
    }

    fn sync(&self) -> Result<()> {
        if let Some(active) = &self.active {
            active.lock().unwrap().sync()?;
        }
        Ok(())
    }

    fn close(self) -> Result<()> {
        if let Some(active) = self.active {
            active.into_inner().unwrap().sync()?;
        }
        Ok(())
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
    fn reopen_recovers_previously_written_keys() {
        let dir = TempDir::new();
        {
            let db = open(&dir);
            db.put(b"k1", b"v1").unwrap();
            db.put(b"k2", b"v2").unwrap();
            db.delete(b"k1").unwrap();
            db.put(b"k3", b"v3").unwrap();
            db.sync().unwrap();
        }
        let reopened = open(&dir);
        assert_eq!(reopened.get(b"k1").unwrap(), None); // deleted, stays deleted
        assert_eq!(reopened.get(b"k2").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(reopened.get(b"k3").unwrap(), Some(b"v3".to_vec()));
    }

    /// Plan §6.5's "crash-and-reopen" case: write through the real engine,
    /// then drop the handle without calling `close()` (an unclean
    /// shutdown), and confirm reopening recovers everything that was
    /// written.
    #[test]
    fn crash_and_reopen_recovers_all_committed_writes() {
        let dir = TempDir::new();
        {
            let db = open(&dir);
            for i in 0..500u32 {
                db.put(format!("k{i}").as_bytes(), format!("v{i}").as_bytes())
                    .unwrap();
            }
            // dropped here without close() — simulates an unclean shutdown
        }
        let reopened = open(&dir);
        for i in 0..500u32 {
            assert_eq!(
                reopened.get(format!("k{i}").as_bytes()).unwrap(),
                Some(format!("v{i}").into_bytes())
            );
        }
    }

    /// Plan §7.5's "race: concurrent put during merge wins" — deterministic
    /// version, via `merge_with_hook` rather than timing. Merge is paused
    /// right before it would repoint "K"'s keydir entry to its merged
    /// location; a `put("K", "new")` happens on another thread while
    /// paused; merge is then allowed to finish. The CAS-repoint must lose
    /// to the concurrent write, not silently resurrect the value merge was
    /// copying forward (see `merge.rs`'s §7.3 discussion).
    #[test]
    fn race_concurrent_put_during_merge_does_not_lose_the_write() {
        let dir = TempDir::new();
        let db = std::sync::Arc::new(
            Engine::open(
                &*dir,
                Options {
                    max_file_size: 1, // every put rotates into its own file
                    ..Options::default()
                },
            )
            .unwrap(),
        );
        db.put(b"K", b"old").unwrap(); // lands in its own already-rotated-out file

        let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();

        let merger_db = std::sync::Arc::clone(&db);
        let merger = std::thread::spawn(move || {
            merger_db
                .merge_with_hook(move |key| {
                    if key == b"K" {
                        paused_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                    }
                })
                .unwrap();
        });

        paused_rx.recv().unwrap(); // merge has copied K forward, about to repoint
        db.put(b"K", b"new").unwrap(); // race: overwrite K while merge is paused
        resume_tx.send(()).unwrap(); // let merge's (now-stale) CAS attempt run

        merger.join().unwrap();

        assert_eq!(db.get(b"K").unwrap(), Some(b"new".to_vec()));
    }

    /// Plan §8.3: a second `read_write` handle on a directory that already
    /// has one open must fail cleanly (not panic, not hang).
    #[test]
    fn second_read_write_open_is_rejected_while_first_is_held() {
        let dir = TempDir::new();
        let _first = open(&dir);
        let second = Engine::open(&*dir, Options::default());
        assert!(matches!(second, Err(Error::AlreadyLocked)));
    }

    /// Plan §8.3: a read-only open must succeed concurrently with an
    /// existing read_write handle, since it never takes the write lock.
    #[test]
    fn read_only_open_succeeds_concurrently_with_a_read_write_open() {
        let dir = TempDir::new();
        let writer = open(&dir);
        writer.put(b"k", b"v").unwrap();

        let reader = Engine::open(
            &*dir,
            Options {
                read_write: false,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(reader.get(b"k").unwrap(), Some(b"v".to_vec()));

        // the writer is still usable too — the read-only open didn't
        // disturb it.
        writer.put(b"k2", b"v2").unwrap();
        assert_eq!(writer.get(b"k2").unwrap(), Some(b"v2".to_vec()));
    }

    /// Plan §8.3: dropping the read_write handle releases the lock, so a
    /// subsequent read_write open succeeds.
    #[test]
    fn dropping_the_write_handle_releases_the_lock_for_the_next_open() {
        let dir = TempDir::new();
        {
            let db = open(&dir);
            db.put(b"k", b"v").unwrap();
            // dropped at end of this block
        }
        let db = open(&dir); // must not fail with AlreadyLocked
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    /// `close()` also releases the lock (it's just an early, explicit
    /// version of the same drop), not only an implicit drop out of scope.
    #[test]
    fn close_releases_the_lock_for_the_next_open() {
        let dir = TempDir::new();
        let db = open(&dir);
        db.put(b"k", b"v").unwrap();
        db.close().unwrap();

        let db = open(&dir); // must not fail with AlreadyLocked
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    /// A read-only handle never touches the write lock at all, so two of
    /// them can coexist even with no writer present.
    #[test]
    fn multiple_read_only_opens_coexist() {
        let dir = TempDir::new();
        {
            let db = open(&dir);
            db.put(b"k", b"v").unwrap();
        }
        let read_only_opts = || Options {
            read_write: false,
            ..Options::default()
        };
        let r1 = Engine::open(&*dir, read_only_opts()).unwrap();
        let r2 = Engine::open(&*dir, read_only_opts()).unwrap();
        assert_eq!(r1.get(b"k").unwrap(), Some(b"v".to_vec()));
        assert_eq!(r2.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    /// Read-only handles must not be able to mutate the datastore even
    /// through `merge` (which was previously ungated — merge does real
    /// destructive writes and file removal, so it needs the write lock
    /// like everything else).
    #[test]
    fn merge_is_rejected_on_a_read_only_handle() {
        let dir = TempDir::new();
        {
            let db = open(&dir);
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
        assert!(matches!(ro.merge(), Err(Error::ReadOnly)));
    }
}
