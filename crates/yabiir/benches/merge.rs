//! Benchmarks for merge/compaction (`src/merge.rs`). See
//! `docs/bitcask-implementation-plan.md` §7.
//!
//! Real filesystem I/O throughout: each iteration builds a directory of
//! already-rotated data files (mimicking writes/overwrites accumulated
//! since the last merge — `max_file_size: 1` forces every `put` into its
//! own file, so a dataset's total entry count and live/dead split are both
//! under direct control here), then times `Engine::merge()` compacting it.
//! Run with: `cargo bench --bench merge`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use yabiir::{Bitcask, Engine, Options, now_unix};

/// Minimal self-cleaning temp directory — same pattern used throughout this
/// crate's own tests and other benches.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "yabiir-merge-bench-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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

/// Write `versions_per_key` sequential updates to each of `num_keys`
/// distinct keys (interleaved: all keys' v0, then all keys' v1, ...), with
/// `max_file_size: 1` so every `put` rotates into its own already-closed
/// file — every one of them (except the final, still-empty active file)
/// becomes eligible merge input. Only each key's *last* write survives as
/// live, so `versions_per_key` directly controls the dead/live ratio
/// merge has to sort through: 1 means nothing is dead, 20 means 95% of
/// scanned entries are superseded and get dropped.
fn build_dataset(num_keys: usize, versions_per_key: usize, value_size: usize) -> (TempDir, Engine) {
    let dir = TempDir::new();
    let db = Engine::open(
        &*dir,
        Options {
            max_file_size: 1,
            ..Options::default()
        },
    )
    .unwrap();
    let value = vec![0xABu8; value_size];
    for _ in 0..versions_per_key {
        for k in 0..num_keys {
            db.put(format!("key-{k:06}").as_bytes(), &value, now_unix())
                .unwrap();
        }
    }
    (dir, db)
}

/// The cheap path: nothing to compact (only the active file exists, so
/// `input_ids` is empty and `merge` returns immediately). Confirms that
/// case is actually near-free, not accidentally doing a full directory
/// scan's worth of work for nothing.
fn bench_merge_noop(c: &mut Criterion) {
    c.bench_function("merge_noop", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new();
                let db = Engine::open(&*dir, Options::default()).unwrap();
                for i in 0..50u32 {
                    db.put(format!("k{i}").as_bytes(), b"v", now_unix())
                        .unwrap();
                }
                (dir, db)
            },
            |(dir, db)| {
                db.merge()
                    .unwrap(); // unit result — nothing for black_box to guard
                (dir, db) // keep both alive until the batch is torn down
            },
            BatchSize::SmallInput,
        );
    });
}

/// Fixed key count, varying how many dead (superseded) versions of each key
/// merge has to scan past before finding the live one — isolates the cost
/// of dropping dead entries from the cost of copying live ones forward.
fn bench_merge_by_dead_ratio(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge_by_versions_per_key");
    group.sample_size(10); // real per-iteration disk setup + merge; keep it affordable
    const NUM_KEYS: usize = 200;
    const VALUE_SIZE: usize = 100;
    for &versions in &[1usize, 5, 20] {
        let total_entries = (NUM_KEYS * versions) as u64;
        group.throughput(Throughput::Elements(total_entries));
        group.bench_with_input(
            BenchmarkId::from_parameter(versions),
            &versions,
            |b, &versions| {
                b.iter_batched(
                    || build_dataset(NUM_KEYS, versions, VALUE_SIZE),
                    |(dir, db)| {
                        db.merge()
                            .unwrap();
                        (dir, db)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

/// Fixed total live data (one live entry per key, `versions_per_key: 1`, so
/// no dead-entry-skipping cost at all), varying how many separate input
/// files it's spread across — isolates the cost of opening/scanning many
/// small files from the cost of scanning the entries themselves.
fn bench_merge_by_input_file_count(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge_by_input_file_count");
    group.sample_size(10);
    const VALUE_SIZE: usize = 100;
    for &num_files in &[10usize, 100, 500] {
        group.throughput(Throughput::Elements(num_files as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(num_files),
            &num_files,
            |b, &num_files| {
                b.iter_batched(
                    || build_dataset(num_files, 1, VALUE_SIZE),
                    |(dir, db)| {
                        db.merge()
                            .unwrap();
                        (dir, db)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_merge_noop,
    bench_merge_by_dead_ratio,
    bench_merge_by_input_file_count,
);
criterion_main!(benches);
