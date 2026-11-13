use super::*;
use crate::datafile::DataFileSet;
use std::sync::mpsc::{Receiver, Sender};

/// A store whose every `put` rotates into its own file, holding `K` and
/// one other key, so `merge` has immutable input files to compact and
/// remove.
fn store_for_merge_race(dir: &Path) -> Engine {
    let db = Engine::open(
        dir,
        Options {
            max_file_size: 1, // every put rotates into its own file
            ..Options::default()
        },
    )
    .unwrap();
    db.put(b"K", b"v", now_unix())
        .unwrap();
    db.put(b"other", b"x", now_unix())
        .unwrap();
    db
}

/// `get` looks `K` up, then — before it reads the value — a whole `merge`
/// runs, repointing `K` and removing the file the lookup returned. This
/// is the interleaving behind the intermittent failures of
/// `merge::tests::stress_concurrent_put_delete_and_merge_match_reference_model`,
/// made deterministic.
#[test]
fn race_get_survives_merge_removing_its_file_between_lookup_and_read() {
    let dir = TempDir::new();
    let db = store_for_merge_race(&dir);

    let value = db.get_with_hook(b"K", |entry| {
        db.merge()
            .unwrap();
        // The precondition of the race, checked so the test can't pass
        // vacuously: the file the lookup returned is gone.
        assert!(
            !DataFileSet::data_path(&dir, entry.file_id).exists(),
            "merge should have removed file {}",
            entry.file_id
        );
    });

    assert_eq!(value.unwrap(), Some(b"v".to_vec()));
}

/// The same race for `fold`: the snapshot is taken, then a whole `merge`
/// runs before any value is read.
#[test]
fn race_fold_survives_merge_removing_files_after_its_snapshot() {
    let dir = TempDir::new();
    let db = store_for_merge_race(&dir);

    let rows = db.fold_with_hook(
        |key, value, mut rows: Vec<(Vec<u8>, Vec<u8>)>| {
            rows.push((key.to_vec(), value.to_vec()));
            rows
        },
        Vec::new(),
        || {
            db.merge()
                .unwrap();
        },
    );

    let mut rows = rows.unwrap();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            (b"K".to_vec(), b"v".to_vec()),
            (b"other".to_vec(), b"x".to_vec()),
        ]
    );
}

/// Runs `merge` on another thread from inside a write's
/// append-to-publish window, and gives it up to a second to finish
/// *inside* that window. Unfixed, merge completes there. Fixed, merge
/// must wait for the write to publish, so the wait times out and merge
/// finishes after the write returns. The verdict doesn't depend on
/// timing: a merge that can't finish inside the window can't cause the
/// race either.
fn merge_inside_window<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    db: &'env Engine,
) {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    scope.spawn(move || {
        db.merge()
            .unwrap();
        let _ = done_tx.send(());
    });
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(1));
}

/// `put` appends `N`, and before it publishes `N`, a merge tries to run.
/// If it could, it would see `N`'s entry as dead (the keydir doesn't point
/// at it yet), drop it and remove its file; `put` would then publish a
/// location in a file that no longer exists, and the write would be lost.
/// The publish happens under the writer's lock, which a merge needs to
/// start, so the merge waits.
#[test]
fn race_put_survives_merge_between_append_and_publish() {
    let dir = TempDir::new();
    let db = store_for_merge_race(&dir);

    std::thread::scope(|scope| {
        db.put_with_hook(b"N", b"new", now_unix(), || merge_inside_window(scope, &db))
            .unwrap();
    });

    assert_eq!(
        db.get(b"N")
            .unwrap(),
        Some(b"new".to_vec())
    );
    drop(db);
    let reopened = open(&dir);
    assert_eq!(
        reopened
            .get(b"N")
            .unwrap(),
        Some(b"new".to_vec())
    );
}

/// The same window for `delete`. Unfixed, merge still sees `K`'s old
/// value as live (the keydir hasn't dropped it yet), copies it forward,
/// and removes the file holding the tombstone; after a reopen, `K` is
/// back.
#[test]
fn race_delete_survives_merge_between_append_and_publish() {
    let dir = TempDir::new();
    let db = store_for_merge_race(&dir);

    std::thread::scope(|scope| {
        db.delete_with_hook(b"K", now_unix(), || merge_inside_window(scope, &db))
            .unwrap();
    });

    assert_eq!(
        db.get(b"K")
            .unwrap(),
        None
    );
    drop(db);
    let reopened = open(&dir);
    assert_eq!(
        reopened
            .get(b"K")
            .unwrap(),
        None
    );
}

/// Runs `write` on another thread from inside a write's
/// append-to-publish window, and gives it up to a second to finish
/// *inside* that window — the same verdict-by-timeout as
/// `merge_inside_window`. The first write holds the append lock through
/// its publish, so the second can't even append until the first has
/// published: the wait times out and it lands after. If the publish ever
/// moved back outside the lock, the second write would append after the
/// first and publish before it.
fn write_inside_window<'scope, 'env>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    write: impl FnOnce() + Send + 'scope,
) {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    scope.spawn(move || {
        write();
        let _ = done_tx.send(());
    });
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(1));
}

/// Two puts of the same key, the second one entirely inside the first's
/// append-to-publish window. The log holds `v1` then `v2`, so `v2` is the
/// last write: it's what a reopen recovers. With the publish outside the
/// append lock, `v1` published last and overwrote `v2` in the keydir, so
/// the session read `v1` while the disk said `v2`.
#[test]
fn race_two_puts_publish_in_log_order() {
    let dir = TempDir::new();
    let db = open(&dir);

    std::thread::scope(|scope| {
        db.put_with_hook(b"K", b"v1", now_unix(), || {
            write_inside_window(scope, || {
                db.put(b"K", b"v2", now_unix())
                    .unwrap()
            })
        })
        .unwrap();
    });

    let in_session = db
        .get(b"K")
        .unwrap();
    drop(db);
    let after_reopen = open(&dir)
        .get(b"K")
        .unwrap();
    assert_eq!(after_reopen, Some(b"v2".to_vec()), "the log's last write");
    assert_eq!(
        in_session, after_reopen,
        "the keydir must agree with the log"
    );
}

/// The same window, with a delete as the second write. The log holds
/// `v1` then a tombstone, so after a reopen the key is gone. With the
/// publish outside the append lock, `v1` published after the delete had
/// removed the key, and the session saw a deleted key come back.
#[test]
fn race_put_then_delete_publish_in_log_order() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"K", b"v0", now_unix())
        .unwrap();

    std::thread::scope(|scope| {
        db.put_with_hook(b"K", b"v1", now_unix(), || {
            write_inside_window(scope, || {
                db.delete(b"K", now_unix())
                    .unwrap()
            })
        })
        .unwrap();
    });

    let in_session = db
        .get(b"K")
        .unwrap();
    drop(db);
    let after_reopen = open(&dir)
        .get(b"K")
        .unwrap();
    assert_eq!(after_reopen, None, "the log's last write is the tombstone");
    assert_eq!(
        in_session, after_reopen,
        "the keydir must agree with the log"
    );
}

/// Merge is paused just before it repoints `K` to the copy it made; another
/// thread overwrites `K`; merge resumes. Its repoint must lose to the write,
/// not put back the value it copied. The pause is a hook, not a sleep.
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
    db.put(b"K", b"old", now_unix())
        .unwrap(); // lands in its own already-rotated-out file

    let (paused_tx, paused_rx) = std::sync::mpsc::channel::<()>();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();

    let merger_db = std::sync::Arc::clone(&db);
    let merge_thread_handle = std::thread::spawn(move || {
        merger_db
            .merge_with_hook(move |key| {
                // this runs when K is about to be repointed, after it has been copied forward
                if key == b"K" {
                    pause_thread(&paused_tx);
                    resume_thread(&resume_rx);
                }
            })
            .unwrap();
    });

    wait_for_merge(paused_rx); // merge has copied K forward, about to repoint
    db.put(b"K", b"new", now_unix())
        .unwrap(); // race: overwrite K while merge is paused
    resume_merge(resume_tx); // let merge's (now-stale) CAS attempt run

    merge_thread_handle
        .join()
        .unwrap();

    assert_eq!(
        db.get(b"K")
            .unwrap(),
        Some(b"new".to_vec())
    );
}

/// A write that lands while a merge is running must still be the last
/// write after a reopen. Recovery replays files in id order, so the file
/// the write went to has to sort after every file the merge wrote: the
/// merge's copy of `K` is older than the write, whatever order the two
/// reached the disk in.
#[test]
fn race_put_during_merge_is_still_the_last_write_after_reopen() {
    let dir = TempDir::new();
    let db = store_for_merge_race(&dir);
    db.merge_with_hook(|key| {
        if key == b"K" {
            // merge has copied K forward and is about to repoint it
            db.put(b"K", b"new", now_unix())
                .unwrap();
        }
    })
    .unwrap();
    assert_eq!(
        db.get(b"K")
            .unwrap(),
        Some(b"new".to_vec())
    );
    drop(db);
    assert_eq!(
        open(&dir)
            .get(b"K")
            .unwrap(),
        Some(b"new".to_vec()),
        "the reopened store must not go back to the value merge copied"
    );
}

/// The same, with a delete: the key must stay deleted after a reopen.
#[test]
fn race_delete_during_merge_stays_deleted_after_reopen() {
    let dir = TempDir::new();
    let db = store_for_merge_race(&dir);
    db.merge_with_hook(|key| {
        if key == b"K" {
            db.delete(b"K", now_unix())
                .unwrap();
        }
    })
    .unwrap();
    assert_eq!(
        db.get(b"K")
            .unwrap(),
        None
    );
    drop(db);
    assert_eq!(
        open(&dir)
            .get(b"K")
            .unwrap(),
        None,
        "the reopened store must not bring back the value merge copied"
    );
}

fn resume_merge(resume_tx: Sender<()>) {
    resume_tx
        .send(())
        .unwrap();
}

fn wait_for_merge(paused_rx: Receiver<()>) {
    paused_rx
        .recv()
        .unwrap();
}

fn pause_thread(paused_tx: &Sender<()>) {
    paused_tx
        .send(())
        .unwrap();
}

fn resume_thread(resume_rx: &Receiver<()>) {
    resume_rx
        .recv()
        .unwrap(); // continue merge
}
