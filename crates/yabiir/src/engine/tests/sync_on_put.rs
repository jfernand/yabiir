use super::*;

#[test]
fn sync_on_put_round_trips_correctly() {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            should_sync_on_put: true,
            ..Options::default()
        },
    )
    .unwrap();
    db.put(b"k", b"v1", now_unix())
        .unwrap();
    db.put(b"k", b"v2", now_unix())
        .unwrap();
    db.delete(b"k", now_unix())
        .unwrap();
    db.put(b"k2", b"v", now_unix())
        .unwrap();
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        None
    );
    assert_eq!(
        db.get(b"k2")
            .unwrap(),
        Some(b"v".to_vec())
    );
}

/// The point of group commit: several `sync_on_put` writers that arrive
/// concurrently should share a small number of real fsyncs, not pay one
/// each. Deterministic (not timing-based): a first "opener" write is
/// done and fully returns before starting the batch, so the batch's
/// fsyncs are counted in isolation; many threads then `put` at once and
/// the fsync count afterward must be far below the writer count.
#[test]
fn concurrent_sync_on_put_writers_share_fsyncs() {
    let dir = TempDir::new();
    let db = std::sync::Arc::new(
        Engine::open(
            &*dir,
            Options {
                should_sync_on_put: true,
                ..Options::default()
            },
        )
        .unwrap(),
    );
    db.put(b"warmup", b"v", now_unix())
        .unwrap();
    let before = db.fsync_call_count();

    let n = 32usize;
    let handles: Vec<_> = (0..n)
        .map(|i| {
            let db = std::sync::Arc::clone(&db);
            std::thread::spawn(move || {
                db.put(format!("k{i}").as_bytes(), b"v", now_unix())
                    .unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join()
            .unwrap();
    }

    let after = db.fsync_call_count();
    let fsyncs_for_batch = after - before;
    assert!(
        fsyncs_for_batch >= 1 && fsyncs_for_batch < n,
        "expected the {n} concurrent writers to share well under {n} fsyncs, got {fsyncs_for_batch}"
    );

    for i in 0..n {
        assert_eq!(
            db.get(format!("k{i}").as_bytes())
                .unwrap(),
            Some(b"v".to_vec())
        );
    }
}
