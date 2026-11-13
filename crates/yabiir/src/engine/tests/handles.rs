use super::*;

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
    writer
        .put(b"k", b"v", now_unix())
        .unwrap();

    let reader = Engine::open(
        &*dir,
        Options {
            is_read_write: false,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(
        reader
            .get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );

    // the writer is still usable too — the read-only open didn't
    // disturb it.
    writer
        .put(b"k2", b"v2", now_unix())
        .unwrap();
    assert_eq!(
        writer
            .get(b"k2")
            .unwrap(),
        Some(b"v2".to_vec())
    );
}

/// Plan §8.3: dropping the read_write handle releases the lock, so a
/// subsequent read_write open succeeds.
#[test]
fn dropping_the_write_handle_releases_the_lock_for_the_next_open() {
    let dir = TempDir::new();
    {
        let db = open(&dir);
        db.put(b"k", b"v", now_unix())
            .unwrap();
        // dropped at end of this block
    }
    let db = open(&dir); // must not fail with AlreadyLocked
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
}

/// `close()` also releases the lock (it's just an early, explicit
/// version of the same drop), not only an implicit drop out of scope.
#[test]
fn close_releases_the_lock_for_the_next_open() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"v", now_unix())
        .unwrap();
    db.close()
        .unwrap();

    let db = open(&dir); // must not fail with AlreadyLocked
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
}

/// A read-only handle never touches the write lock at all, so two of
/// them can coexist even with no writer present.
#[test]
fn multiple_read_only_opens_coexist() {
    let dir = TempDir::new();
    {
        let db = open(&dir);
        db.put(b"k", b"v", now_unix())
            .unwrap();
    }
    let read_only_opts = || Options {
        is_read_write: false,
        ..Options::default()
    };
    let r1 = Engine::open(&*dir, read_only_opts()).unwrap();
    let r2 = Engine::open(&*dir, read_only_opts()).unwrap();
    assert_eq!(
        r1.get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
    assert_eq!(
        r2.get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
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
        db.put(b"k", b"v", now_unix())
            .unwrap();
    }
    let ro = Engine::open(
        &*dir,
        Options {
            is_read_write: false,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(matches!(ro.merge(), Err(Error::ReadOnly)));
}
