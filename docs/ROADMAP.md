# Roadmap to 1.0.0

This is a working list of what stands between the current, tested-but-never-deployed state of yabiir and a 1.0 the
project would actually trust in production. None of it is scheduled; it's ordered roughly by how directly each area
protects the data itself, since that's the thing a key/value store can least afford to get wrong.

## 1. Correctness and data integrity

The property-based model test (`tests/model.rs`) and the concurrency stress tests already catch real bugs — the
merge/recovery ordering issue documented in `src/merge/mod.rs` was found exactly this way — but they only exercise
one process, on one machine, with no injected faults beyond what a proptest sequence of operations can express. The
next step up is deterministic simulation testing: running the engine under a harness like `madsim` or `turmoil` (or
similar deterministic-simulation frameworks) that can inject disk latency, reordering, and partial writes, then
replay any failure byte-for-byte to debug it. Getting there needs two things the codebase doesn't have yet. First,
the keydir's hasher would need to become deterministic for simulation runs. `ahash`'s default `RandomState` doesn't
just pick one seed per process — every `HashMap::default()` construction gets its own fresh, OS-randomized seed (see
`ahash::RandomState::new`'s own docs), which is exactly right for resisting adversarial keys in production but means
even two `Keydir`s in the *same* run, let alone two "identical" simulated runs, wouldn't see the same internal
hash-bucket layout — that defeats reproducible replay entirely. A `BTreeMap` (deterministic iteration order by
construction) or `ahash::RandomState::with_seeds` (a genuinely fixed seed), swapped in only for the simulation build,
would fix that without touching the production default. Second, the file system access in `datafile.rs`, `lock.rs`,
and `recovery.rs` talks directly to
`std::fs` and `std::os::unix::fs::FileExt` — a real simulation needs that behind a trait it can substitute a
fault-injecting implementation for, which is a genuine architectural change, not a config flag.

Beyond simulation, there's testing the real thing under real chaos. Jepsen-style testing — recording every operation
a client actually issued and whether it was acknowledged, then checking the state recovered after killing the
process mid-run is consistent with *some* valid prefix of those acknowledged operations — would verify the crash
recovery story empirically instead of by code review. Power failure testing is the sharper version of the same
question, aimed specifically at the `should_sync_on_put` guarantee: the reasoning for why a `put` that returns `Ok`
under `should_sync_on_put: true` is durable against a true power loss is laid out in `src/commit.rs` and the two
`.typ` design reports, but nothing today actually pulls power (or simulates it, e.g. with block-layer fault
injection) to confirm it. That reasoning has never been checked against reality.

Where to start:

- Make the keydir's hasher swappable first — a `#[cfg(feature = "dst")]` build using `BTreeMap` or a fixed-seed
  `RandomState` instead of the production default needs no file system work, so it's small enough to land on its own
  and start proving out the approach.
- Scope the file system abstraction down to `datafile.rs` first (the path everything else depends on), leaving
  `lock.rs` and `recovery.rs` for later rather than doing all three at once.
- Before committing to `madsim` or `turmoil`, check whether either actually fits a synchronous, thread-based engine
  like this one — both are built around async network services. `loom` (already a known gap for `src/keydir.rs`) or
  `shuttle` may be a closer match for exploring thread interleavings, and a hand-rolled fault-injecting file system
  driven by proptest-generated fault schedules may cover the disk side without a full simulator at all.
- For Jepsen-style testing, skip the Jepsen/Clojure stack — a purpose-built harness that drives the `kv` CLI (or a
  thin wrapper around `Engine`) from one process while a supervisor `kill -9`s it at random points, then checks
  recovered state against a logged history of acknowledged operations, answers the same question and could start as
  a new `xtask` subcommand or its own test crate.
- Power failure testing has no cheap starting point — the realistic first step is Linux block-device fault injection
  (`dm-flakey`, or ALICE-style crash-consistency tooling) in a disposable VM, not literally pulling power.

## 2. Resilience and fault tolerance

Crash recovery already exists and is tested for the ordinary case — kill the process, reopen, get back everything
that was durably written. What's still open is the known gap in `src/merge/mod.rs`: a key written *truly
concurrently* with a merge touching that same key can end up misordered on a *later* reopen, even though the
in-memory keydir stays correct for the rest of that session. Closing it needs merge output to reuse the file ids it
frees rather than always claiming new ones, via a rename step careful enough that a concurrent reader never observes
a renamed-but-not-yet-repointed file under stale coordinates. That's the concrete next milestone under this heading,
not a vague hardening pass.

The engine also has no backpressure today. `put` and `delete` always succeed immediately regardless of how far merge
has fallen behind — under sustained write load, rotated files just pile up, and merge's own pass duration grows with
them (already visible in the merge-batched-flushing report's numbers). Nothing currently slows writers down or
schedules merge more aggressively when that backlog crosses a reasonable size; a real system would need one or the
other before it could be trusted not to fill a disk under load it can't keep up with. Related to that is memory
behavior over a long-running process: the keydir allocates one `Box<[u8]>` per key with no pooling or compaction, so
a process that churns through many keys over weeks can fragment its heap in ways a short-lived test run never
surfaces. Protecting against that likely means an arena or slab allocator for key storage, or a periodic
rebuild-into-a-fresh-map pass, rather than leaving it to whatever the system allocator happens to do.

Where to start:

- The ordering gap has a clear, test-first path: write a regression test that reproduces it (a key written truly
  concurrently with a merge touching it, followed by a reopen, matching the scenario already described in
  `src/merge/mod.rs`'s module doc), confirm it fails today, then design the rename-based fix against that failing
  test — the same way every fix landed during this project's mutation-testing and group-commit work was verified.
- For backpressure, the cheap version: cap `put`/`delete` (block, or return an error) once the count of un-merged
  rotated files crosses a configurable threshold — a small, local change to `Engine::append`.
- The more thorough version: have `Engine` schedule its own background merge automatically past a low-water mark,
  instead of leaving callers (like `loadtest`'s `--merge-interval-secs`) to manage timing themselves — a bigger
  change than the cap above.
- For memory fragmentation, measure before fixing: instrument a soak run (see §5) with periodic heap-size sampling
  to confirm this is a real problem in practice before investing in an arena allocator or a rebuild pass for a risk
  that might turn out to be negligible at realistic key counts.

## 3. Performance

Everything tuned so far — dropping the active-file read lock, batching merge's flushes, sharing fsyncs across
concurrent writers with group commit — attacked contention on the two locks the engine currently has: one `Mutex`
around the active file, one `RwLock` around the whole keydir. Every write still serializes through that single
active-file mutex no matter which key it touches, and every keydir mutation takes the same global write lock no
matter which key either. Finer-grained concurrency control means sharding the keydir the way concurrent hash maps
usually do, and possibly moving away from one single active file as the only write target, rather than continuing to
optimize what happens inside that one lock.

The I/O path itself is still plain synchronous `pread`/`write` syscalls, one blocking call at a time. `io_uring`
would let writes and reads be submitted without blocking a thread per call, which matters most exactly where group
commit already showed the biggest win: many concurrent writers. `mmap`-backed reads would let the OS's page cache
serve reads directly instead of `DataFileSet::read_at` allocating and copying into a fresh `Vec<u8>` on every call —
real gains, but real complexity too, since mmap's failure modes (a `SIGBUS` on a truncated or corrupted backing file)
are a different, harder kind of thing to reason about than a `Result` from a syscall.

Tail latency is the thread connecting all of this, and it isn't finished: the merge-batched-flushing report already
notes that pass duration climbs over a sustained run, because more data accumulates between merges than the
current fixed interval clears. Splitting a merge pass into smaller, interruptible chunks, or scheduling it off
backlog size instead of a fixed timer, would target that directly — it's the same problem as the backpressure item
above, looked at from the latency side rather than the correctness side.

Where to start:

- Keydir sharding is the more contained of the two concurrency options: replace `SharedKeydir`'s single
  `RwLock<Keydir>` with a fixed number of `RwLock<Keydir>` shards partitioned by a cheap hash of the key (16 or 32
  shards is a reasonable default to start benchmarking from) — a well-understood pattern with existing prior art
  (`dashmap`) to compare against directly.
- Defer multiple active files until sharding alone is shown insufficient by an actual benchmark — it's a much larger,
  structural change that touches on-disk file-id assignment, merge's input-file selection, and recovery ordering, not
  something to assume is necessary up front.
- Don't adopt `io_uring` or `mmap` as the first move either: consistent with how every other performance fix here
  started (profile, then fix what profiling actually shows), run `loadtest`/flamegraph under enough concurrent I/O
  pressure to confirm syscall overhead is really the bottleneck first — `tokio-uring` or the `io-uring` crate for the
  write path, `memmap2` for reads, if it is.
- Try incremental, resumable merge (process a bounded number of input files or entries per call, resumable across
  calls) before either full backpressure or a scheduler rewrite — it's the smallest of the tail-latency options,
  contained entirely within `merge/mod.rs`.

## 4. Observability

There's currently no way to see any of this from outside the process. No counters for how many `put`/`get`/
`delete`/`merge` calls happened or how long they took, no visibility into keydir size, active file size, or how far
merge has fallen behind — everything this project knows about its own performance came from external tools
(`loadtest`, `cargo flamegraph`, Criterion) run by hand, not from the engine reporting on itself. A real metrics
story means instrumenting `Engine` with counters and histograms for the operations that matter, exposed through a
trait or callback so a caller can wire them into whatever backend they already use, rather than the crate picking
one and forcing that dependency on everyone.

Logging today is a handful of bare `eprintln!` calls in `recovery.rs` and `merge/mod.rs`, for truncated entries and
CRC mismatches — no levels, no structure, no way to redirect or suppress them, and (as the mutation-testing pass
over this codebase found) not even exercised by a single test. Routing those through `tracing` instead would fix
the structure problem and also open the door to the more useful piece: spans around merge's phases — scan,
copy-forward, flush, remove old files — so an operator can see which part of a slow merge pass was actually slow,
instead of only the total duration a load generator measured from outside.

Where to start:

- Logging is the smallest, lowest-risk piece and doesn't need to wait for the rest: replace the handful of
  `eprintln!` calls in `recovery.rs` and `merge/mod.rs` with `tracing::warn!` (or `log::warn!`), behind an optional
  feature so consumers who don't want the dependency aren't forced into it — a mechanical change that could land on
  its own.
- Add tracing spans around merge's phases as a natural follow-on once `tracing` is already a dependency for logging,
  not as a separate effort.
- For metrics, the bigger design question: give `Engine` a small `Metrics` trait it takes as an optional
  `Arc<dyn Metrics>` at `open()` time (defaulting to a no-op), keeping the crate itself backend-agnostic — matching
  how `format`/`datafile`/`keydir` are already `pub` "internal building blocks, not stable public API" rather than a
  fixed public surface. A `tracing`- or `metrics`-crate-backed adapter could ship as an example rather than a hard
  dependency.

## 5. Aging

Everything measured so far has run against synthetic, uniformly-random keys and values, generated by `loadtest`
over tens of seconds. That's already caught real problems — the FxHash rejection earlier in this project's history
was specifically about a key pattern (sequential, zero-padded IDs) that a uniform-random generator would never
produce — which is exactly the argument for going further: shadowing real production traffic, replaying a captured
or mirrored copy of actual reads and writes against the engine without serving results back to real users, would
exercise whatever key and value distribution real usage actually has instead of the one this project happened to
imagine.

Soaking is the same idea applied to time instead of traffic shape: running under load for days, not tens of
seconds, to catch what only shows up slowly — the keydir fragmentation risk mentioned above, merge backlog trending
upward faster than merge can clear it, and `DataFileSet`'s read-handle cache, which today never evicts anything and
is documented as relying on the assumption that "a real Bitcask directory has at most a few thousand files even at
scale, thanks to merge." That's a reasonable assumption. It has never actually been tested.

Where to start:

- Soaking is the one item on this entire roadmap that's actionable today, with no new infrastructure: `loadtest`
  already takes an arbitrary `--duration-secs`, so running it unattended for a day or more (as a scheduled CI job, or
  just left running) is possible right now.
- The only real gap is instrumentation: `loadtest`'s periodic reporting doesn't yet sample memory use, file
  descriptor count, or merge backlog size over that kind of run — add that to its existing reporting loop to actually
  see the slow-building problems this section cares about.
- Shadowing has to wait on a real deployment to shadow, which doesn't exist yet. The useful near-term step is
  narrower: teach `loadtest` to replay a captured operation sequence (from a file) instead of only generating
  synthetic uniform-random traffic, so the tooling is ready the moment a real trace exists rather than needing to be
  built from scratch then.
