# Quality review — 2026-10-22

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
