//! Benchmarks for the in-memory keydir index (`src/keydir.rs`). See
//! `docs/bitcask-implementation-plan.md` §2.
//!
//! Run with: `cargo bench --bench keydir`

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use yabiir::keydir::{Keydir, KeydirEntry, SharedKeydir};

const POPULATION_SIZES: &[usize] = &[100, 1_000, 10_000, 100_000];

fn entry(n: u32) -> KeydirEntry {
    KeydirEntry {
        file_id: n,
        value_size: n,
        value_pos: n as u64,
        timestamp: n,
    }
}

fn key(n: u32) -> Vec<u8> {
    format!("key-{n:010}").into_bytes()
}

fn populated(n: usize) -> Keydir {
    let mut kd = Keydir::new();
    for i in 0..n as u32 {
        kd.insert(&key(i), entry(i));
    }
    kd
}

fn bench_insert_new_key(c: &mut Criterion) {
    // Each iteration inserts a never-before-seen key into a HashMap that
    // already holds `size` entries, isolating the "grow the map" cost from
    // "overwrite in place" (bench_insert_overwrite, below).
    let mut group = c.benchmark_group("insert_new_key");
    // iter_batched rebuilds a `size`-entry Keydir per sample below, so at
    // the largest population size that setup alone dominates the default
    // 5s measurement window. Lower the sample count *and* explicitly
    // declare the longer measurement time this group actually needs —
    // sample_size alone still isn't enough to clear Criterion's default 5s
    // window at 1,000,000 entries, and leaving that implicit just trades
    // one "unable to complete in time" warning for another.
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(15));
    for &size in POPULATION_SIZES {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || populated(size),
                |mut kd| {
                    kd.insert(black_box(&key(size as u32 + 1)), black_box(entry(0)));
                    kd
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_insert_overwrite(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_overwrite");
    for &size in POPULATION_SIZES {
        let mut kd = populated(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| kd.insert(black_box(&key(0)), black_box(entry(1))));
        });
    }
    group.finish();
}

fn bench_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_hit");
    for &size in POPULATION_SIZES {
        let kd = populated(size);
        let lookup_key = key(size as u32 / 2);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| black_box(kd.get(black_box(&lookup_key))));
        });
    }
    group.finish();
}

fn bench_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_miss");
    for &size in POPULATION_SIZES {
        let kd = populated(size);
        let lookup_key = key(size as u32 + 1);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| black_box(kd.get(black_box(&lookup_key))));
        });
    }
    group.finish();
}

fn bench_remove(c: &mut Criterion) {
    let mut group = c.benchmark_group("remove");
    group.sample_size(20); // see insert_new_key's comment
    group.measurement_time(Duration::from_secs(15));
    for &size in POPULATION_SIZES {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || populated(size),
                |mut kd| black_box(kd.remove(black_box(&key(0)))),
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_cas_repoint_success(c: &mut Criterion) {
    // The common case on merge's happy path: nothing raced, the CAS wins.
    let mut group = c.benchmark_group("cas_repoint_success");
    group.sample_size(20); // see insert_new_key's comment
    group.measurement_time(Duration::from_secs(15));
    for &size in POPULATION_SIZES {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter_batched(
                || populated(size),
                |mut kd| black_box(kd.cas_repoint(black_box(&key(0)), entry(0), entry(999))),
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_cas_repoint_failure(c: &mut Criterion) {
    // Merge's race-lost case (plan §7.3): a concurrent put already moved
    // the entry, so `expected_old` is stale and the CAS must bail out
    // without mutating anything.
    let mut group = c.benchmark_group("cas_repoint_failure");
    for &size in POPULATION_SIZES {
        let mut kd = populated(size);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                black_box(kd.cas_repoint(
                    black_box(&key(0)),
                    entry(12345), // never matches — always the failure path
                    entry(999),
                ))
            });
        });
    }
    group.finish();
}

/// `snapshot` is what `fold`/`list_keys` pay once per call, up front,
/// before doing any per-key disk I/O — its cost scales with the whole
/// keydir, unlike every other operation benchmarked here.
fn bench_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot");
    // Unlike the groups above, this is genuine timed work (cloning up to
    // 1,000,000 entries out), not per-sample setup overhead, so keep the
    // full default sample count for a tighter estimate and just give it
    // the longer window that many samples of that actually need.
    group.measurement_time(Duration::from_secs(16));
    for &size in POPULATION_SIZES {
        let shared = SharedKeydir::new(populated(size));
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| black_box(shared.snapshot()));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_insert_new_key,
    bench_insert_overwrite,
    bench_get_hit,
    bench_get_miss,
    bench_remove,
    bench_cas_repoint_success,
    bench_cas_repoint_failure,
    bench_snapshot,
);
criterion_main!(benches);
