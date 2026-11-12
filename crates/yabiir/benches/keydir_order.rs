//! Hash vs. sorted keydir, measured on the two maps themselves, before the engine
//! can use either: the aHash `HashMap` the keydir uses today and a `BTreeMap`, with
//! the same keys and values, for every operation the keydir performs, plus the one
//! only the sorted map can answer cheaply (a range of keys).
//!
//! Run with: `cargo bench --bench keydir_order`

use std::collections::{BTreeMap, HashMap};
use std::hint::black_box;
use std::time::Duration;

use ahash::RandomState;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use yabiir::keydir::KeydirEntry;

const SIZES: &[usize] = &[1_000, 100_000, 1_000_000];
/// How many keys a range query returns.
const RANGE_LEN: usize = 100;

type Hashed = HashMap<Box<[u8]>, KeydirEntry, RandomState>;
type Sorted = BTreeMap<Box<[u8]>, KeydirEntry>;

fn entry(n: u32) -> KeydirEntry {
    KeydirEntry {
        file_id: n,
        value_size: n,
        value_pos: n as u64,
        timestamp: n,
    }
}

fn key(n: usize) -> Box<[u8]> {
    format!("key-{n:010}")
        .into_bytes()
        .into_boxed_slice()
}

fn hashed(n: usize) -> Hashed {
    (0..n)
        .map(|i| (key(i), entry(i as u32)))
        .collect()
}

fn sorted(n: usize) -> Sorted {
    (0..n)
        .map(|i| (key(i), entry(i as u32)))
        .collect()
}

fn bench_get_hit(c: &mut Criterion) {
    let mut group = c.benchmark_group("order/get_hit");
    for &n in SIZES {
        let (h, s) = (hashed(n), sorted(n));
        let k = key(n / 2);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("hashed", n), &n, |b, _| {
            b.iter(|| black_box(h.get(black_box(&k[..]))))
        });
        group.bench_with_input(BenchmarkId::new("sorted", n), &n, |b, _| {
            b.iter(|| black_box(s.get(black_box(&k[..]))))
        });
    }
    group.finish();
}

/// `get_hit` looks the same key up every iteration, so the path to it stays in
/// the CPU cache — the best case for the tree. Here each iteration looks up the
/// next of 4,096 keys picked at random from the whole map, the access pattern
/// of a real workload.
fn bench_get_random(c: &mut Criterion) {
    let mut group = c.benchmark_group("order/get_random");
    for &n in SIZES {
        let (h, s) = (hashed(n), sorted(n));
        // A fixed LCG, so both maps see the same keys in the same order.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let keys: Vec<Box<[u8]>> = (0..4096)
            .map(|_| {
                x = x
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                key((x >> 33) as usize % n)
            })
            .collect();
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("hashed", n), &n, |b, _| {
            let mut i = 0;
            b.iter(|| {
                i = (i + 1) & 4095;
                black_box(h.get(black_box(&keys[i][..])))
            })
        });
        group.bench_with_input(BenchmarkId::new("sorted", n), &n, |b, _| {
            let mut i = 0;
            b.iter(|| {
                i = (i + 1) & 4095;
                black_box(s.get(black_box(&keys[i][..])))
            })
        });
    }
    group.finish();
}

fn bench_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("order/get_miss");
    for &n in SIZES {
        let (h, s) = (hashed(n), sorted(n));
        let k = key(n + 1);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("hashed", n), &n, |b, _| {
            b.iter(|| black_box(h.get(black_box(&k[..]))))
        });
        group.bench_with_input(BenchmarkId::new("sorted", n), &n, |b, _| {
            b.iter(|| black_box(s.get(black_box(&k[..]))))
        });
    }
    group.finish();
}

fn bench_overwrite(c: &mut Criterion) {
    // What a put of an existing key, and merge's repoint, do to the map.
    let mut group = c.benchmark_group("order/overwrite");
    for &n in SIZES {
        let (mut h, mut s) = (hashed(n), sorted(n));
        let k = key(n / 2);
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("hashed", n), &n, |b, _| {
            b.iter(|| {
                if let Some(slot) = h.get_mut(black_box(&k[..])) {
                    *slot = black_box(entry(1));
                }
            })
        });
        group.bench_with_input(BenchmarkId::new("sorted", n), &n, |b, _| {
            b.iter(|| {
                if let Some(slot) = s.get_mut(black_box(&k[..])) {
                    *slot = black_box(entry(1));
                }
            })
        });
    }
    group.finish();
}

fn bench_build(c: &mut Criterion) {
    // Building the whole map from n entries: what `open` pays to rebuild the keydir,
    // apart from reading the files.
    let mut group = c.benchmark_group("order/build");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    for &n in SIZES {
        let entries: Vec<(Box<[u8]>, KeydirEntry)> = (0..n)
            .map(|i| (key(i), entry(i as u32)))
            .collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("hashed", n), &n, |b, _| {
            b.iter_batched(
                || entries.clone(),
                |es| {
                    let mut m = Hashed::default();
                    for (k, v) in es {
                        m.insert(k, v);
                    }
                    m
                },
                criterion::BatchSize::LargeInput,
            )
        });
        group.bench_with_input(BenchmarkId::new("sorted", n), &n, |b, _| {
            b.iter_batched(
                || entries.clone(),
                |es| {
                    let mut m = Sorted::new();
                    for (k, v) in es {
                        m.insert(k, v);
                    }
                    m
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }
    group.finish();
}

fn bench_range(c: &mut Criterion) {
    // The RANGE_LEN keys from the middle of the store, in order: one descent and a
    // walk for the sorted map; a pass over every key, then a sort, for the hashed one.
    let mut group = c.benchmark_group("order/range_100");
    group.sample_size(20);
    for &n in SIZES {
        let (h, s) = (hashed(n), sorted(n));
        let (lo, hi) = (key(n / 2), key(n / 2 + RANGE_LEN));
        group.throughput(Throughput::Elements(RANGE_LEN as u64));
        group.bench_with_input(BenchmarkId::new("hashed", n), &n, |b, _| {
            b.iter(|| {
                let mut hits: Vec<(&[u8], &KeydirEntry)> = h
                    .iter()
                    .filter(|(k, _)| (&lo[..]..&hi[..]).contains(&&k[..]))
                    .map(|(k, v)| (&k[..], v))
                    .collect();
                hits.sort_unstable_by(|a, b| {
                    a.0.cmp(b.0)
                });
                black_box(hits)
            })
        });
        group.bench_with_input(BenchmarkId::new("sorted", n), &n, |b, _| {
            b.iter(|| {
                let hits: Vec<(&[u8], &KeydirEntry)> = s
                    .range::<[u8], _>((
                        std::ops::Bound::Included(&lo[..]),
                        std::ops::Bound::Excluded(&hi[..]),
                    ))
                    .map(|(k, v)| (&k[..], v))
                    .collect();
                black_box(hits)
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_get_hit,
    bench_get_random,
    bench_get_miss,
    bench_overwrite,
    bench_build,
    bench_range,
);
criterion_main!(benches);
