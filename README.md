# yabiir

[![CI](https://github.com/jfernand/yabiir/actions/workflows/ci.yml/badge.svg)](https://github.com/jfernand/yabiir/actions/workflows/ci.yml)

Yet another Bitcask implementation, in Rust — a log-structured hash-table key/value store, built from scratch following
Sheehy & Smith's 2010 paper (included locally at [`refs/bitcask-intro.pdf`](refs/bitcask-intro.pdf)).

> **Status: not yet battle-tested.** Covered by tests (unit, integration, a property-based model test, and
> concurrency stress tests), but this hasn't run in production anywhere. See [Known limitations](#known-limitations)
> before trusting it with data you care about.

## What it does

- `put`/`get`/`delete` with append-only writes, O(1) reads (one keydir lookup + one positioned read)
- Atomic write batches with conditions: `Engine::write_batch(vec![Op::Check { .. }, Op::Put { .. }, ..])` writes every
  put and delete or none, after checking `Cond::Absent` / `Exists` / `Version(v)` against the store as it was just
  before the batch. A batch is one record on disk, so a crash leaves all of it or none. The returned `Commit` is a
  runtime-agnostic `Future` with a blocking `wait()`
- Versions: every write gets a sequence number, which becomes the version of each key it puts
  (`Engine::get_versioned`). A version is never reused, even after a delete, so `Cond::Version` is an exact
  compare-and-swap
- Crash recovery on `open` — full-scan or hint-file-based, whichever's available
- `merge` (compaction): reclaims space from superseded values and deleted keys, safe to run concurrently with live
  traffic
- A choice of keydir per open, `Options::key_order`: `KeyOrder::Hashed` (the default, fastest point lookups) or
  `KeyOrder::Sorted`, which adds `Engine::scan(start, end)` for ranges of keys in order, `Engine::scan_page` for a
  page of one either way (with a limit and versions), and makes `fold`/`list_keys` return keys in order. Nothing
  about it is stored on disk, so a store opens either way. `benches/keydir_order.rs` measures what the order costs
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

A write batch with a condition, here a compare-and-swap on a counter:

```rust
use yabiir::{Cond, Error, Op};

let current = db.get_versioned(b"hits")?.expect("set earlier");
let n: u64 = std::str::from_utf8(&current.value)?.parse()?;
let next = (n + 1).to_string();
match db
    .write_batch(vec![
        Op::Check { key: b"hits".to_vec(), cond: Cond::Version(current.version) },
        Op::Put { key: b"hits".to_vec(), value: next.into_bytes() },
    ])
    .wait() // or `.await`: a Commit is a Future too
{
    Ok(_) => {}                                  // written, all of it
    Err(Error::ConditionFailed { .. }) => {}     // someone wrote first: read again and retry
    Err(e) => return Err(e.into()),
}
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
| [`crates/yabiir-difftest`](crates/yabiir-difftest) | `yabiir-diffd`, which drives a store over stdin/stdout for differential tests. |
| [`xtask`](xtask) | Dev tooling: `cargo xtask release`, `bench`, `bench-table`, `campaign`. |

Beside the crates: [`book/`](book) is *Append Only*, a book in progress that builds a relational database on yabiir
one operator at a time (Typst; `book/tools/build.sh`), with its programs in `book/code/`.
[`kotlin/`](kotlin) is a port of the engine to Kotlin/JVM 21 that writes the same files, byte for byte, checked
against this one by differential tests (see [`kotlin/README.md`](kotlin/README.md)).
[`bench-history/`](bench-history) holds the recorded Criterion medians of every release, and `report.pdf`, their
charts with commentary. [`docs/ROADMAP.md`](docs/ROADMAP.md) is what comes next.

Inside the engine, bottom up: `format` (the bytes of a record and of a hint record, in both on-disk formats),
`datafile` (the files: appending, positioned reads, scanning), `keydir` (the in-memory index), `batch` (write batches,
conditions and `Commit`), `writer` (the single writer thread: check, append, sync, publish, rotate), `recovery`
(rebuilding the keydir at open), `merge` (compaction), and `engine`, which puts them behind the `Bitcask` trait.

## Performance

The design goal is **predictable latency**, not just peak throughput — a storage engine that's fast on average but
occasionally stalls for tens of milliseconds is worse for most real workloads than one that's a bit slower but
consistent. A few design choices follow directly from that:

- Reads never wait for the writer — every write is handed to the OS before its location is published, so an
  independent read handle always sees the bytes the keydir points at. Reads run on the caller's thread.
- There is one writer thread, and it alone appends: it takes every queued batch as a group, checks, appends, syncs and
  publishes them in log order, so the keydir changes in the order of the log. It spins briefly before sleeping, so a
  lone writer pays little for the hand-off (a `put` without fsync: 2.9 µs at the median against 1.8 µs in 0.8, in
  `loadtest` on one thread), and many writers gain (4 threads: more throughput than 0.8, and p99.9 65 µs instead of
  2.8 ms).
- The keydir's `HashMap` uses `ahash` rather than the default SipHash or `rustc-hash`/FxHash: `ahash` is fast without
  FxHash's weakness on sequential/structured keys (`key-0000000042`-style — auto-incrementing IDs, timestamps,
  zero-padded counters), which can collapse almost every key into one hash bucket and turn O(1) map operations into
  O(n).
- `merge`'s compaction pass batches its flushes (every 256 entries, or when an output file fills up) instead of
  flushing after every entry, with keydir repoints deferred to line up with each batch — so a long merge running
  concurrently with live traffic doesn't stall foreground `put`/`get`/`delete` calls behind its own I/O.
- With `should_sync_on_put: true`, concurrent writers share a single `fsync`: the writer thread syncs everything that
  queued while it was busy at once (`crates/yabiir/src/writer.rs`).

Full design writeups — including the profiling methodology and measured before/after numbers — live in
[`docs/merge-batched-flushing.pdf`](docs/merge-batched-flushing.pdf)
([`.typ`](docs/merge-batched-flushing.typ)) and [`docs/group-commit.pdf`](docs/group-commit.pdf)
([`.typ`](docs/group-commit.typ)). The second describes the group-commit protocol of 0.8 and earlier; 0.9's writer
thread replaced it, and gets the same sharing of fsyncs by writing every queued batch as one group.

**Tooling, if you want to reproduce or extend this:**

- `crates/yabiir-loadtest` (`cargo run --release --bin loadtest -- <dir>`) — a configurable mixed get/put/delete load generator that reports periodic p50/p90/p99/p99.9/max
  latency per operation kind, specifically so pauses show up as a visible spike rather than getting smoothed into an
  average.
- `scripts/flamegraph.sh` — wraps `cargo flamegraph --profile profiling --bin loadtest` (the `profiling` Cargo profile
  keeps release optimizations but adds debug symbols so frames resolve to real function names).
- `cargo bench` — Criterion suites for `format`, `keydir`, `keydir_order`, `datafile`, and `merge`, each isolating one
  layer's cost. Note that `benches/merge.rs` fixes `max_file_size: 1` to control the live/dead entry ratio precisely,
  which as a side effect puts every entry in a file of its own (with its fsync) — so it can't isolate the
  flush-batching behavior above; that's covered by unit tests and `loadtest` instead. Its `merge_noop` merges an empty
  store; `merge_active_only` merges 50 keys still in the active file, which since 0.7 is a real merge.
- `cargo xtask bench [REF…]` records the Criterion medians of the working tree, or of any tag or commit, in
  `bench-history/`; `cargo xtask bench-table` prints them side by side.
  It warns first about settings that make the clock drift (turbo boost, the frequency governor, an online hyperthread
  sibling of the pinned core) and records them with the numbers.
- `cargo xtask campaign new --cpu 6 [--rounds R] REF…` plans the same measurement for several versions at once,
  interleaved: each benchmark is measured on every version, in a shuffled order, before the next, so the clock's drift
  reaches every version alike. `cargo xtask campaign run 30m` measures for that long and stops between units (or, at
  the deadline, inside one, which is measured again later); further sessions carry on, and the last one writes
  `bench-history/`. `cargo xtask campaign status` shows what is left. The current history is one such campaign over
  v0.2.0 to v0.8.0; `typst compile bench-history/report.typ` turns it into `report.pdf`.

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
  of the process, but a power failure can lose the last writes. With it, `put` returns once the record is fsynced, and
  no reader sees a write before it is durable.
- **A batch is all or nothing.** Its puts and deletes are one record under one CRC; recovery applies a record whole or
  skips it, whether a crash cut it short or bit rot damaged it.
- **Stores written by 0.8 and earlier open as they are.** Their entries read as version 1, and the next merge rewrites
  them in the current format.
- **A closed file is durable.** A data file is fsynced when it fills up and is closed, and the directory is fsynced
  when a file is created or removed, so a file's name is as durable as its contents.
- **A failed write or fsync is not retried.** The kernel reports a failed write-back once; a second fsync can succeed
  although the data is gone. So a failed fsync fails every write that wasn't known durable, and the file is closed:
  the next write starts a new one. A write that returned an error may or may not have taken effect.
- **A write made during a merge wins**, in the session and after a reopen. Merge output takes file ids below those of
  everything written after the merge started, and recovery replays files in id order.
- **A crashed or failed merge loses nothing.** Its partial output holds copies of entries the input still has.

## Known limitations

- **A batch that fails to write may still be on disk.** If the `write` or fsync of a group fails, its batches get an
  error and are not published, but their records may survive a crash and reappear at the next open.
- **A read-only handle is a snapshot.** It sees the store as it was at open, and it holds no lock, so a merge in the
  writing process can remove files it still needs. Reopen it after a merge.
- **Reads don't verify CRCs.** A record's CRC is checked when a file is scanned (recovery, merge), not on `get`.
- **Corruption in the middle of a file** is skipped record by record, using the length in the corrupt record's header;
  a damaged batch is skipped whole. If the length itself is damaged, the rest of that file may be unreadable.
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
cargo bench                                  # Criterion suites — format, keydir, keydir_order, datafile, merge
kotlin/gradlew -p kotlin check               # the Kotlin port, and its differential tests against this engine
```

The proptest model test runs random sequences of `put`/`delete`/`get`/`reopen`/`merge` and write batches with
conditions against a plain map as the reference model, over a small, deliberately-colliding key alphabet, and checks
values, versions, and paged scans both ways. The races (a merge between a lookup and its read, a write during a
merge, two writes to one key) are unit tests in `crates/yabiir/src/engine/tests/races.rs`: each uses a hook to stop
one thread at the exact point of interest, so none depends on timing. `engine/tests/batches.rs` cuts a batch's record
at every byte, as a crash would, and checks that recovery keeps all of it or none.

CI (`.github/workflows/ci.yml`) runs `rustfmt --check`, `clippy -D warnings`, and the full test suite (including
doctests and an extra-cases proptest run) on every push and pull request, on Linux and macOS.

## Releasing

Publishing to crates.io (`.github/workflows/publish.yml`) is triggered by pushing a tag matching `v*.*.*`, whose
version must exactly match `crates/yabiir/Cargo.toml`'s. Only `yabiir` is published; the other crates are
`publish = false`. Bump the version in `crates/yabiir/Cargo.toml` and in the root `Cargo.toml`'s workspace
dependency, refresh `Cargo.lock` and the book programs' lockfiles (`book/code/ch*/Cargo.lock`, by building them),
commit that, push, and once CI is green:

```sh
cargo xtask release
```

This is a small [xtask](https://github.com/matklad/cargo-xtask)-style dev-tooling crate (`xtask/`, a workspace
member, `publish = false` — it never ships with the published `yabiir` crate). It reads the version straight from
`crates/yabiir/Cargo.toml`, then runs `git tag v<version>` and `git push origin` that tag, which triggers the publish workflow. It
refuses to run with a dirty working tree or a tag that already exists, rather than tagging the wrong commit or
silently no-op-ing. `cargo xtask <anything else>` prints the available tasks.

Needs a `CARGO_REGISTRY_TOKEN` repo secret (Settings > Secrets and variables > Actions) — a crates.io API token
scoped to publish this crate (`publish-update`), generated from crates.io's Account Settings > API Tokens. Tokens
expire: a publish that fails with `403 Forbidden: authentication failed` needs a new one under exactly that name, and
then a rerun of the failed job (`gh run rerun <id>`); the tag stays as it is.

## Design notes

The full implementation plan — every design decision and why, walked through milestone by milestone — is in [
`docs/bitcask-implementation-plan.md`](docs/bitcask-implementation-plan.md). Start there for the on-disk format, the
keydir, recovery, merge, and locking, in that order. The plan describes format version 1; version 2 (0.9: a file
header, records holding one or more operations under one CRC, and a sequence number per write) is documented in
`crates/yabiir/src/format/mod.rs`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
