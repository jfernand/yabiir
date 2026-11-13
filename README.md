# yabiir

[![CI](https://github.com/jfernand/yabiir/actions/workflows/ci.yml/badge.svg)](https://github.com/jfernand/yabiir/actions/workflows/ci.yml)

Yet another Bitcask implementation, in Rust — a log-structured hash-table key/value store, built from scratch following
Sheehy & Smith's 2010 paper (included locally at [`refs/bitcask-intro.pdf`](refs/bitcask-intro.pdf)).

> **Status: not yet battle-tested.** Covered by tests (unit, integration, a property-based model test, and
> concurrency stress tests), but this hasn't run in production anywhere. See [Known limitations](#known-limitations)
> before trusting it with data you care about.

## What it does

- `put`/`get`/`delete` with append-only writes, O(1) reads (one keydir lookup + one positioned read)
- Crash recovery on `open` — full-scan or hint-file-based, whichever's available
- `merge` (compaction): reclaims space from superseded values and deleted keys, safe to run concurrently with live
  traffic
- A choice of keydir per open, `Options::key_order`: `KeyOrder::Hashed` (the default, fastest point lookups) or
  `KeyOrder::Sorted`, which adds `Engine::scan(start, end)` for ranges of keys in order and makes `fold`/`list_keys`
  return keys in order. Nothing about it is stored on disk, so a store opens either way. `benches/keydir_order.rs`
  measures what the order costs
- A process-level single-writer lock, so two writable (`is_read_write: true`) handles can't corrupt the same directory
- A small SQL front end over a store, in its own crate (`yabiir-sql`): `SELECT [DISTINCT] * | key | value [WHERE key =
  … | WHERE key BETWEEN … AND …]`, parsed, planned onto `get`/`scan`/`fold`, and executed
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

See [`examples/basic_usage.rs`](crates/yabiir/examples/basic_usage.rs) for the full API walked through end to end (`put`/`get`/
`delete`/`list_keys`/`fold`/`merge`/`close`/reopen), or run it directly:

```sh
cargo run --example basic_usage
```

The larger examples, which need a terminal UI or an async runtime, are in `crates/yabiir-examples`: `cargo run --example
todo_tui` and `cargo run --example bsky_firehose`.

The `kv` CLI covers the same operations from the shell:

```sh
cargo run --bin kv -- /tmp/my-data add name yabiir
cargo run --bin kv -- /tmp/my-data get name
cargo run --bin kv -- /tmp/my-data list
cargo run --bin kv -- /tmp/my-data merge
```

## The workspace

| Crate | What it is |
|---|---|
| [`crates/yabiir`](crates/yabiir) | The storage engine. The one published crate; depends on `crc32fast`, `fs2`, `ahash` and, optionally, `tracing`. |
| [`crates/yabiir-sql`](crates/yabiir-sql) | The SQL front end: parser, AST, planner, executor, over yabiir's public API. |
| [`crates/yabiir-cli`](crates/yabiir-cli) | `kv`, the command-line tool. |
| [`crates/yabiir-loadtest`](crates/yabiir-loadtest) | `loadtest`, the load generator. |
| [`crates/yabiir-examples`](crates/yabiir-examples) | Example programs with heavy dependencies. |
| [`crates/yabiir-testutil`](crates/yabiir-testutil) | A self-removing temporary directory, shared by tests and benches. |
| [`xtask`](xtask) | Dev tooling: `cargo xtask release`, `cargo xtask bench`. |

Inside the engine, bottom up: `format` (the bytes of an entry and of a hint record), `datafile` (the files: appending,
positioned reads, scanning), `keydir` (the in-memory index), `writer` (the single writer: append, publish, rotate),
`commit` (group commit), `recovery` (rebuilding the keydir at open), `merge` (compaction), and `engine`, which puts them
behind the `Bitcask` trait.

## Performance

The design goal is **predictable latency**, not just peak throughput — a storage engine that's fast on average but
occasionally stalls for tens of milliseconds is worse for most real workloads than one that's a bit slower but
consistent. A few design choices follow directly from that:

- `get`/`fold` never take the writer's lock — every write is handed to the OS before its location is published, so
  an independent read handle always sees the bytes the keydir points at.
- There is one writer, strictly: an append and the keydir update that publishes it are one step under one lock, so the
  keydir changes in the order of the log. Readers aren't behind that lock.
- The keydir's `HashMap` uses `ahash` rather than the default SipHash or `rustc-hash`/FxHash: `ahash` is fast without
  FxHash's weakness on sequential/structured keys (`key-0000000042`-style — auto-incrementing IDs, timestamps,
  zero-padded counters), which can collapse almost every key into one hash bucket and turn O(1) map operations into
  O(n).
- `merge`'s compaction pass batches its flushes (every 256 entries, or when an output file fills up) instead of
  flushing after every entry, with keydir repoints deferred to line up with each batch — so a long merge running
  concurrently with live traffic doesn't stall foreground `put`/`get`/`delete` calls behind its own I/O.
- With `should_sync_on_put: true`, concurrent writers share a single `fsync` (group commit,
  `crates/yabiir/src/commit.rs`) instead of each paying for their own — one caller becomes the leader and fsyncs
  without holding the write lock, everyone else just waits for that shared fsync to cover their write.

Full design writeups — including the profiling methodology and measured before/after numbers — live in
[`docs/merge-batched-flushing.pdf`](docs/merge-batched-flushing.pdf)
([`.typ`](docs/merge-batched-flushing.typ)) and [`docs/group-commit.pdf`](docs/group-commit.pdf)
([`.typ`](docs/group-commit.typ)).

**Tooling, if you want to reproduce or extend this:**

- `crates/yabiir-loadtest` (`cargo run --release --bin loadtest -- <dir>`) — a configurable mixed get/put/delete load generator that reports periodic p50/p90/p99/p99.9/max
  latency per operation kind, specifically so pauses show up as a visible spike rather than getting smoothed into an
  average.
- `scripts/flamegraph.sh` — wraps `cargo flamegraph --profile profiling --bin loadtest` (the `profiling` Cargo profile
  keeps release optimizations but adds debug symbols so frames resolve to real function names).
- `cargo bench` — Criterion suites for `format`, `keydir`, `datafile`, and `merge`, each isolating one layer's cost.
  Note that `benches/merge.rs` fixes `max_file_size: 1` to control the live/dead entry ratio precisely, which as a
  side effect puts every entry in a file of its own (with its fsync) — so it can't isolate the flush-batching
  behavior above; that's covered by unit tests and `loadtest` instead.
- `cargo xtask bench [REF…]` records the Criterion medians of the working tree, or of any tag or commit, in
  `bench-history/`; `cargo xtask bench-table` prints them side by side.

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

[`examples/bsky_firehose.rs`](crates/yabiir-examples/examples/bsky_firehose.rs) wires up a `Metrics` implementation and renders it as a live
`ratatui` dashboard alongside its own application-level stats — a working example of both hooks together.

## Durability and failures

- **Without `should_sync_on_put`** (the default), a write is handed to the OS before `put` returns: it survives a crash
  of the process, but a power failure can lose the last writes. With it, `put` returns once the entry is fsynced.
- **A closed file is durable.** A data file is fsynced when it fills up and is closed, and the directory is fsynced
  when a file is created or removed, so a file's name is as durable as its contents.
- **A failed write or fsync is not retried.** The kernel reports a failed write-back once; a second fsync can succeed
  although the data is gone. So a failed fsync fails every write that wasn't known durable, and the file is closed:
  the next write starts a new one. A write that returned an error may or may not have taken effect.
- **A write made during a merge wins**, in the session and after a reopen. Merge output takes file ids below those of
  everything written after the merge started, and recovery replays files in id order.
- **A crashed or failed merge loses nothing.** Its partial output holds copies of entries the input still has.

## Known limitations

- **With `should_sync_on_put`, a write is visible slightly before it is durable.** The keydir is updated when the entry
  is handed to the OS; the shared fsync comes after. Another thread can read a value that a power failure in that
  window would lose. The writer itself isn't told `Ok` until the fsync is done.
- **A read-only handle is a snapshot.** It sees the store as it was at open, and it holds no lock, so a merge in the
  writing process can remove files it still needs. Reopen it after a merge.
- **Reads don't verify CRCs.** An entry's CRC is checked when a file is scanned (recovery, merge), not on `get`.
- **Corruption in the middle of a file** is skipped entry by entry, using the lengths in the corrupt entry's header. If
  the header itself is damaged, the rest of that file may be unreadable.
- **`open` holds one file descriptor per data file.** A store with thousands of unmerged files needs a matching
  `ulimit -n`. Merge regularly.
- **Every merge rewrites the whole store.** There is no threshold of dead bytes below which a file is left alone.
- **The single-writer lock is a best-effort `flock`**, not reliable on network filesystems (NFS in particular). Don't
  point this at an NFS mount and expect the lock to actually exclude a second writer.
- **Unix only**: values are read with `pread`.
- **No loom coverage** of the concurrency: it is tested with deterministic hooks and stress tests instead.

## Testing

```sh
cargo test                                   # unit tests + tests/model.rs's proptest model test
PROPTEST_CASES=2000 cargo test --test model  # more property-based cases, for extra confidence
cargo bench                                  # Criterion suites — format, keydir, datafile, merge
```

The proptest model test runs random sequences of `put`/`delete`/`get`/`reopen`/`merge` against a plain map as the
reference model, over a small, deliberately-colliding key alphabet. The races (a merge between a lookup and its read,
a write during a merge, two writes to one key) are unit tests in `crates/yabiir/src/engine/tests/races.rs`: each uses
a hook to stop one thread at the exact point of interest, so none depends on timing.

CI (`.github/workflows/ci.yml`) runs `rustfmt --check`, `clippy -D warnings`, and the full test suite (including
doctests and an extra-cases proptest run) on every push and pull request, on Linux and macOS.

## Releasing

Publishing to crates.io (`.github/workflows/publish.yml`) is triggered by pushing a tag matching `v*.*.*`, whose
version must exactly match `crates/yabiir/Cargo.toml`'s. Only `yabiir` is published; the other crates are
`publish = false`. After bumping the version and committing that:

```sh
cargo xtask release
```

This is a small [xtask](https://github.com/matklad/cargo-xtask)-style dev-tooling crate (`xtask/`, a workspace
member, `publish = false` — it never ships with the published `yabiir` crate). It reads the version straight from
`crates/yabiir/Cargo.toml`, then runs `git tag v<version>` and `git push origin` that tag, which triggers the publish workflow. It
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
