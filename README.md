# yabiir

Yet another Bitcask implementation, in Rust — a log-structured hash-table key/value store, built from scratch following Sheehy & Smith's 2010 paper (included locally at [`docs/papers/bitcask-intro.pdf`](docs/papers/bitcask-intro.pdf)).

> **Status: 0.1.0, not yet battle-tested.** Every milestone in the [implementation plan](docs/bitcask-implementation-plan.md) is done and covered by tests (unit, integration, a property-based model test, and concurrency stress tests), but this hasn't run in production anywhere. See [Known limitations](#known-limitations) before trusting it with data you care about.

## What it does

- `put`/`get`/`delete` with append-only writes, O(1) reads (one keydir lookup + one positioned read)
- Crash recovery on `open` — full-scan or hint-file-based, whichever's available
- `merge` (compaction): reclaims space from superseded values and deleted keys, safe to run concurrently with live traffic
- A process-level single-writer lock, so two `is_read_write` handles can't corrupt the same directory
- A small CLI (`kv`) and a synthetic load generator (`loadtest`) for exercising it

## Quick start

```rust
use yabiir::{Bitcask, Engine, Options};

let db = Engine::open("/tmp/my-data", Options::default())?;
db.put(b"key", b"value")?;
assert_eq!(db.get(b"key")?, Some(b"value".to_vec()));
db.delete(b"key")?;
db.merge()?; // compact away old versions and deletes, whenever you want
db.close()?;
```

See [`examples/basic_usage.rs`](examples/basic_usage.rs) for the full API walked through end to end (`put`/`get`/`delete`/`list_keys`/`fold`/`merge`/`close`/reopen), or run it directly:

```sh
cargo run --example basic_usage
```

The `kv` CLI covers the same operations from the shell:

```sh
cargo run --bin kv -- /tmp/my-data add name yabiir
cargo run --bin kv -- /tmp/my-data get name
cargo run --bin kv -- /tmp/my-data list
cargo run --bin kv -- /tmp/my-data merge
```

## Performance

The design goal here is **predictable latency**, not just peak throughput — a storage engine that's fast on average but occasionally stalls for tens of milliseconds is worse for most real workloads than one that's a bit slower but consistent. That's reflected in how this crate was actually tuned: not by chasing a single throughput number, but by profiling a sustained mixed workload with [`cargo flamegraph`](https://github.com/flamegraph-rs/flamegraph) and fixing whatever showed up as a real, measured cost — including once catching a "safe, drop-in" optimization that would have silently turned every keydir operation into O(n) for a plausible real-world key pattern (see below).

**Tooling, if you want to reproduce or extend this:**
- `src/bin/loadtest.rs` — a configurable mixed get/put/delete load generator that reports periodic p50/p90/p99/p99.9/max latency per operation kind, specifically so pauses show up as a visible spike rather than getting smoothed into an average.
- `scripts/flamegraph.sh` — wraps `cargo flamegraph --profile profiling --bin loadtest` (the `profiling` Cargo profile keeps release optimizations but adds debug symbols so frames resolve to real function names).
- `cargo bench` — Criterion suites for `format`, `keydir`, `datafile`, and `merge`, each isolating one layer's cost.

**What profiling found and fixed, in order:**

1. **Lock contention on reads.** `get` was taking a read lock on the active file's `RwLock` to special-case reading recently-written keys — unnecessary, since every write already flushes before returning (see below), so a fresh, independent read handle sees the same bytes. Removed the special case entirely; `get`/`fold` now never touch that lock.
2. **Keydir hash cost.** The keydir's `HashMap` used Rust's default SipHash, which showed up directly in profiling. The first fix tried (`rustc-hash`/FxHash) looked like a clean win on `get` in isolation — until benchmarking the rest of the keydir's operations showed 40-150%+ regressions that scaled *linearly* with map size. The cause: FxHash has almost no avalanche step, and for sequential/structured string keys (`key-0000000042`-style — auto-incrementing IDs, timestamps, zero-padded counters) that collapses nearly every key into a single hash bucket, turning O(1) map operations into O(n). Verified directly: 10,000 sequential keys landed in 1 of 16,384 buckets. Switched to `ahash` instead, confirmed it doesn't have the same failure mode on the same adversarial key pattern, and re-benchmarked clean.
3. **Per-entry flush during merge.** Every write flushes immediately (a `write()` syscall, not `fsync`) so a concurrent reader never sees a stale/short file — correct and necessary for `put`, but `merge`'s compaction pass was paying that same per-entry cost while copying forward every still-live value, which was a measurable share of merge taking 1.0-1.5s per pass on a modest dataset and a large part of why merge running concurrently with load caused 36-55ms foreground latency spikes. Batched merge's flushes (once per 256 entries, or immediately at a rotation boundary) instead of once per entry, with keydir repoints deferred to line up with the batch boundary so the same "never repoint before bytes are flushed" guarantee holds. Verified with a controlled A/B (`git stash` to isolate the change): merge durations 2.25s/3.94s/5.44s without batching vs. 2.16s/3.32s/3.86s/3.99s with it, over the same load window — faster at every comparable point, one more merge pass completed in the same time.
4. **Unbatched `fsync` under `should_sync_on_put`.** Every `put`/`delete` issued its own `fsync`, even though writes were already fully serialized through one lock — concurrent callers each paid for a separate fsync instead of sharing one. Added group commit (`src/commit.rs`): the first caller to arrive becomes leader and fsyncs (without holding the write lock for the fsync itself), everyone else just waits for that shared fsync to cover their write. Verified with a controlled A/B (`git worktree` at the prior commit): `put` p50 49.24ms → 12.82ms, p99 270.16ms → 16.73ms, max 525.15ms → 22.88ms, combined write throughput ≈146/s → ≈609/s — the tail compressed as much as the average improved.

Full writeups live in [`docs/merge-batched-flushing.pdf`](docs/merge-batched-flushing.pdf) ([`.typ`](docs/merge-batched-flushing.typ)) and [`docs/group-commit.pdf`](docs/group-commit.pdf) ([`.typ`](docs/group-commit.typ)) if you want the detailed numbers and reasoning, not just the summary above.

**Known gaps in the benchmark suite, not the code:**
- **`benches/merge.rs`'s existing suite can't see the flush-batching change** — it deliberately uses `max_file_size: 1` to control the live/dead entry ratio precisely, which happens to make every entry trigger a (stronger) rotation-fsync before the batching logic is ever reached. The fix was verified against a realistic file size via `loadtest` instead (see the PDF above); the benchmark suite itself hasn't been changed to also cover this.

## Known limitations

These are called out explicitly in the relevant module docs, not hidden:

- **Merge/recovery ordering gap** (`src/merge.rs`): a key written *truly concurrently* with a merge that also touches that key can, in rare cases, be resolved incorrectly by a *subsequent* recovery/reopen — even though the live, in-memory keydir stays correct for the rest of that session. Closing this fully needs merge output to reuse freed file ids via a carefully-ordered rename step; not implemented.
- **No loom coverage** for `src/keydir.rs`'s concurrency, flagged as optional in the implementation plan and never picked up.
- **The single-writer lock is a best-effort `flock`**, not reliable on network filesystems (NFS in particular). Don't point this at an NFS mount and expect the lock to actually exclude a second writer.
- **`should_sync_on_put` defaults to `false`** — writes are flushed (visible to concurrent readers, survives a process crash) but not `fsync`'d by default, so a true power outage can lose the last few writes unless you opt into `sync_on_put: true`. This is a standard, documented tradeoff, not a bug.

## Testing

```sh
cargo test                          # 59 unit tests + tests/model.rs's proptest model test
PROPTEST_CASES=2000 cargo test --test model  # more property-based cases, for extra confidence
cargo bench                         # Criterion suites — format, keydir, datafile, merge
```

The proptest model test runs random sequences of `put`/`delete`/`get`/`reopen`/`merge` against a plain `HashMap` reference model over a small, deliberately-colliding key alphabet — it's what caught a real recovery-ordering bug (documented in `src/merge.rs`) during development.

## Design notes

The full implementation plan — every design decision and why, walked through milestone by milestone — is in [`docs/bitcask-implementation-plan.md`](docs/bitcask-implementation-plan.md). Start there for the on-disk format, the keydir, recovery, merge, and locking, in that order.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
