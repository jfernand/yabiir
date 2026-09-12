//! Benchmarks for data file management (`src/datafile.rs`). See
//! `docs/bitcask-implementation-plan.md` §3.
//!
//! These exercise real filesystem I/O (not an in-memory fake), since the
//! whole point of `ActiveFile`/`DataFileSet` is I/O behavior — buffering,
//! fsync cost, and positioned reads. Run with: `cargo bench --bench datafile`

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use yabiir::datafile::{ActiveFile, DataFileSet};
use yabiir::format;

const VALUE_SIZES: &[usize] = &[64, 1024, 64 * 1024];

/// Minimal self-cleaning temp directory — mirrors the one in
/// `src/datafile.rs`'s own tests, duplicated here since that one is
/// private to this crate's test module and benches are a separate crate.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "yabiir-datafile-bench-{}-{}-{}",
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

/// `ActiveFile::append` without a sync per write — the common case
/// (`sync_on_put: false`), buffered through `BufWriter` so cost is
/// dominated by encoding + memcpy into the buffer, not syscalls.
fn bench_append_no_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_no_sync");
    for &size in VALUE_SIZES {
        let value = vec![0xABu8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &value, |b, value| {
            b.iter_batched(
                || {
                    let dir = TempDir::new();
                    let active = ActiveFile::create(&dir, 1).unwrap();
                    (dir, active)
                },
                |(dir, mut active)| {
                    for i in 0..100u32 {
                        let encoded =
                            format::encode_entry(format!("k{i}").as_bytes(), value, false, 0);
                        active.append(black_box(&encoded)).unwrap();
                    }
                    dir // keep alive until the batch is torn down
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// `ActiveFile::append` immediately followed by `sync` on every call — the
/// `sync_on_put: true` path. Dominated by the `fsync`/`sync_data` syscall,
/// so this is intentionally run with a small sample size (real fsyncs are
/// slow and noisy compared to the in-memory benches elsewhere).
fn bench_append_with_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_with_sync");
    group.sample_size(20);
    let value = vec![0xCDu8; 1024];
    group.throughput(Throughput::Bytes(value.len() as u64));
    group.bench_function("1024", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new();
                let active = ActiveFile::create(&dir, 1).unwrap();
                (dir, active)
            },
            |(dir, mut active)| {
                let encoded = format::encode_entry(b"key", &value, false, 0);
                active.append(black_box(&encoded)).unwrap();
                active.sync().unwrap();
                dir
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// Positioned read of a just-written entry through `ActiveFile::read_at`,
/// i.e. before the file has rotated out — no `DataFileSet` lookup involved.
fn bench_active_file_read_at(c: &mut Criterion) {
    let mut group = c.benchmark_group("active_file_read_at");
    for &size in VALUE_SIZES {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 1).unwrap();
        let value = vec![0xEFu8; size];
        let encoded = format::encode_entry(b"key", &value, false, 0);
        let (_, value_pos, _) = active.append(&encoded).unwrap();
        active.sync().unwrap();

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| black_box(active.read_at(value_pos, size as u32).unwrap()));
        });
    }
    group.finish();
}

/// Positioned read through `DataFileSet::read_at` against an immutable
/// (rotated-out) file — first call per `file_id` pays the `File::open`
/// cost, later calls hit the cached handle. Benchmarks the steady-state
/// (already-cached) case, which is what a hot key's repeated reads look
/// like in practice.
fn bench_data_file_set_read_at_cached(c: &mut Criterion) {
    let mut group = c.benchmark_group("data_file_set_read_at_cached");
    for &size in VALUE_SIZES {
        let dir = TempDir::new();
        let mut active = ActiveFile::create(&dir, 1).unwrap();
        let value = vec![0x11u8; size];
        let encoded = format::encode_entry(b"key", &value, false, 0);
        let (file_id, value_pos, _) = active.append(&encoded).unwrap();
        active.sync().unwrap();
        drop(active); // now immutable

        let files = DataFileSet::new(&*dir);
        files.read_at(file_id, value_pos, size as u32).unwrap(); // warm the handle cache

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                black_box(
                    files
                        .read_at(file_id, value_pos, size as u32)
                        .unwrap(),
                )
            });
        });
    }
    group.finish();
}

/// `DataFileSet::discover` scanning a directory of already-rotated data
/// files, at a few directory sizes — this runs once per `open()` in the
/// real engine, so its cost matters for startup latency as a Bitcask
/// directory accumulates files between merges.
fn bench_discover(c: &mut Criterion) {
    let mut group = c.benchmark_group("discover");
    for &n in &[10usize, 100, 1000] {
        let dir = TempDir::new();
        for file_id in 0..n as u32 {
            ActiveFile::create(&dir, file_id).unwrap();
        }
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(DataFileSet::discover(black_box(&dir)).unwrap()));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_append_no_sync,
    bench_append_with_sync,
    bench_active_file_read_at,
    bench_data_file_set_read_at_cached,
    bench_discover,
);
criterion_main!(benches);
