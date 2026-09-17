//! Merge (compaction): copy forward only the live entries from every
//! non-active data file into a fresh, smaller set of data files (each with
//! an accompanying hint file), then remove the old files. See
//! `docs/bitcask-implementation-plan.md` §7.
//!
//! ## File-ordering correctness (found by `tests/model.rs`'s proptest)
//!
//! Recovery (`crate::recovery`) rebuilds the keydir by scanning files in
//! ascending `file_id` order and letting later entries unconditionally
//! overwrite earlier ones — "last write wins" falls out of that order being
//! a correct proxy for chronological order. Merge output claims fresh ids
//! from the *same* counter active-file rotation uses, so on its own that's
//! fine. But if the active file at the moment merge starts happens to
//! *stay* active (never crosses the rotation threshold) through and after
//! the merge, it keeps its old, low id — while merge's output claims a
//! *higher* one. Any further write landing in that still-active file (even
//! well after merge returns, no concurrency required) then has a lower id
//! than data merge already considered "older", and a future recovery scans
//! them in the wrong order, silently resurrecting stale values.
//!
//! Fixed by having `merge` force the active file to rotate at the end if
//! its own output ever caught up to or passed it, so the active file is
//! always the numerically newest thing in the directory again once merge
//! returns — the same invariant `open()` establishes fresh every time
//! (plan §6.4). This fully resolves the sequential case above. It does
//! **not** fully resolve a key written *truly concurrently* with a merge
//! that also touches that key, followed by a reopen: such a write still
//! lands in the pre-merge active file before the forced rotation can run,
//! so it can still end up misordered on a *subsequent* recovery, even
//! though the CAS-repoint below keeps the live, in-memory keydir correct
//! for the remainder of that same session. Closing that gap in general
//! would need merge output to reuse ids freed by the files it replaces
//! (rather than claim new ones), which in turn needs a rename-based
//! finalization step designed carefully enough not to let a concurrent
//! read observe a renamed-but-not-yet-repointed file under stale keydir
//! coordinates — not implemented here; flagged as a known limitation
//! rather than worked around with an unproven fix.
//!
//! ## Batched flushing (found by profiling `loadtest` under `cargo flamegraph`)
//!
//! `ActiveFile::append` flushes on every call — required for `put`, since
//! any other thread could `get()` that key immediately after `put()`
//! returns. `MergeOutputWriter::write_live_entry` used to go through that
//! same `append`, meaning a merge pass paid one flush syscall *per live
//! entry it copied* — a real, measured cost (see `docs/` profiling notes)
//! and a big share of why merge took over a second on a modest dataset.
//!
//! Merge doesn't need that per-entry: it writes, then compare-and-swaps the
//! keydir, and nothing requires those two steps to be immediately adjacent
//! — only that the repoint never happens before the bytes it points at are
//! flushed. So `MergeOutputWriter` now writes through
//! `ActiveFile::append_buffered` (no flush) and flushes once per batch
//! ([`MERGE_FLUSH_BATCH_SIZE`] entries, or immediately at a rotation
//! boundary, which already fsyncs). `merge_with_hook` defers each entry's
//! keydir repoint into a `pending` list and only drains (applies) it once
//! `write_live_entry` reports a flush actually happened — so a key is still
//! never repointed before its bytes are visible to a fresh read, exactly
//! the same guarantee as before, just satisfied once per batch instead of
//! once per entry.
//!
//! This doesn't weaken crash safety: old input files are still only
//! removed after `out.finish()` has fully flushed and fsynced every output
//! file (§ above). Batching only changes how much of merge's own
//! in-progress work is discarded and redone by the next attempt if the
//! process crashes mid-merge — never the durability of data any `put`/
//! `delete` call already returned `Ok` for.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::commit::GroupCommit;
use crate::datafile::{ActiveFile, DataFileSet};
use crate::error::Result;
use crate::format::{self, Entry, EntryRead};
use crate::keydir::{KeydirEntry, SharedKeydir};

/// Compact every non-active data file in `dir`. Safe to call concurrently
/// with ongoing `put`/`get`/`delete` against the same keydir/active file —
/// a key written concurrently with the merge is never lost (see the
/// CAS-repoint discussion below).
///
/// `active` and `next_file_id` are the engine's own fields, passed through
/// rather than accessed via `&Engine`, since `Engine`'s fields are private
/// to `engine.rs` — this keeps merge's logic in its own module while still
/// sharing the *same* active-file lock and file-id counter the engine's
/// write path uses, which is what makes merge-output file ids never
/// collide with concurrently-rotated active files.
pub fn merge(
    dir: &Path,
    keydir: &SharedKeydir,
    files: &DataFileSet,
    active: &Mutex<ActiveFile>,
    group_commit: &GroupCommit,
    next_file_id: &AtomicU32,
    max_file_size: u64,
) -> Result<()> {
    merge_with_hook(
        dir,
        keydir,
        files,
        active,
        group_commit,
        next_file_id,
        max_file_size,
        |_| {},
    )
}

/// Same as [`merge`], plus a hook invoked for every live entry right after
/// it's been copied into the merge output but *before* the keydir
/// compare-and-swap repoint — the exact race window plan §7.3 describes.
/// `merge` itself is just this with a no-op hook; tests use this directly
/// (via `Engine`'s `#[cfg(test)]` forwarding method) to deterministically
/// pause merge mid-repoint and inject a concurrent write, rather than
/// relying on timing.
// One parameter over clippy's default threshold, all engine fields passed
// through individually (see the doc comment above) rather than bundled into
// a context struct — not worth the extra indirection for a `pub(crate)`,
// two-caller function.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_with_hook(
    dir: &Path,
    keydir: &SharedKeydir,
    files: &DataFileSet,
    active: &Mutex<ActiveFile>,
    group_commit: &GroupCommit,
    next_file_id: &AtomicU32,
    max_file_size: u64,
    mut before_repoint: impl FnMut(&[u8]),
) -> Result<()> {
    // 1. Snapshot which files are eligible: everything except the current
    //    active file at the moment merge starts. Files created by rotation
    //    *during* the merge (concurrent puts are still allowed) are simply
    //    not included in this pass — they'll be picked up by the next
    //    merge. Safe and simple (plan §7.2 step 1).
    let active_id_at_start = active.lock().unwrap().file_id();
    let input_ids: Vec<u32> = DataFileSet::discover(dir)?
        .into_iter()
        .filter(|&id| id < active_id_at_start)
        .collect();
    if input_ids.is_empty() {
        return Ok(()); // nothing to do
    }

    // 2. Iterate input files oldest -> newest, entry by entry, copying
    //    forward only the live ones.
    let mut out = MergeOutputWriter::new(dir, next_file_id, max_file_size)?;
    let mut highest_output_id = out.current_file_id();
    // Entries written but not yet repointed: write_live_entry batches its
    // flushes (see this module's doc comment), so a key's bytes may not be
    // durable-to-the-OS yet even though write_live_entry already returned.
    // Drained (repointed) whenever a flush actually happens, and once more
    // after out.finish() for whatever's left in the final partial batch —
    // never repointing a key before its bytes are flushed, same guarantee
    // as before, just satisfied once per batch instead of once per entry.
    let mut pending: Vec<(Vec<u8>, KeydirEntry, KeydirEntry)> = Vec::new();

    for &file_id in &input_ids {
        let data_path = DataFileSet::data_path(dir, file_id);
        for (offset, entry) in read_all_entries(&data_path)? {
            if entry.header.tombstone {
                continue; // dead by definition — never "live"
            }
            let entry_value_pos = offset + format::HEADER_SIZE as u64 + entry.header.ksz as u64;
            let is_live = keydir
                .get(&entry.key)
                .is_some_and(|kd| kd.file_id == file_id && kd.value_pos == entry_value_pos);
            if !is_live {
                continue;
            }

            let (new_file_id, new_value_pos, flushed) = out.write_live_entry(&entry)?;
            highest_output_id = highest_output_id.max(out.current_file_id());

            // 3. Repoint the keydir ONLY IF it still points at the exact
            //    (file_id, offset) we just copied — if a concurrent put()
            //    already overwrote this key while we were mid-merge, our
            //    copy is now stale and must NOT clobber the newer entry
            //    (plan §7.3). Queued here, applied once the batch covering
            //    it is actually flushed, below.
            let old = KeydirEntry {
                file_id,
                value_sz: entry.header.value_sz,
                value_pos: entry_value_pos,
                tstamp: entry.header.tstamp,
            };
            let new = KeydirEntry {
                file_id: new_file_id,
                value_sz: entry.header.value_sz,
                value_pos: new_value_pos,
                tstamp: entry.header.tstamp,
            };
            pending.push((entry.key, old, new));

            if flushed {
                apply_pending_repoints(keydir, &mut pending, &mut before_repoint);
            }
        }
    }
    out.finish()?; // flushes+fsyncs whatever's left in the final batch
    apply_pending_repoints(keydir, &mut pending, &mut before_repoint);

    // 4. Remove the old input files now that nothing in the keydir points
    //    at them anymore: every key that was live in them now points at
    //    `out`'s files (or was already repointed elsewhere by a race);
    //    every key that wasn't live was already pointing elsewhere and
    //    still does.
    for &file_id in &input_ids {
        let _ = fs::remove_file(DataFileSet::data_path(dir, file_id));
        let _ = fs::remove_file(DataFileSet::hint_path(dir, file_id));
        files.forget(file_id);
    }

    // 5. Restore "the active file is numerically newest" if merge's own
    //    output caught up to or passed it — see the module-level doc note
    //    above on why this matters for a future recovery's scan order. A
    //    no-op in the common case where the active file already rotated
    //    past highest_output_id on its own (e.g. from puts during merge).
    {
        let mut active_guard = active.lock().unwrap();
        if active_guard.file_id() <= highest_output_id {
            active_guard.sync()?;
            group_commit.mark_all_durable();
            let new_id = next_file_id.fetch_add(1, Ordering::SeqCst);
            *active_guard = ActiveFile::create(dir, new_id)?;
        }
    }

    Ok(())
}

/// Drain and apply every queued repoint: for each, run the race-window hook
/// then attempt the CAS. A `false` return from `cas_repoint` means a racing
/// put already won — correct: that entry's copy stays orphaned in the new
/// file, never pointed to, and gets cleaned up by the *next* merge pass.
fn apply_pending_repoints(
    keydir: &SharedKeydir,
    pending: &mut Vec<(Vec<u8>, KeydirEntry, KeydirEntry)>,
    before_repoint: &mut impl FnMut(&[u8]),
) {
    for (key, old, new) in pending.drain(..) {
        before_repoint(&key);
        keydir.cas_repoint(&key, old, new);
    }
}

/// Sequentially scan `path` (an already-closed, immutable data file — never
/// the active file) into `(offset, Entry)` pairs, tombstones included.
/// Unlike recovery's scan, every input file here is expected to be
/// complete, so a truncated entry is always a warning (there's no "this is
/// the last, still-active file" exemption the way there is during
/// recovery).
fn read_all_entries(path: &Path) -> io::Result<Vec<(u64, Entry)>> {
    let mut f = File::open(path)?;
    let mut out = Vec::new();
    let mut pos: u64 = 0;
    loop {
        match format::read_entry(&mut f)? {
            None => break,
            Some(EntryRead::Truncated) => {
                eprintln!(
                    "warning: {} has a truncated entry at offset {pos} during merge — \
                     unexpected for an already-closed file, possible corruption",
                    path.display()
                );
                break;
            }
            Some(EntryRead::Ok(entry, total_len)) => {
                out.push((pos, entry));
                pos += total_len;
            }
            Some(EntryRead::CrcMismatch { total_len }) => {
                eprintln!(
                    "warning: {} has a corrupt entry at offset {pos} (CRC mismatch) \
                     during merge, skipping it",
                    path.display()
                );
                pos += total_len;
            }
        }
    }
    Ok(out)
}

/// How many live entries `MergeOutputWriter` writes before flushing, absent
/// an earlier rotation boundary (which already flushes+fsyncs). See this
/// module's doc comment on batched flushing for why this is safe.
const MERGE_FLUSH_BATCH_SIZE: usize = 256;

/// Writes merge output: a data file plus its companion hint file, rotating
/// to `output_id, output_id+1, ...` on the same `max_file_size` threshold
/// normal active files use. Reuses `ActiveFile` for the data-file side;
/// doesn't touch the keydir itself — `merge` handles keydir updates,
/// batched to line up with this writer's own flush batches (plan §7.4 and
/// this module's doc comment on batched flushing).
struct MergeOutputWriter<'a> {
    dir: &'a Path,
    next_file_id: &'a AtomicU32,
    max_file_size: u64,
    current_data: ActiveFile,
    current_hint: BufWriter<File>,
    /// Live entries written since the last flush (via `write_unflushed`) —
    /// reset on every flush, whether from hitting `MERGE_FLUSH_BATCH_SIZE`
    /// or from a rotation boundary.
    pending_since_flush: usize,
}

impl<'a> MergeOutputWriter<'a> {
    fn new(dir: &'a Path, next_file_id: &'a AtomicU32, max_file_size: u64) -> io::Result<Self> {
        let (current_data, current_hint) = Self::open_output(dir, next_file_id)?;
        Ok(Self {
            dir,
            next_file_id,
            max_file_size,
            current_data,
            current_hint,
            pending_since_flush: 0,
        })
    }

    fn open_output(dir: &Path, next_file_id: &AtomicU32) -> io::Result<(ActiveFile, BufWriter<File>)> {
        // Claims an id from the *same* counter the engine's write path uses
        // for active-file rotation, so merge-output ids never collide with
        // a concurrently-rotated active file.
        let file_id = next_file_id.fetch_add(1, Ordering::SeqCst);
        let data = ActiveFile::create(dir, file_id)?;
        let hint = BufWriter::new(File::create(DataFileSet::hint_path(dir, file_id))?);
        Ok((data, hint))
    }

    fn current_file_id(&self) -> u32 {
        self.current_data.file_id()
    }

    /// Writes one live entry (unflushed) plus its hint record, and rotates
    /// or batch-flushes as needed. Returns `(file_id, value_pos, flushed)`
    /// — `flushed` tells the caller whether this entry's bytes (and every
    /// other still-pending entry's) are now safe to repoint in the keydir.
    fn write_live_entry(&mut self, entry: &Entry) -> io::Result<(u32, u64, bool)> {
        let encoded = format::encode_entry(&entry.key, &entry.value, false, entry.header.tstamp);
        let (file_id, value_pos, _total_len) = self.current_data.append_buffered(&encoded)?;
        let hint = format::encode_hint(&entry.key, &entry.header, value_pos);
        self.current_hint.write_all(hint.as_bytes())?;
        self.pending_since_flush += 1;

        let flushed = if self.current_data.len() >= self.max_file_size {
            self.rotate()?; // fsyncs — a strictly stronger guarantee than a flush
            true
        } else if self.pending_since_flush >= MERGE_FLUSH_BATCH_SIZE {
            self.flush_batch()?;
            true
        } else {
            false
        };

        Ok((file_id, value_pos, flushed))
    }

    /// A plain flush (not fsync) — same durability step `rotate`/`finish`
    /// do, just without also syncing to disk, since a batch boundary only
    /// needs to make bytes visible to a fresh read, not survive a crash any
    /// more than the rest of this design already promises without
    /// `sync_on_put`.
    fn flush_batch(&mut self) -> io::Result<()> {
        self.current_data.flush_only()?;
        self.pending_since_flush = 0;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.current_data.sync()?;
        self.current_hint.flush()?;
        let (data, hint) = Self::open_output(self.dir, self.next_file_id)?;
        self.current_data = data;
        self.current_hint = hint;
        self.pending_since_flush = 0;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.current_data.sync()?;
        self.current_hint.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bitcask, Engine, Options};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// Minimal self-cleaning temp directory — same pattern used throughout
    /// this crate's tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "yabiir-merge-test-{}-{}-{}",
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

    /// Raw scan of every data file currently in `dir` for how many entries
    /// (live or tombstone) exist anywhere for `key` — used to check that
    /// merge actually removed superseded/dead bytes from disk, not just
    /// that the keydir looks right.
    fn count_entries_for_key(dir: &Path, key: &[u8]) -> usize {
        let mut count = 0;
        for file_id in DataFileSet::discover(dir).unwrap() {
            let path = DataFileSet::data_path(dir, file_id);
            for (_, entry) in read_all_entries(&path).unwrap() {
                if entry.key == key {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn basic_compaction_keeps_only_latest_version() {
        let dir = TempDir::new();
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 1, // every put rotates into its own file
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"A", b"v1").unwrap();
        db.put(b"A", b"v2").unwrap();
        db.put(b"A", b"v3").unwrap();

        let before_ids = DataFileSet::discover(&dir).unwrap();
        assert!(
            before_ids.len() >= 4,
            "expected 3 rotated files + 1 active, got {before_ids:?}"
        );

        db.merge().unwrap();

        assert_eq!(db.get(b"A").unwrap(), Some(b"v3".to_vec()));
        assert_eq!(count_entries_for_key(&dir, b"A"), 1); // only the latest version survives anywhere on disk

        let after_ids = DataFileSet::discover(&dir).unwrap();
        for &id in &before_ids[..before_ids.len() - 1] {
            assert!(!after_ids.contains(&id), "old file {id} should have been removed");
        }
        // The highest id after merge is always the (possibly freshly
        // forced-rotated, per merge.rs's file-ordering correctness note)
        // active file — an ordinary ActiveFile, not a merge output, so it
        // has no hint file. Every *other* new id is a genuine merge output
        // and must have one.
        let mut new_ids: Vec<u32> = after_ids
            .iter()
            .copied()
            .filter(|id| !before_ids.contains(id))
            .collect();
        new_ids.sort_unstable();
        assert!(!new_ids.is_empty(), "expected at least one new merge-output file");
        let merge_output_ids = &new_ids[..new_ids.len() - 1];
        assert!(
            !merge_output_ids.is_empty(),
            "expected at least one new merge-output file besides the active file, got {new_ids:?}"
        );
        for &id in merge_output_ids {
            assert!(
                DataFileSet::hint_path(&dir, id).exists(),
                "merge output file {id} missing its hint file"
            );
        }
    }

    #[test]
    fn tombstone_reclamation() {
        let dir = TempDir::new();
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 1,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"B", b"v").unwrap();
        db.delete(b"B").unwrap();

        db.merge().unwrap();

        assert_eq!(db.get(b"B").unwrap(), None);
        assert_eq!(count_entries_for_key(&dir, b"B"), 0); // no trace, live or tombstone
    }

    #[test]
    fn merge_does_not_touch_the_active_file() {
        let dir = TempDir::new();
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 40,
                ..Options::default()
            },
        )
        .unwrap();
        for i in 0..30u32 {
            db.put(format!("k{i}").as_bytes(), format!("v{i}").as_bytes())
                .unwrap();
        }

        let before_ids = DataFileSet::discover(&dir).unwrap();
        assert!(
            before_ids.len() > 2,
            "test needs multiple rotated files, got {before_ids:?}"
        );
        let active_id = *before_ids.last().unwrap();
        let an_old_id = before_ids[0];

        db.merge().unwrap();

        let after_ids = DataFileSet::discover(&dir).unwrap();
        assert!(after_ids.contains(&active_id), "active file must survive merge untouched");
        assert!(!after_ids.contains(&an_old_id), "an old rotated file should have been merged away");

        for i in 0..30u32 {
            assert_eq!(
                db.get(format!("k{i}").as_bytes()).unwrap(),
                Some(format!("v{i}").into_bytes())
            );
        }
    }

    #[test]
    fn hint_files_match_full_scan_recovery_post_merge() {
        let dir = TempDir::new();
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 1,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"a", b"1").unwrap();
        db.put(b"a", b"2").unwrap();
        db.put(b"b", b"3").unwrap();
        db.delete(b"b").unwrap();
        db.put(b"c", b"4").unwrap();
        db.merge().unwrap();
        db.sync().unwrap();

        let mut expected_keys = db.list_keys().unwrap();
        expected_keys.sort();

        // A fresh, independent, *read-only* open (a second read_write
        // handle on the same directory is correctly refused by the new
        // single-writer lock — plan §8.1 — while `db` is still open) — the
        // keydir is rebuilt purely by recovery, hint-based here since merge
        // just wrote hint files.
        let reopened = Engine::open(
            &*dir,
            Options {
                read_write: false,
                ..Options::default()
            },
        )
        .unwrap();
        let mut got_keys = reopened.list_keys().unwrap();
        got_keys.sort();
        assert_eq!(got_keys, expected_keys);
        for key in &got_keys {
            assert_eq!(reopened.get(key).unwrap(), db.get(key).unwrap());
        }
    }

    #[test]
    fn repeated_merges_are_idempotent() {
        let dir = TempDir::new();
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 1,
                ..Options::default()
            },
        )
        .unwrap();
        db.put(b"a", b"1").unwrap();
        db.put(b"a", b"2").unwrap();
        db.put(b"b", b"3").unwrap();

        db.merge().unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"3".to_vec()));

        db.merge().unwrap(); // merging an already-merged, unchanged datastore again
        assert_eq!(db.get(b"a").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"3".to_vec()));
    }

    /// Plan §7.5's stress test: one thread merging in a loop while several
    /// others concurrently put/delete/get on a small, overlapping keyspace,
    /// with every put/delete mirrored (under one shared lock, so the
    /// comparison at the end is valid) into a plain `HashMap` reference
    /// model; the real engine's final state must match it exactly.
    #[test]
    fn stress_concurrent_put_delete_and_merge_match_reference_model() {
        let dir = TempDir::new();
        let db = Arc::new(
            Engine::open(
                &*dir,
                Options {
                    max_file_size: 256,
                    ..Options::default()
                },
            )
            .unwrap(),
        );
        let reference = Arc::new(Mutex::new(HashMap::<Vec<u8>, Vec<u8>>::new()));
        let keys: Vec<Vec<u8>> = (0..8u32).map(|i| format!("k{i}").into_bytes()).collect();
        let deadline = Instant::now() + Duration::from_millis(500);

        let merger = {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                while Instant::now() < deadline {
                    db.merge().unwrap();
                }
            })
        };

        let workers: Vec<_> = (0..4u64)
            .map(|worker_id| {
                let db = Arc::clone(&db);
                let reference = Arc::clone(&reference);
                let keys = keys.clone();
                std::thread::spawn(move || {
                    // Small xorshift-style PRNG — avoids pulling in a `rand`
                    // dependency just for test-input shuffling.
                    let mut state = 0x9E3779B97F4A7C15u64.wrapping_add(worker_id);
                    while Instant::now() < deadline {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        let key = &keys[(state as usize) % keys.len()];
                        let op = (state >> 32) % 3;
                        // Hold the reference lock across both the real
                        // mutation and the model update, so the two never
                        // drift apart and the final comparison is valid.
                        let mut reference = reference.lock().unwrap();
                        match op {
                            0 => {
                                let value = format!("v{state}").into_bytes();
                                db.put(key, &value).unwrap();
                                reference.insert(key.clone(), value);
                            }
                            1 => {
                                db.delete(key).unwrap();
                                reference.remove(key.as_slice());
                            }
                            _ => {
                                db.get(key).unwrap(); // extra concurrent read pressure
                            }
                        }
                    }
                })
            })
            .collect();

        for w in workers {
            w.join().unwrap();
        }
        merger.join().unwrap();

        let reference = reference.lock().unwrap();
        for key in &keys {
            assert_eq!(
                db.get(key).unwrap(),
                reference.get(key.as_slice()).cloned(),
                "mismatch for {:?}",
                String::from_utf8_lossy(key)
            );
        }
    }
}

