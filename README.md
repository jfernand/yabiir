# yabiir

Yet another Bitcask implementation, in Rust — a log-structured hash-table key/value store, built from scratch following
Sheehy & Smith's 2010 paper (included locally at [`docs/papers/bitcask-intro.pdf`](docs/papers/bitcask-intro.pdf)).

> **Status: not yet battle-tested.** Covered by tests (unit, integration, a property-based model test, and
> concurrency stress tests), but this hasn't run in production anywhere. See [Known limitations](#known-limitations)
> before trusting it with data you care about.

## What it does

- `put`/`get`/`delete` with append-only writes, O(1) reads (one keydir lookup + one positioned read)
- Crash recovery on `open` — full-scan or hint-file-based, whichever's available
- `merge` (compaction): reclaims space from superseded values and deleted keys, safe to run concurrently with live
  traffic
- A process-level single-writer lock, so two writable (`is_read_write: true`) handles can't corrupt the same directory
- A small CLI (`kv`) and a synthetic load generator (`loadtest`) for exercising it
- Optional observability: `put`/`get`/`delete`/`merge`/`sync` call durations via `Options::metrics`, and structured
  logging/spans via the `tracing` feature — see [Observability](#observability)

## Quick start

```rust
use yabiir::{Bitcask, Engine, Options, now_unix};

let db = Engine::open("/tmp/my-data", Options::default())?;
db.put(b"key", b"value", now_unix())?;
assert_eq!(db.get(b"key")?, Some(b"value".to_vec()));
db.delete(b"key", now_unix())?;
db.merge()?; // compact away old versions and deletes, whenever you want
db.close()?;
```

See [`examples/basic_usage.rs`](examples/basic_usage.rs) for the full API walked through end to end (`put`/`get`/
`delete`/`list_keys`/`fold`/`merge`/`close`/reopen), or run it directly:

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

The design goal is **predictable latency**, not just peak throughput — a storage engine that's fast on average but
occasionally stalls for tens of milliseconds is worse for most real workloads than one that's a bit slower but
consistent. A few design choices follow directly from that:

- `get`/`fold` never take a lock on the active (currently-written-to) file — every write flushes before returning, so
  a fresh, independent read handle always sees the same bytes a locked read would.
- The keydir's `HashMap` uses `ahash` rather than the default SipHash or `rustc-hash`/FxHash: `ahash` is fast without
  FxHash's weakness on sequential/structured keys (`key-0000000042`-style — auto-incrementing IDs, timestamps,
  zero-padded counters), which can collapse almost every key into one hash bucket and turn O(1) map operations into
  O(n).
- `merge`'s compaction pass batches its flushes (every 256 entries, or immediately at a rotation boundary) instead of
  flushing after every entry, with keydir repoints deferred to line up with each batch — so a long merge running
  concurrently with live traffic doesn't stall foreground `put`/`get`/`delete` calls behind its own I/O.
- With `should_sync_on_put: true`, concurrent writers share a single `fsync` (group commit, `src/commit.rs`) instead
  of each paying for their own — one caller becomes the leader and fsyncs without holding the write lock, everyone
  else just waits for that shared fsync to cover their write.

Full design writeups — including the profiling methodology and measured before/after numbers — live in
[`docs/merge-batched-flushing.pdf`](docs/merge-batched-flushing.pdf)
([`.typ`](docs/merge-batched-flushing.typ)) and [`docs/group-commit.pdf`](docs/group-commit.pdf)
([`.typ`](docs/group-commit.typ)).

**Tooling, if you want to reproduce or extend this:**

- `src/bin/loadtest.rs` — a configurable mixed get/put/delete load generator that reports periodic p50/p90/p99/p99.9/max
  latency per operation kind, specifically so pauses show up as a visible spike rather than getting smoothed into an
  average.
- `scripts/flamegraph.sh` — wraps `cargo flamegraph --profile profiling --bin loadtest` (the `profiling` Cargo profile
  keeps release optimizations but adds debug symbols so frames resolve to real function names).
- `cargo bench` — Criterion suites for `format`, `keydir`, `datafile`, and `merge`, each isolating one layer's cost.
  Note that `benches/merge.rs` fixes `max_file_size: 1` to control the live/dead entry ratio precisely, which as a
  side effect forces a rotation (and its fsync) on every entry — so it can't isolate the flush-batching behavior
  above; that's covered by unit tests and `loadtest` instead.

## Observability

The crate stays backend-agnostic by default — no metrics or logging dependency is forced on you — but exposes two
opt-in hooks, per [`docs/ROADMAP.md`](docs/ROADMAP.md)'s observability section:

- **Metrics**: implement the `Metrics` trait and pass it via `Options::metrics: Option<Arc<dyn Metrics>>`; every
  method has an empty default body, so implement only what you need. `record_put`/`record_get`/`record_delete`/
  `record_merge`/`record_sync` are given each call's duration. `record_pending_queue_depth` and
  `record_merge_summary` are gauges into what changes *during* a single merge pass rather than just how long it
  took: the depth merge's deferred keydir-repoint queue reaches before each batch drain (which doubles as that
  batch's size), and — once per completed pass — how many input files it compacted and how many live entries it
  copied forward. Leaving `metrics` `None` (the default) records nothing.
- **Structured logging and merge spans**: enable the `tracing` feature (`yabiir = { version = "...", features =
  ["tracing"] }`) to route the crate's warnings (truncated/corrupt entries found during recovery or merge) through
  `tracing::warn!` instead of `eprintln!`, and wrap merge's phases (scan, copy-forward, flush, remove old input
  files) in `tracing` spans. Without the feature, warnings still go to stderr via `eprintln!` — nothing is silently
  dropped either way.

[`examples/bsky_firehose.rs`](examples/bsky_firehose.rs) wires up a `Metrics` implementation and renders it as a live
`ratatui` dashboard alongside its own application-level stats — a working example of both hooks together.

## Known limitations

These are called out explicitly in the relevant module docs, not hidden:

- **Merge/recovery ordering gap** (`src/merge/mod.rs`): a key written *truly concurrently* with a merge that also
  touches that key can, in rare cases, be resolved incorrectly by a *subsequent* recovery/reopen — even though the
  live, in-memory keydir stays correct for the rest of that session. Closing this fully needs merge output to reuse
  freed file ids via a carefully-ordered rename step; not implemented.
- **No loom coverage** for `src/keydir.rs`'s concurrency, flagged as optional in the implementation plan and never
  picked up.
- **The single-writer lock is a best-effort `flock`**, not reliable on network filesystems (NFS in particular). Don't
  point this at an NFS mount and expect the lock to actually exclude a second writer.
- **`should_sync_on_put` defaults to `false`** — writes are flushed (visible to concurrent readers, survives a process
  crash) but not `fsync`'d by default, so a true power outage can lose the last few writes unless you opt into
  `should_sync_on_put: true`. This is a standard, documented tradeoff, not a bug.

## Testing

```sh
cargo test                                   # unit tests + tests/model.rs's proptest model test
PROPTEST_CASES=2000 cargo test --test model  # more property-based cases, for extra confidence
cargo bench                                  # Criterion suites — format, keydir, datafile, merge
```

The proptest model test runs random sequences of `put`/`delete`/`get`/`reopen`/`merge` against a plain `HashMap`
reference model over a small, deliberately-colliding key alphabet, exercising the same recovery/merge ordering edge
cases described in [Known limitations](#known-limitations).

CI (`.github/workflows/ci.yml`) runs `rustfmt --check`, `clippy -D warnings`, and the full test suite (including
doctests and an extra-cases proptest run) on every push and pull request, on Linux and macOS.

## Releasing

Publishing to crates.io (`.github/workflows/publish.yml`) is triggered by pushing a tag matching `v*.*.*`, whose
version must exactly match `Cargo.toml`'s. After bumping the version and committing that:

```sh
cargo xtask release
```

This is a small [xtask](https://github.com/matklad/cargo-xtask)-style dev-tooling crate (`xtask/`, a workspace
member, `publish = false` — it never ships with the published `yabiir` crate). It reads the version straight from
`Cargo.toml`, then runs `git tag v<version>` and `git push origin` that tag, which triggers the publish workflow. It
refuses to run with a dirty working tree or a tag that already exists, rather than tagging the wrong commit or
silently no-op-ing. `cargo xtask <anything else>` prints the available tasks.

Needs a `CARGO_REGISTRY_TOKEN` repo secret (Settings > Secrets and variables > Actions) — a crates.io API token
scoped to publish this crate, generated from crates.io's Account Settings > API Tokens.

## Design notes

The full implementation plan — every design decision and why, walked through milestone by milestone — is in [
`docs/bitcask-implementation-plan.md`](docs/bitcask-implementation-plan.md). Start there for the on-disk format, the
keydir, recovery, merge, and locking, in that order.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
