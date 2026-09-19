//! Benchmarks for the on-disk entry/hint encode-decode path
//! (`src/format/mod.rs`). See `docs/bitcask-implementation-plan.md` §1.
//!
//! Run with: `cargo bench --bench format`

use std::hint::black_box;
use std::io::Cursor;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use yabiir::format::{
    EntryHeader, HEADER_SIZE, HINT_HEADER_SIZE, decode_entry_header, decode_hint_header,
    encode_entry, encode_hint, read_entry, read_hint, verify_crc,
};

const KEY: &[u8] = b"benchmark-key-0000000000";

const VALUE_SIZES: &[usize] = &[64, 1024, 16 * 1024, 32 * 1024, 64 * 1024];

fn bench_encode_entry(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode_entry");
    for &size in VALUE_SIZES {
        let value = vec![0xABu8; size];
        group.throughput(Throughput::Bytes((HEADER_SIZE + KEY.len() + size) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &value, |b, value| {
            b.iter(|| black_box(encode_entry(black_box(KEY), black_box(value), false, 0)));
        });
    }
    group.finish();
}

fn bench_decode_entry_header(c: &mut Criterion) {
    let encoded = encode_entry(KEY, b"value", false, 1_700_000_000).into_bytes();
    let header_buf: [u8; HEADER_SIZE] = encoded[..HEADER_SIZE]
        .try_into()
        .unwrap();

    c.bench_function("decode_entry_header", |b| {
        b.iter(|| black_box(decode_entry_header(black_box(&header_buf))));
    });
}

fn bench_verify_crc(c: &mut Criterion) {
    let mut group = c.benchmark_group("verify_crc");
    for &size in VALUE_SIZES {
        let encoded = encode_entry(KEY, &vec![0xCDu8; size], false, 0).into_bytes();
        let (crc, _) = decode_entry_header(
            &encoded[..HEADER_SIZE]
                .try_into()
                .unwrap(),
        );
        let rest = &encoded[4..];
        group.throughput(Throughput::Bytes(rest.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), rest, |b, rest| {
            b.iter(|| black_box(verify_crc(black_box(crc), black_box(rest))));
        });
    }
    group.finish();
}

/// End-to-end: sequentially reading a fully-decoded, CRC-verified entry back
/// out of a byte buffer, exercising `read_entry` (header read + body read +
/// CRC check + key/value split) as a whole — this is the hot path for
/// startup recovery scanning a data file.
fn bench_read_entry(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_entry");
    for &size in VALUE_SIZES {
        let encoded = encode_entry(KEY, &vec![0xEFu8; size], false, 0).into_bytes();
        group.throughput(Throughput::Bytes(encoded.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &encoded, |b, encoded| {
            b.iter(|| {
                let mut cursor = Cursor::new(black_box(encoded.as_slice()));
                black_box(read_entry(&mut cursor).unwrap())
            });
        });
    }
    group.finish();
}

fn bench_encode_hint(c: &mut Criterion) {
    let header = EntryHeader {
        timestamp: 1_700_000_000,
        key_size: KEY.len() as u32,
        value_size: 1024,
        tombstone: false,
    };
    c.bench_function("encode_hint", |b| {
        b.iter(|| {
            black_box(encode_hint(
                black_box(KEY),
                black_box(&header),
                black_box(4096),
            ))
        });
    });
}

fn bench_decode_hint_header(c: &mut Criterion) {
    let header = EntryHeader {
        timestamp: 1_700_000_000,
        key_size: KEY.len() as u32,
        value_size: 1024,
        tombstone: false,
    };
    let encoded = encode_hint(KEY, &header, 4096).into_bytes();
    let hint_header_buf: [u8; HINT_HEADER_SIZE] = encoded[..HINT_HEADER_SIZE]
        .try_into()
        .unwrap();

    c.bench_function("decode_hint_header", |b| {
        b.iter(|| black_box(decode_hint_header(black_box(&hint_header_buf))));
    });
}

fn bench_read_hint(c: &mut Criterion) {
    let header = EntryHeader {
        timestamp: 1_700_000_000,
        key_size: KEY.len() as u32,
        value_size: 1024,
        tombstone: false,
    };
    let encoded = encode_hint(KEY, &header, 4096).into_bytes();

    c.bench_function("read_hint", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(black_box(encoded.as_slice()));
            black_box(read_hint(&mut cursor).unwrap())
        });
    });
}

criterion_group!(
    benches,
    bench_encode_entry,
    bench_decode_entry_header,
    bench_verify_crc,
    bench_read_entry,
    bench_encode_hint,
    bench_decode_hint_header,
    bench_read_hint,
);
criterion_main!(benches);
