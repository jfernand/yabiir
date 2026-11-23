# yabiir for Kotlin/JVM

A port of the yabiir 0.9 engine (`../crates/yabiir`) to Kotlin on JVM 21. It reads
and writes the same files, byte for byte: a store written by one opens in the other.

```kotlin
import io.github.jfernand.yabiir.*

Engine.open(Path.of("store"), Options(keyOrder = KeyOrder.SORTED)).use { db ->
    db.put("hits".toByteArray(), "0".toByteArray())
    val current = db.getVersioned("hits".toByteArray())!!
    val next = (String(current.value).toLong() + 1).toString().toByteArray()
    try {
        db.writeBatch(listOf(
            Op.Check("hits".toByteArray(), Cond.Version(current.version)),
            Op.Put("hits".toByteArray(), next),
        )).join()                       // or db.writeBatch(...).future, a CompletableFuture
    } catch (e: YabiirException.ConditionFailed) {
        // another writer got there first: read again and retry
    }
}
```

| Rust | Kotlin |
|---|---|
| `format/` | `format/Format.kt` |
| `datafile/` | `datafile/DataFiles.kt` |
| `keydir/` | `keydir/Keydir.kt` |
| `batch.rs` | `Batch.kt` (`Commit.join()` is Rust's `wait()`) |
| `writer.rs` | `Writer.kt` |
| `recovery/` | `Recovery.kt` |
| `merge/` | `Merge.kt` |
| `lock.rs` | `Lock.kt` |
| `engine/`, `api.rs`, `error.rs` | `Engine.kt`, `Api.kt`, `Errors.kt` |

The unit tests in `yabiir/src/test` port the Rust crate's tests file by file, as
`<RustFile>Test.kt`. The proptest models (`crates/yabiir/tests/model.rs`) became jqwik
properties in `ModelTest`.

Differences from the Rust crate:
- Errors are exceptions: `YabiirException` has one subclass per Rust `Error` variant.
- Timestamps are `Long`s that must fit in 32 unsigned bits.
- The directory lock is a POSIX record lock, which doesn't exclude a Rust process
  holding the `flock`. Don't open one store from both at once.

## Golden files

`yabiir/src/test/resources/golden/` holds stores the Rust engine wrote: a format-1
store, a format-2 store with merge output and hint files, and a mix of the two.
`GoldenTest` checks two things:
- the Kotlin engine reads from them what Rust read;
- every data and hint file decodes and encodes back to the same bytes.

Each store is made from the trace next to it (`<name>.jsonl`). After a format
change, rerun `regenerate.sh` and commit what it writes.

## Load test

`loadtest/` ports `crates/yabiir-loadtest`, with the same options and output.
`../scripts/loadtest-compare.py` builds both, runs them on the same workloads in
alternation, and compares throughput and latency:

```sh
scripts/loadtest-compare.py --rounds 3 --duration 15 --cpus 2-5   # from the repository root
```

## Differential tests

`difftest/` runs the Kotlin engine in process and the Rust engine in a
`yabiir-diffd` process (`../crates/yabiir-difftest`), sends both the same
commands, and checks after every command that:
- they replied the same, errors and versions included;
- their directories hold the same files, byte for byte.

jqwik generates the runs:
- puts, deletes, gets and batches with checks
- scans and pages
- merges, syncs, and reopens in another key order or read-only
- crashes that cut a data file, flipped bits, and *swaps*, where each engine reopens
  the other's directory
- stores seeded with format-1 files

A model of the store checks the values and that versions only grow.

```sh
./gradlew check                                  # from kotlin/: unit tests + 200 random runs
./gradlew :yabiir:test -Pmodel.tries=2000        # more model cases (Rust's PROPTEST_CASES)
./gradlew :difftest:soak -Pdifftest.tries=20000  # a long run
```

The Gradle build compiles `yabiir-diffd` with cargo, or uses `$YABIIR_DIFFD` if
it is set.

- **A failing run** is shrunk, and its trace is written to
  `difftest/build/failures/shortest.jsonl`.
- **The corpus.** Add a fixed failure's trace to
  `difftest/src/test/resources/corpus/`; every trace there is replayed on every
  build.
