use super::*;
use crate::testutil::{TempDir, entries_of};
use crate::{Bitcask, Engine, Options, now_unix};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Raw scan of every data file currently in `dir` for how many entries
/// (live or tombstone) exist anywhere for `key` — used to check that
/// merge actually removed superseded/dead bytes from disk, not just
/// that the keydir looks right.
fn count_entries_for_key(dir: &Path, key: &[u8]) -> usize {
    let mut count = 0;
    for file_id in DataFileSet::discover(dir).unwrap() {
        let path = DataFileSet::data_path(dir, file_id);
        for (_, entry) in entries_of(&path) {
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
    db.put(b"A", b"v1", now_unix())
        .unwrap();
    db.put(b"A", b"v2", now_unix())
        .unwrap();
    db.put(b"A", b"v3", now_unix())
        .unwrap();

    let before_ids = DataFileSet::discover(&dir).unwrap();
    assert_eq!(before_ids.len(), 3, "one file per put: {before_ids:?}");

    db.merge()
        .unwrap();

    assert_eq!(
        db.get(b"A")
            .unwrap(),
        Some(b"v3".to_vec())
    );
    assert_eq!(count_entries_for_key(&dir, b"A"), 1); // only the latest version survives anywhere on disk

    // Every file there was is gone, and what replaced it is merge output:
    // a data file with its hint file.
    let after_ids = DataFileSet::discover(&dir).unwrap();
    assert_eq!(after_ids.len(), 1, "one live entry, one output file");
    for id in &after_ids {
        assert!(!before_ids.contains(id), "old file {id} should be gone");
        assert!(
            DataFileSet::hint_path(&dir, *id).exists(),
            "merge output file {id} is missing its hint file"
        );
    }
    for id in &before_ids {
        assert!(!DataFileSet::hint_path(&dir, *id).exists());
    }
}

/// The output writer starts a new file when one reaches `max_file_size`,
/// like the write path does. With a tiny `max_file_size`, the output of a
/// merge must span several data files, each with its hint file.
#[test]
fn merge_output_rotates_when_it_exceeds_max_file_size() {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1,
            ..Options::default()
        },
    )
    .unwrap();
    for i in 0..10u32 {
        db.put(
            format!("k{i}").as_bytes(),
            format!("v{i}").as_bytes(),
            now_unix(),
        )
        .unwrap();
    }

    db.merge()
        .unwrap();

    let merge_output_ids: Vec<u32> = DataFileSet::discover(&dir)
        .unwrap()
        .into_iter()
        .filter(|&id| DataFileSet::hint_path(&dir, id).exists())
        .collect();
    assert!(
        merge_output_ids.len() > 1,
        "expected merge's own output to rotate into multiple files under \
         a tiny max_file_size, got {merge_output_ids:?}"
    );

    for i in 0..10u32 {
        assert_eq!(
            db.get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
}

/// The other merge tests use a tiny `max_file_size`, so the output writer
/// closes (and syncs) a file after nearly every entry. That leaves one job
/// untested: flushing the *last* batch, the entries written since the last
/// flush when no file filled up and the batch size wasn't reached. Here
/// many keys are written under a small `max_file_size`, so that they land
/// in many input files, and the store is reopened with the default one
/// before merging: the output is one file that never fills, and only the
/// final flush makes its entries readable.
#[test]
fn merge_flushes_a_partial_final_batch_that_never_rotated() {
    let dir = TempDir::new();
    {
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 1, // every put rotates into its own file
                ..Options::default()
            },
        )
        .unwrap();
        for i in 0..20u32 {
            db.put(
                format!("k{i}").as_bytes(),
                format!("v{i}").as_bytes(),
                now_unix(),
            )
            .unwrap();
        }
        // dropped: 20 input files, one per put.
    }

    let before_ids = DataFileSet::discover(&dir).unwrap();
    assert!(
        before_ids.len() >= 20,
        "expected many small rotated files, got {before_ids:?}"
    );

    // Reopen with the default, large max_file_size — merge's own
    // output writer now has no reason to rotate for 20 small entries,
    // so they all land in one batch that only `finish()` flushes.
    let db = Engine::open(&*dir, Options::default()).unwrap();
    db.merge()
        .unwrap();

    for i in 0..20u32 {
        assert_eq!(
            db.get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("v{i}").into_bytes()),
            "key k{i} missing or wrong after merge — finish()'s final-batch \
             flush likely didn't happen"
        );
    }

    // Exactly one output file (a data file with a hint file) confirms
    // that the output writer put everything in a single file.
    let merge_output_ids: Vec<u32> = DataFileSet::discover(&dir)
        .unwrap()
        .into_iter()
        .filter(|id| !before_ids.contains(id))
        .filter(|&id| DataFileSet::hint_path(&dir, id).exists())
        .collect();
    assert_eq!(
        merge_output_ids.len(),
        1,
        "expected exactly one un-rotated merge-output file, got {merge_output_ids:?}"
    );

    // Durability across a fresh reopen too, not just within the same
    // process (would also catch finish() flushing but never syncing).
    drop(db);
    let reopened = Engine::open(&*dir, Options::default()).unwrap();
    for i in 0..20u32 {
        assert_eq!(
            reopened
                .get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
}

/// A `Metrics` test double recording every `record_pending_queue_depth`
/// and `record_merge_summary` call it sees — proves merge actually
/// calls into a supplied `Metrics` for these two gauges specifically,
/// not just the duration-based ones the engine's tests cover.
#[derive(Default)]
struct GaugeRecordingMetrics {
    pending_queue_depths: Mutex<Vec<usize>>,
    merge_summaries: Mutex<Vec<(usize, usize)>>, // (input_files, live_entries_copied)
}

impl Metrics for GaugeRecordingMetrics {
    fn record_pending_queue_depth(&self, depth: usize) {
        self.pending_queue_depths
            .lock()
            .unwrap()
            .push(depth);
    }
    fn record_merge_summary(&self, input_files: usize, live_entries_copied: usize) {
        self.merge_summaries
            .lock()
            .unwrap()
            .push((input_files, live_entries_copied));
    }
}

#[test]
fn merge_reports_merge_summary_metrics() {
    let dir = TempDir::new();
    let metrics = Arc::new(GaugeRecordingMetrics::default());
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1, // every put rotates into its own file
            metrics: Some(metrics.clone() as Arc<dyn crate::Metrics>),
            ..Options::default()
        },
    )
    .unwrap();
    for i in 0..10u32 {
        db.put(
            format!("k{i}").as_bytes(),
            format!("v{i}").as_bytes(),
            now_unix(),
        )
        .unwrap();
    }

    db.merge()
        .unwrap();

    let summaries = metrics
        .merge_summaries
        .lock()
        .unwrap();
    assert_eq!(
        summaries.len(),
        1,
        "expected exactly one merge summary recorded"
    );
    let (input_files, live_entries_copied) = summaries[0];
    assert_eq!(
        input_files, 10,
        "expected merge to report all 10 rotated-out input files"
    );
    assert_eq!(
        live_entries_copied, 10,
        "expected merge to report all 10 live entries it copied forward"
    );
}

/// Same setup as `merge_flushes_a_partial_final_batch_that_never_rotated`
/// (many small input files, reopened under a large `max_file_size` so
/// merge's output writer never rotates) — specifically so all 20 live
/// entries land in *one* batch that only `finish()`'s final flush
/// covers, and `record_pending_queue_depth` gets one real, multi-entry
/// sample instead of a string of 1s (which `max_file_size: 1` would
/// produce, since every entry would fill its own file).
#[test]
fn merge_reports_pending_queue_depth_for_the_final_batch() {
    let dir = TempDir::new();
    {
        let db = Engine::open(
            &*dir,
            Options {
                max_file_size: 1,
                ..Options::default()
            },
        )
        .unwrap();
        for i in 0..20u32 {
            db.put(
                format!("k{i}").as_bytes(),
                format!("v{i}").as_bytes(),
                now_unix(),
            )
            .unwrap();
        }
    }

    let metrics = Arc::new(GaugeRecordingMetrics::default());
    let db = Engine::open(
        &*dir,
        Options {
            metrics: Some(metrics.clone() as Arc<dyn crate::Metrics>),
            ..Options::default()
        },
    )
    .unwrap();
    db.merge()
        .unwrap();

    let depths = metrics
        .pending_queue_depths
        .lock()
        .unwrap();
    assert_eq!(
        depths.as_slice(),
        &[20],
        "expected exactly one pending-queue-depth sample, for the whole \
         un-rotated final batch"
    );
}

/// A live entry that comes after a corrupt one in the same input file is
/// still copied. Merge decides an entry is live by comparing where it found
/// it with where the keydir says the key is, so the scan has to keep the
/// right offset across the corrupt entry. If it didn't, the live key would
/// be taken for dead, dropped, and left pointing into a file merge removes.
#[test]
fn merge_correctly_resumes_scanning_after_a_corrupt_entry_in_an_input_file() {
    let dir = TempDir::new();
    {
        let db = Engine::open(&*dir, Options::default()).unwrap();
        db.put(b"a", b"a-v1", now_unix())
            .unwrap();
        db.put(b"b", b"b-v1", now_unix())
            .unwrap();
        db.put(b"c", b"c-v1", now_unix())
            .unwrap();
        // all three land in one file (file 0), which becomes an
        // ordinary rotated-out (merge-eligible) file once reopened.
    }

    // Corrupt "b"'s value bytes in file 0 — a CRC mismatch, but leaves
    // "c" (which comes right after it in the same file) intact.
    let path = DataFileSet::data_path(&dir, 0);
    let mut bytes = fs::read(&path).unwrap();
    let corrupt_at = bytes
        .windows(4)
        .position(|w| w == b"b-v1")
        .unwrap();
    bytes[corrupt_at] ^= 0xFF;
    fs::write(&path, &bytes).unwrap();

    // Recovery's scan of file 0 (the same scanner, a different caller)
    // already drops "b" and keeps "c" here.
    let db = Engine::open(&*dir, Options::default()).unwrap();
    assert_eq!(
        db.get(b"a")
            .unwrap(),
        Some(b"a-v1".to_vec())
    );
    assert_eq!(
        db.get(b"b")
            .unwrap(),
        None
    );
    assert_eq!(
        db.get(b"c")
            .unwrap(),
        Some(b"c-v1".to_vec())
    );

    db.merge()
        .unwrap();

    assert_eq!(
        db.get(b"a")
            .unwrap(),
        Some(b"a-v1".to_vec()),
        "a (before the corrupt entry) must survive merge"
    );
    assert_eq!(
        db.get(b"b")
            .unwrap(),
        None
    );
    assert_eq!(
        db.get(b"c")
            .unwrap(),
        Some(b"c-v1".to_vec()),
        "c (after the corrupt entry, same input file) must survive merge"
    );
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
    db.put(b"B", b"v", now_unix())
        .unwrap();
    db.delete(b"B", now_unix())
        .unwrap();

    db.merge()
        .unwrap();

    assert_eq!(
        db.get(b"B")
            .unwrap(),
        None
    );
    assert_eq!(count_entries_for_key(&dir, b"B"), 0); // no trace, live or tombstone
}

/// A merge takes every data file there is, the one being appended to
/// included, and whatever is written afterwards goes to a file whose id is
/// above every output file's. Recovery replays files in id order, so this
/// is what keeps a later write the last write (see the module docs).
#[test]
fn merge_takes_every_file_and_later_writes_sort_after_its_output() {
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
        db.put(
            format!("k{i}").as_bytes(),
            format!("v{i}").as_bytes(),
            now_unix(),
        )
        .unwrap();
    }
    let before_ids = DataFileSet::discover(&dir).unwrap();
    assert!(
        before_ids.len() > 2,
        "test needs several files, got {before_ids:?}"
    );

    db.merge()
        .unwrap();

    let output_ids = DataFileSet::discover(&dir).unwrap();
    assert!(output_ids.len() > 1, "the output spans several files");
    for id in &before_ids {
        assert!(!output_ids.contains(id), "input file {id} should be gone");
    }

    db.put(b"later", b"write", now_unix())
        .unwrap();
    let newest_id = *DataFileSet::discover(&dir)
        .unwrap()
        .last()
        .unwrap();
    assert!(
        output_ids
            .iter()
            .all(|&id| id < newest_id),
        "a write after the merge must land above the output ({output_ids:?}), got {newest_id}"
    );

    for i in 0..30u32 {
        assert_eq!(
            db.get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
}

/// A merge with nothing to keep writes nothing: no empty output file is
/// left behind, and merging an empty store is a no-op.
#[test]
fn merge_of_only_dead_entries_leaves_no_files() {
    let dir = TempDir::new();
    let db = Engine::open(&*dir, Options::default()).unwrap();
    db.merge()
        .unwrap();
    assert!(
        DataFileSet::discover(&dir)
            .unwrap()
            .is_empty()
    );

    db.put(b"k", b"v", now_unix())
        .unwrap();
    db.delete(b"k", now_unix())
        .unwrap();
    db.merge()
        .unwrap();
    assert!(
        DataFileSet::discover(&dir)
            .unwrap()
            .is_empty(),
        "a value and its tombstone leave nothing to keep"
    );
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        None
    );
}

/// Merges are serialized: two at once would each take the other's
/// half-written output for input. Several threads merge while others
/// write, and every key must still read back, before and after a reopen.
#[test]
fn concurrent_merges_do_not_lose_data() {
    let dir = TempDir::new();
    let options = Options {
        max_file_size: 256,
        ..Options::default()
    };
    let db = Arc::new(Engine::open(&*dir, options.clone()).unwrap());
    std::thread::scope(|scope| {
        for writer in 0..2u32 {
            let db = &db;
            scope.spawn(move || {
                for i in 0..300u32 {
                    let key = format!("w{writer}-k{}", i % 40);
                    db.put(key.as_bytes(), format!("v{i}").as_bytes(), now_unix())
                        .unwrap();
                }
            });
        }
        for _ in 0..3 {
            let db = &db;
            scope.spawn(move || {
                for _ in 0..20 {
                    db.merge()
                        .unwrap();
                }
            });
        }
    });

    let check = |db: &Engine| {
        for writer in 0..2u32 {
            for k in 0..40u32 {
                let last_write = (0..300u32)
                    .rev()
                    .find(|i| i % 40 == k)
                    .unwrap();
                assert_eq!(
                    db.get(format!("w{writer}-k{k}").as_bytes())
                        .unwrap(),
                    Some(format!("v{last_write}").into_bytes())
                );
            }
        }
    };
    check(&db);
    drop(db);
    check(&Engine::open(&*dir, options).unwrap());
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
    db.put(b"a", b"1", now_unix())
        .unwrap();
    db.put(b"a", b"2", now_unix())
        .unwrap();
    db.put(b"b", b"3", now_unix())
        .unwrap();
    db.delete(b"b", now_unix())
        .unwrap();
    db.put(b"c", b"4", now_unix())
        .unwrap();
    db.merge()
        .unwrap();
    db.sync()
        .unwrap();

    let mut expected_keys = db
        .list_keys()
        .unwrap();
    expected_keys.sort();

    // A fresh, independent, *read-only* open (a second read_write
    // handle on the same directory is correctly refused by the new
    // single-writer lock — plan §8.1 — while `db` is still open) — the
    // keydir is rebuilt purely by recovery, hint-based here since merge
    // just wrote hint files.
    let reopened = Engine::open(
        &*dir,
        Options {
            is_read_write: false,
            ..Options::default()
        },
    )
    .unwrap();
    let mut got_keys = reopened
        .list_keys()
        .unwrap();
    got_keys.sort();
    assert_eq!(got_keys, expected_keys);
    for key in &got_keys {
        assert_eq!(
            reopened
                .get(key)
                .unwrap(),
            db.get(key)
                .unwrap()
        );
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
    db.put(b"a", b"1", now_unix())
        .unwrap();
    db.put(b"a", b"2", now_unix())
        .unwrap();
    db.put(b"b", b"3", now_unix())
        .unwrap();

    db.merge()
        .unwrap();
    assert_eq!(
        db.get(b"a")
            .unwrap(),
        Some(b"2".to_vec())
    );
    assert_eq!(
        db.get(b"b")
            .unwrap(),
        Some(b"3".to_vec())
    );

    db.merge()
        .unwrap(); // merging an already-merged, unchanged datastore again
    assert_eq!(
        db.get(b"a")
            .unwrap(),
        Some(b"2".to_vec())
    );
    assert_eq!(
        db.get(b"b")
            .unwrap(),
        Some(b"3".to_vec())
    );
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
    let keys: Vec<Vec<u8>> = (0..8u32)
        .map(|i| format!("k{i}").into_bytes())
        .collect();
    let deadline = Instant::now() + Duration::from_millis(500);

    let merger = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            while Instant::now() < deadline {
                db.merge()
                    .unwrap();
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
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1);
                    let key = &keys[(state as usize) % keys.len()];
                    let op = (state >> 32) % 3;
                    // Hold the reference lock across both the real
                    // mutation and the model update, so the two never
                    // drift apart and the final comparison is valid.
                    let mut reference = reference
                        .lock()
                        .unwrap();
                    match op {
                        0 => {
                            let value = format!("v{state}").into_bytes();
                            db.put(key, &value, now_unix())
                                .unwrap();
                            reference.insert(key.clone(), value);
                        }
                        1 => {
                            db.delete(key, now_unix())
                                .unwrap();
                            reference.remove(key.as_slice());
                        }
                        _ => {
                            db.get(key)
                                .unwrap(); // extra concurrent read pressure
                        }
                    }
                }
            })
        })
        .collect();

    for w in workers {
        w.join()
            .unwrap();
    }
    merger
        .join()
        .unwrap();

    let reference = reference
        .lock()
        .unwrap();
    for key in &keys {
        assert_eq!(
            db.get(key)
                .unwrap(),
            reference
                .get(key.as_slice())
                .cloned(),
            "mismatch for {:?}",
            String::from_utf8_lossy(key)
        );
    }
}

/// A data file can exist under the very id the writer would use next: a
/// file whose creation half-failed, or one that appeared between open and
/// the merge. The merge takes it as input, so it must not also write its
/// output there, or it would remove its own output with its input.
#[test]
fn merge_output_never_shares_an_id_with_an_input_file() {
    let dir = TempDir::new();
    let db = Engine::open(&*dir, Options::default()).unwrap();
    for i in 0..10u32 {
        db.put(format!("k{i}").as_bytes(), b"v", now_unix())
            .unwrap();
    }
    // The puts are in file 0; the writer's next file would be 1.
    fs::write(DataFileSet::data_path(&dir, 1), b"").unwrap();

    db.merge()
        .unwrap();

    let check = |db: &Engine| {
        for i in 0..10u32 {
            assert_eq!(
                db.get(format!("k{i}").as_bytes())
                    .unwrap(),
                Some(b"v".to_vec())
            );
        }
    };
    check(&db);
    drop(db);
    check(&Engine::open(&*dir, Options::default()).unwrap());
}
