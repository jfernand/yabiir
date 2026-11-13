use super::*;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// A test `Metrics` implementation counting how many times each
/// callback fired — proves `Engine` actually calls into a caller-
/// supplied `Metrics`, not just that `Options::metrics` compiles.
#[derive(Default)]
struct CountingMetrics {
    puts: AtomicU64,
    gets: AtomicU64,
    deletes: AtomicU64,
    merges: AtomicU64,
    syncs: AtomicU64,
}

impl crate::Metrics for CountingMetrics {
    fn record_put(&self, _duration: std::time::Duration) {
        self.puts
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
    fn record_get(&self, _duration: std::time::Duration) {
        self.gets
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
    fn record_delete(&self, _duration: std::time::Duration) {
        self.deletes
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
    fn record_merge(&self, _duration: std::time::Duration) {
        self.merges
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
    fn record_sync(&self, _duration: std::time::Duration) {
        self.syncs
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
}

#[test]
fn metrics_are_recorded_for_every_op_kind() {
    let dir = TempDir::new();
    let metrics = std::sync::Arc::new(CountingMetrics::default());
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1, // every put rotates — guarantees merge has something to do
            metrics: Some(metrics.clone() as std::sync::Arc<dyn crate::Metrics>),
            ..Options::default()
        },
    )
    .unwrap();

    db.put(b"a", b"1", now_unix())
        .unwrap();
    db.put(b"b", b"2", now_unix())
        .unwrap();
    assert_eq!(
        db.get(b"a")
            .unwrap(),
        Some(b"1".to_vec())
    );
    db.get(b"missing")
        .unwrap(); // a miss still counts as a get
    db.delete(b"a", now_unix())
        .unwrap();
    db.delete(b"missing", now_unix())
        .unwrap(); // no-op delete still counts
    db.merge()
        .unwrap();
    db.sync()
        .unwrap();

    assert_eq!(
        metrics
            .puts
            .load(AtomicOrdering::Relaxed),
        2
    );
    assert_eq!(
        metrics
            .gets
            .load(AtomicOrdering::Relaxed),
        2
    );
    assert_eq!(
        metrics
            .deletes
            .load(AtomicOrdering::Relaxed),
        2
    );
    assert_eq!(
        metrics
            .merges
            .load(AtomicOrdering::Relaxed),
        1
    );
    assert_eq!(
        metrics
            .syncs
            .load(AtomicOrdering::Relaxed),
        1
    );
}

/// With no `Options::metrics` set, `Engine` falls back to a no-op —
/// this just has to not panic or misbehave, exercising the `unwrap_or_else`
/// path in `open()` that a metrics-less `Options::default()` always takes.
#[test]
fn engine_works_normally_with_no_metrics_configured() {
    let dir = TempDir::new();
    let db = open(&dir);
    db.put(b"k", b"v", now_unix())
        .unwrap();
    assert_eq!(
        db.get(b"k")
            .unwrap(),
        Some(b"v".to_vec())
    );
}
