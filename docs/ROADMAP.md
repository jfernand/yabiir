# Roadmap to 1.0.0

This is a working list of what stands between the current, tested-but-never-deployed state of yabiir and a 1.0 the
project would actually trust in production. None of it is scheduled; it's ordered roughly by how directly each area
protects the data itself, since that's the thing a key/value store can least afford to get wrong.

## 1. Correctness and data integrity

The property-based model test (`tests/model.rs`), the concurrency stress tests and the hook-driven race tests
(`src/engine/tests/races.rs`) already catch real bugs — the merge/recovery ordering issue was found by the model test,
and its concurrent form by a race test — but they only exercise one process, on one machine, with almost no injected
faults: one test makes a merge fail part-way, and nothing makes an append or an fsync fail. The code paths for those
failures (`src/writer.rs` closes the active file and starts a new one; `src/commit.rs` fails every write a failed fsync
might have covered) are reasoned through and unit-tested at the group-commit level, not exercised end to end. The
next step up is deterministic simulation testing: running the engine under a harness like `madsim` or `turmoil` (or
similar deterministic-simulation frameworks) that can inject disk latency, reordering, and partial writes, then
replay any failure byte-for-byte to debug it. Getting there needs two things the codebase doesn't have yet. First,
the keydir's hasher would need to become deterministic for simulation runs. `ahash`'s default `RandomState` doesn't
just pick one seed per process — every `HashMap::default()` construction gets its own fresh, OS-randomized seed (see
`ahash::RandomState::new`'s own docs), which is exactly right for resisting adversarial keys in production but means
even two `Keydir`s in the *same* run, let alone two "identical" simulated runs, wouldn't see the same internal
hash-bucket layout — that defeats reproducible replay entirely. A `BTreeMap` (deterministic iteration order by
construction) or `ahash::RandomState::with_seeds` (a genuinely fixed seed), swapped in only for the simulation build,
would fix that without touching the production default. Second, the file system access in `datafile/`, `lock.rs`,
`recovery/` and `merge/` talks directly to
`std::fs` and `std::os::unix::fs::FileExt` — a real simulation needs that behind a trait it can substitute a
fault-injecting implementation for, which is a genuine architectural change, not a config flag.

Beyond simulation, there's testing the real thing under real chaos. Jepsen-style testing — recording every operation
a client actually issued and whether it was acknowledged, then checking the state recovered after killing the
process mid-run is consistent with *some* valid prefix of those acknowledged operations — would verify the crash
recovery story empirically instead of by code review. Power failure testing is the sharper version of the same
question, aimed specifically at the `should_sync_on_put` guarantee: the reasoning for why a `put` that returns `Ok`
under `should_sync_on_put: true` is durable against a true power loss is laid out in `src/commit.rs`,
`src/writer.rs` and the two `.typ` design reports, but nothing today actually pulls power (or simulates it, e.g. with block-layer fault
injection) to confirm it. That reasoning has never been checked against reality.

Where to start:

- The keydir's determinism is already available: a store opened with `KeyOrder::Sorted` keeps its keys in a
  `BTreeMap`, with no hasher at all. A simulation build can simply open that way.
- Scope the file system abstraction down to `datafile/` first (the path everything else depends on: `ActiveFile`,
  `DataFileSet` and the scanner are the only code that opens, reads or writes a data file), leaving `lock.rs` and the
  hint files for later rather than doing everything at once. The first payoff is small and concrete: tests that make
  an append or an fsync fail and check that the next write lands in a new file and nothing acknowledged is lost.
- Before committing to `madsim` or `turmoil`, check whether either actually fits a synchronous, thread-based engine
  like this one — both are built around async network services. `loom` (a known gap) or
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
that was durably written. The gap that used to head this section is closed: a key written while a merge ran could be
replayed *before* the merge's older copy of it on a later reopen, and come back with the old value. A merge now fixes
its scope in one step under the writer's lock (`Writer::seal_for_merge`): it closes the active file and reserves the
ids of its output files below the id of every file written afterwards, so id order is the order of writes again, also
when a merge fails or the process dies in the middle of one. What remains under this heading is narrower:

- A read-only handle in another process holds no lock and isn't told about merges, so a merge can remove files it
  still points into. Either document it as a snapshot that must be reopened (done) or give it a way to notice.
- Recovery skips a corrupt entry using the lengths in its header. If the header is what was damaged, the rest of the
  file is lost to the scan. A resynchronizing scan, or a per-file index like the hint files for every closed file,
  would bound the damage.
- `open` keeps one descriptor per data file. Thousands of unmerged files need a matching descriptor limit; an LRU of
  read handles would remove the coupling.

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

- For backpressure, the cheap version: cap `put`/`delete` (block, or return an error) once the count of un-merged
  files crosses a configurable threshold — a small, local change to `Writer::append`.
- The more thorough version: have `Engine` schedule its own background merge automatically past a low-water mark,
  instead of leaving callers (like `loadtest`'s `--merge-interval-secs`) to manage timing themselves — a bigger
  change than the cap above.
- For memory fragmentation, measure before fixing: instrument a soak run (see §5) with periodic heap-size sampling
  to confirm this is a real problem in practice before investing in an arena allocator or a rebuild pass for a risk
  that might turn out to be negligible at realistic key counts.

## 3. Performance

Bitcask's design fixes the shape of the write path: one writer process, one active file, every write an append, and
the keydir updated after each append. Everything tuned so far made that one path cheaper without changing its shape:
dropping the active-file read lock, batching merge's flushes, reading files through a buffer when scanning them,
and group commit, which shares fsyncs across concurrent writers. That is the rule for what comes next too. Sharding the keydir and writing to more than one
active file are out of scope: both would end the single total order of writes that recovery, merge and the keydir
all rely on, for throughput no benchmark has shown yabiir needs.

The I/O calls underneath that path are a different matter, because they can change without changing its shape.
The path is still plain synchronous `pread`/`write` syscalls, one blocking call at a time. `io_uring` would let writes
and reads be submitted without blocking a thread per call, which matters most exactly where group commit already
showed the biggest win: many concurrent writers. `mmap`-backed reads would let the OS's page cache serve reads directly
instead of `DataFileSet::read_at` allocating and copying into a fresh `Vec<u8>` on every call. Both can stay
contained: `io_uring` behind the active file's append and sync, and `mmap` behind `read_at`, on immutable closed files
only, where nothing truncates or rewrites the mapping. That still leaves mmap's harder failure mode (a `SIGBUS` on a
corrupted or vanished backing file, instead of a `Result`) and the larger-than-memory stalls described in
`docs/reading-databases.pdf` §24, so either one should sit behind a feature flag next to the plain path, not replace
it.

The single writer is strict, and has a home: `src/writer.rs`. A put or delete appends and updates the keydir in one
critical section under the writer's lock (`Writer::append` takes the publish as a closure), so the keydir changes in
log order, as the paper's keydir is "atomically updated with the location of the newest data". Rotation and the start
of a merge happen under the same lock. The `race_*_publish_in_log_order` tests pin the order down. One consequence:
with `sync_on_put`, the fsync still runs after the lock is released (group commit), so a write becomes visible to
readers just before it is durable and before its `put` returns. Making visibility wait for durability would need
writes to publish in order *after* their shared fsync, a queue this design doesn't have.

Tail latency is the thread connecting all of this, and it isn't finished: the merge-batched-flushing report already
notes that pass duration climbs over a sustained run, because more data accumulates between merges than the
current fixed interval clears. Splitting a merge pass into smaller, interruptible chunks, or scheduling it off
backlog size instead of a fixed timer, would target that directly — it's the same problem as the backpressure item
above, looked at from the latency side rather than the correctness side.

Where to start:

- The strict critical section costs nothing measurable: a 4-thread `loadtest` run (80 % reads, merge every 3 s) is
  slightly faster after the change than before it. Measure again at high writer counts with `sync_on_put`, where the
  lock is most contended. A rotation still fsyncs the file it closes while holding the lock; moving that fsync out
  from under it is the obvious next cut in write tail latency.
- Merge reads its input about three times faster than it did (a 256 KiB read buffer instead of two `read` calls per
  entry). It still copies every live entry on every pass, and still takes the keydir's lock once per entry to check
  liveness and once per entry to repoint; batching those is the next thing to profile.
- Don't adopt `io_uring` or `mmap` as the first move: consistent with how every other performance fix here started
  (profile, then fix what profiling actually shows), run `loadtest`/flamegraph under enough concurrent I/O pressure to
  confirm syscall overhead or the read copy is really the bottleneck first — the `io-uring` crate for the write path,
  `memmap2` for reads, if it is. Keep each behind a feature flag and the existing `DataFileSet`/`ActiveFile`
  boundaries (`datafile/pread.rs` is the whole read path; `ActiveFile::append` and `sync` the whole write path), so
  the plain `pread`/`write` path stays the default and the model test runs against both.
- Try incremental, resumable merge (process a bounded number of input files or entries per call, resumable across
  calls) before either full backpressure or a scheduler rewrite — it's the smallest of the tail-latency options,
  contained entirely within `merge/`.

## 4. Observability

The first two pieces are in place. `Options::metrics` takes an `Arc<dyn Metrics>` (`src/metrics.rs`): durations of
`put`/`get`/`delete`/`merge`/`sync`, the depth of merge's repoint queue at each flush, and a summary per merge pass
(files compacted, entries copied). The crate picks no backend; `examples/bsky_firehose.rs` in `yabiir-examples` shows
one wired to a live dashboard. With the `tracing` feature, the warnings recovery and merge print (corrupt or truncated
entries, unusable hint files) go through `tracing::warn!` instead of stderr, and merge's phases — seal, copy, flush,
remove old files — are spans.

What's missing is the state, as opposed to the events: nothing reports the keydir's size, the number of data files,
the bytes on disk against the bytes that are live, or how long the writer's lock was held. Those are what an operator
needs to decide when to merge, and what backpressure (§2) would be driven by.

Where to start:

- Add gauges to `Metrics`, or a `stats()` call on `Engine`: key count, data file count, total and live bytes. Live
  bytes need the keydir to keep a running sum per file, which is also what a merge that skips mostly-live files would
  need.
- Count errors, not just durations: a failed append or fsync, a corrupt entry skipped at recovery.
- Test the warnings. They are still not asserted on by any test.

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
upward faster than merge can clear it, and `DataFileSet`'s read-handle cache, which keeps one open descriptor per data
file until a merge removes the file, on the assumption that merge keeps the number of files in the low thousands.
That's a reasonable assumption. It has never actually been tested.

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
