# Quality review — 2026-10-22

> **Superseded by the review of 2026-11-12, at the end of this file.** The observations below are kept as written;
> each is marked with what became of it.

A review of structure, formatting and testing ahead of the 0.6.0 work (ordered keydir) described in
email-mcp's design report ISSS-TR-0503. Observations only; nothing below has been changed yet.

## Status at review time

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` (`rustfmt.toml`: `chain_width = 0`) | Clean, no changes needed |
| `cargo clippy --workspace --all-targets` | Clean, no warnings |
| `cargo test --workspace` | 86 unit tests + the proptest model pass; 0 doctests |
| Line coverage (`cargo llvm-cov --workspace`) | **87.3 %** overall |
| CI (`.github/workflows/ci.yml`) | fmt, clippy (default and `tracing`), tests on Linux and macOS, proptest with 2000 cases |

Coverage by file:

| File | Lines covered | Note |
|---|---|---|
| `keydir.rs`, `lib.rs`, `metrics.rs`, `merge/pending_queue.rs`, `format/mod.rs` | 100 % | |
| `datafile.rs`, `engine.rs`, `merge/mod.rs`, `format/hint.rs`, `commit.rs` | 98.8–99.7 % | |
| `recovery.rs`, `format/entry.rs`, `error.rs` | 93.6–98.3 % | |
| `merge/output_writer.rs` | 91.6 % | error paths of the batched flush |
| `api.rs` | 29.6 % | `Options::read_only` and the hand-written `Debug` impl |
| `bin/kv.rs`, `bin/loadtest.rs`, `xtask` | 0 % | no tests at all |

The core engine is in very good shape; the gaps are at the edges.

## Observations

### Correctness

1. **`Engine::put` and `Engine::delete` don't validate sizes.** `format::encode_entry` documents that
   release builds silently truncate a key longer than `MAX_KEY_LEN` and says the engine layer must
   validate first, but `put`/`delete` (`src/engine.rs`) only reject empty keys. Values are cast with
   `value.len() as u32`, so a value of 4 GiB or more writes a header whose size field wraps: the
   entry is corrupt on disk and the keydir records the wrong size. Add `Error::KeyTooLarge` /
   `Error::ValueTooLarge`, check both before encoding, and test the boundaries. (Unlikely in practice,
   but it is silent data corruption.)
2. **Lock-poisoning policy is inconsistent.** `src/commit.rs` has two `.lock().unwrap(); // TODO`
   while every other lock uses `.expect("… poisoned; exiting")`. Pick one policy (panic with a clear
   message everywhere, or map poisoning to an `Error`) and remove the TODOs.

### Structure and API surface

3. **Internal modules are public.** `datafile`, `format` and `keydir` are `pub` only so the benches
   can reach them, which makes them part of the semver surface. Hide them behind `#[doc(hidden)]` or
   a non-default `bench-internals` feature before 1.0. The keydir rewrite in 0.6.0 is a natural moment.
4. **Stale and informal comments.** `src/lib.rs` refers to `src/bin/e.rs`, which doesn't exist (the
   binary is `kv`). The test in `lib.rs` is introduced with "This test is BS"; say what it guards
   against instead (a constant-returning mutant). "no loner" appears in 10 panic messages
   ("no longer").

### Testing

5. **No doctests.** CI runs `cargo test --doc`, but the public API has no examples. A short example
   on `Bitcask` (open, put, get, delete, close) and on `Options` would document usage and be
   compiled on every run.
6. **The `kv` CLI is untested.** A few `assert_cmd`-style integration tests (put/get/delete/list
   against a temp dir, read-only mode, the lock error) would cover the only user-facing binary.
   `loadtest` and `xtask` are developer tools and can stay untested.
7. **One integration test file.** `tests/model.rs` is excellent but covers single-threaded
   sequences only. When the ordered keydir lands, extend it with scans (as planned) and consider a
   second model run with concurrent writers against the same reference, to cover what the unit
   stress tests do today from the public API.
8. **`api.rs` edges.** `Options::read_only()` and the `Debug` impl are trivial but untested; two
   small tests close the file.

### Already good

- Formatting, lints and tests are enforced in CI on two operating systems, with a heavier proptest job.
- Coverage of the storage core (engine, recovery, merge, formats, group commit) is 98–100 %.
- The ROADMAP's §1 determinism concern is resolved as a side effect of the planned `BTreeMap` keydir.

## Suggested order

1. Size validation in `put`/`delete` with boundary tests (correctness first).
2. Lock-poisoning policy and comment/typo clean-up.
3. Doctests for `Bitcask` and `Options`, and `kv` CLI tests.
4. Hide internal modules as part of 0.6.0, together with the ordered keydir and scan model tests.

## What became of each observation (2026-11-12)

1. **Size validation.** Done: `Error::KeyTooLarge` and `Error::ValueTooLarge`, checked in `engine/write.rs` before
   encoding, with a boundary test.
2. **Lock-poisoning policy.** Done: every lock goes through `sync.rs`, which panics with one message.
3. **Internal modules are public.** Now `#[doc(hidden)]`, and documented as public only for the benches.
4. **Stale and informal comments.** Fixed, along with the others the later review found.
5. **No doctests.** One, on the crate root: open, put, get, delete, close.
6. **The `kv` CLI is untested.** Still true. `kv` is now its own crate (`yabiir-cli`), which is where its tests
   would go.
7. **One integration test file.** `tests/model.rs` covers scans now, and still runs one thread. The concurrency
   tests are unit tests with deterministic hooks (`engine/tests/races.rs`) plus stress tests.
8. **`api.rs` edges.** Still untested: `Options`' `Debug` impl.

# Review of 2026-11-12

A second review, of correctness first, done alongside a restructuring of the code. An independent reviewer read
the code as committed at `e78797e`; the findings below are that review's and the restructuring's together. All of
them were fixed in the same change, each with a test where a test could reach it.

## Bugs found and fixed

| # | What was wrong | How it showed | Fix | Test |
|---|---|---|---|---|
| 1 | Merge output got file ids *above* the active file's, and recovery replays files in id order. | A `put` or `delete` made while a merge ran was undone by the next reopen: the old value, or a deleted key, came back. | A merge closes the active file and reserves its output ids under the writer's lock, below every later file (`Writer::seal_for_merge`). | `race_put_during_merge_is_still_the_last_write_after_reopen`, `race_delete_during_merge_stays_deleted_after_reopen` |
| 2 | A merge that failed part-way left output files above the active file. | The next write, then a reopen, went back to the old value. No concurrency needed. | Same fix: the reserved ids are below any later file whether or not the merge finishes. | `a_write_after_a_failed_merge_is_still_the_last_write_after_reopen` |
| 3 | A failed append of a large entry left part of it in the file without advancing the tracked offset. | Later writes were published at the wrong position: `get` returned bytes of the wrong entry, silently. | An append error closes the active file; the next write starts a new one. | By reading only: needs a failing disk (ROADMAP §1). |
| 4 | Two merges could run at once, and one could take the other's unfinished output as input. | An error from one of them at best; at worst keys pointing into a removed file. | A merge lock in `Engine`. | `concurrent_merges_do_not_lose_data` |
| 5 | A failed fsync was retried by the next waiter. | The kernel reports a write-back error once, so the retry could report success for lost data. | A failed fsync fails every write not known durable, for good, and the file is closed. | `a_failed_fsync_fails_its_writes_for_good`, `a_failed_fsync_fails_the_waiting_followers` |
| 6 | No directory fsync. | A new file's name, or a removal, wasn't durable: a synced write could vanish with its file on power loss. | The directory is fsynced when a data file is created or removed. | By reading only. |
| 7 | Every read-write open created a data file. | `kv get` a thousand times left a thousand empty files, each holding a descriptor at the next open. | The active file is created by the first write; `kv` opens read-only to read. | `opening_without_writing_creates_no_data_files` |
| 8 | A torn or corrupt header could declare gigabytes, allocated before reading. | A huge allocation at open, for a file with a bad tail. | The body is read as it arrives, not pre-allocated. | `truncation_is_detected_at_every_boundary` covers the path. |
| 9 | An unreadable hint file failed the open. | A store that couldn't be opened because of a file that is only a shortcut. | Any fault in a hint falls back to scanning the data file. | Exercised by test 2's setup. |
| 10 | Merge ignored errors removing input files. | A newer file could be removed while an older one stayed, and a deleted key come back from it. | Oldest first, stopping at the first error. | By reading only. |

A second reviewer then read the rewritten code and found four defects *in the fixes*, also fixed:

| # | What was wrong | Fix | Test |
|---|---|---|---|
| 11 | If creating the active file half-failed (file made, directory fsync failed), its id was handed out again, and a merge could use it for both an input and an output file, then remove its own output: every key gone. | A file id is spent as soon as it is chosen, and a merge's first output id is above its newest input whatever the counter says. | `merge_output_never_shares_an_id_with_an_input_file` |
| 12 | A sync that succeeded while a leader's fsync was failing (a rotation, an explicit `sync`) marked the failed writes durable. | A failed fsync fails everything that wasn't durable when it *started*. | `a_sync_that_succeeds_during_a_failed_fsync_does_not_rescue_its_writes` |
| 13 | After a failed fsync, other writers could still append to the same file, behind bytes that might be lost, and a second fsync could acknowledge them. | The failing leader drops the file, unsynced, before group commit hears of the failure. | By reading only. |
| 14 | Without `sync_on_put`, a failed fsync at rotation was reported to nobody, and a later `sync()` returned `Ok`. | The failure is remembered and reported by the next `sync` or `close`. | By reading only. |

It also noted that a hint file could outlive its data file and later be taken for the hint of a new file with the
same id; a data file's creation now removes any hint under its id, and removal takes the hint first.

Smaller ones: every `flock` error was reported as `AlreadyLocked`; a read-only open created the directory; a panic
inside a leader's fsync left every later `sync_on_put` writer waiting; `Error::Corrupt` and `Error::NotImplemented`
were never constructed and are gone; `Error` is `#[non_exhaustive]`.

## Structure

- **Six crates instead of one.** The engine no longer depends on `clap`, and its dev-dependencies no longer include
  a terminal UI, an async runtime and a websocket client: `kv`, `loadtest`, the SQL front end and the large examples
  each have a crate. `yabiir-sql` uses only the engine's public API.
- **A `Writer`.** The active file, the file-id counter and group commit used to be three fields of `Engine`, passed
  one by one (with five more arguments) to `merge`. They are one type with one lock, and `merge` takes four
  arguments.
- **One scanner.** Recovery and merge each had a loop over `read_entry` with its own offset arithmetic; merge's read
  a whole file into memory first. Both use `datafile::scan`, an iterator over one buffered reader.
- **Modules by job.** `engine.rs` (1,700 lines, 1,200 of them tests) is `engine/{mod,read,write}.rs` and seven test
  files by subject; `datafile`, `keydir`, `recovery` and `merge` are split the same way. No file has more than 300
  lines of non-test code.
- **Five copies of `TempDir`** are one, in `yabiir-testutil`.

## Measured

`loadtest`, 4 threads, 80 % gets, a merge every 3 s, 8 s after warmup, same machine, before and after: gets and puts
a few percent faster at every percentile up to p99.9; a merge pass about 0.6–1.1 s where it took 2.1–2.6 s. The
unit tests take about 5 s where they took 1 s, and the model test 21 s where it took 4 s: both create thousands of
tiny files, and each creation now fsyncs the directory.

## Still open

- No test makes an append or an fsync fail (bugs 3, 6 and 10 are fixed by reasoning). ROADMAP §1 has the plan: a
  fault-injecting file layer under `datafile/`.
- The maximum (not p99.9) latency of a `get` under a concurrent merge looked higher after the change in some runs
  (24–30 ms against 12–13 ms) and not in others. Not investigated.
- The limitations listed in the README: visibility before durability with `sync_on_put`, read-only handles and
  merges, no CRC on reads, one descriptor per file, full-rewrite merges.
- A merge reserves one file id per `max_file_size` bytes of input, used or not. With the default 64 MiB that is
  nothing; with a `max_file_size` of a few bytes, as some tests use, a store of gigabytes would run out of ids.
- If an entry rots on disk after the keydir was built from a hint file, a merge drops the entry (its CRC fails) and
  removes the file, and `get` of that key returns an I/O error until the store is reopened.
- `kv` and `Options`' `Debug` impl remain untested.
