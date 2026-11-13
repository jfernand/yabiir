use super::*;
use crate::datafile::DataFileSet;

#[test]
fn reopen_recovers_previously_written_keys() {
    let dir = TempDir::new();
    {
        let db = open(&dir);
        db.put(b"k1", b"v1", now_unix())
            .unwrap();
        db.put(b"k2", b"v2", now_unix())
            .unwrap();
        db.delete(b"k1", now_unix())
            .unwrap();
        db.put(b"k3", b"v3", now_unix())
            .unwrap();
        db.sync()
            .unwrap();
    }
    let reopened = open(&dir);
    assert_eq!(
        reopened
            .get(b"k1")
            .unwrap(),
        None
    ); // deleted, stays deleted
    assert_eq!(
        reopened
            .get(b"k2")
            .unwrap(),
        Some(b"v2".to_vec())
    );
    assert_eq!(
        reopened
            .get(b"k3")
            .unwrap(),
        Some(b"v3".to_vec())
    );
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
            db.put(
                format!("k{i}").as_bytes(),
                format!("v{i}").as_bytes(),
                now_unix(),
            )
            .unwrap();
        }
        // dropped here without close() — simulates an unclean shutdown
    }
    let reopened = open(&dir);
    for i in 0..500u32 {
        assert_eq!(
            reopened
                .get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
}

/// A reopened store never appends to a file that already exists: its first
/// write starts a new file, with an id above every id on disk. The first
/// session leaves several files; after the reopen, old and new keys must
/// both read back, and the new write must be in a new file.
#[test]
fn reopen_after_rotation_starts_a_new_active_file_and_does_not_collide() {
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
        for i in 0..5u32 {
            db.put(
                format!("k{i}").as_bytes(),
                format!("v{i}").as_bytes(),
                now_unix(),
            )
            .unwrap();
        }
    }
    let before_ids = DataFileSet::discover(&dir).unwrap();
    assert!(
        before_ids.len() > 1,
        "test needs multiple rotated files, got {before_ids:?}"
    );
    let highest_before = *before_ids
        .iter()
        .max()
        .unwrap();

    let db = open(&dir);
    db.put(b"new-key", b"new-value", now_unix())
        .unwrap();

    // The new write's file must be newer than anything from the first
    // session, not an existing file appended to.
    let after_ids = DataFileSet::discover(&dir).unwrap();
    let highest_after = *after_ids
        .iter()
        .max()
        .unwrap();
    assert!(
        highest_after > highest_before,
        "expected a new file id above {highest_before}, got {highest_after}"
    );

    // Every old key must still be intact — a colliding reopen would
    // silently corrupt whichever old file it reused.
    for i in 0..5u32 {
        assert_eq!(
            db.get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(format!("v{i}").into_bytes())
        );
    }
    assert_eq!(
        db.get(b"new-key")
            .unwrap(),
        Some(b"new-value".to_vec())
    );
}

/// Opening a store and closing it again, without writing, leaves the
/// directory as it was: the active file is created by the first write.
#[test]
fn opening_without_writing_creates_no_data_files() {
    let dir = TempDir::new();
    for _ in 0..5 {
        let db = open(&dir);
        assert_eq!(
            db.get(b"k")
                .unwrap(),
            None
        );
        db.close()
            .unwrap();
    }
    assert!(
        DataFileSet::discover(&dir)
            .unwrap()
            .is_empty()
    );

    open(&dir)
        .put(b"k", b"v", now_unix())
        .unwrap();
    for _ in 0..5 {
        drop(open(&dir));
    }
    assert_eq!(
        DataFileSet::discover(&dir)
            .unwrap()
            .len(),
        1
    );
}

/// A read-only handle creates nothing, not even the directory.
#[test]
fn a_read_only_open_of_a_missing_directory_fails_and_creates_nothing() {
    let dir = TempDir::new();
    let missing = dir.join("no-such-store");
    assert!(matches!(
        Engine::open(&missing, Options::default().read_only()),
        Err(Error::Io(_))
    ));
    assert!(!missing.exists());
}

/// A merge that fails part-way leaves some of its output on disk. A write
/// after it must still be the last write after a reopen: the failed merge's
/// files sort below the file the write goes to.
#[test]
fn a_write_after_a_failed_merge_is_still_the_last_write_after_reopen() {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1, // every put gets its own file, and so does every merged entry
            ..Options::default()
        },
    )
    .unwrap();
    for key in [b"a", b"b", b"c"] {
        db.put(key, b"1", now_unix())
            .unwrap();
    }

    // Files 0, 1 and 2 hold the puts, so the merge writes 3, 4 and 5. A
    // directory where file 4's hint file should go makes it fail there,
    // after it has copied `a` into file 3.
    std::fs::create_dir(DataFileSet::hint_path(&dir, 4)).unwrap();
    db.merge()
        .unwrap_err();

    db.put(b"a", b"2", now_unix())
        .unwrap();
    let read_all = |db: &Engine| {
        [b"a", b"b", b"c"].map(|key| {
            db.get(key)
                .unwrap()
        })
    };
    let expected = [
        Some(b"2".to_vec()),
        Some(b"1".to_vec()),
        Some(b"1".to_vec()),
    ];
    assert_eq!(read_all(&db), expected);
    drop(db);
    assert_eq!(read_all(&open(&dir)), expected);
}
