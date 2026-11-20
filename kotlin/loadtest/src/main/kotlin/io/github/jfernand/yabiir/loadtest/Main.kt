/**
 * A port of `crates/yabiir-loadtest`: a mixed get/put/delete load on a yabiir
 * store, from several threads, with an optional merge every few seconds. It
 * prints windowed latency percentiles per kind of operation, then a summary,
 * in the same format and from the same options as the Rust tool, so that
 * `scripts/loadtest-compare.py` can run the two on the same workloads.
 *
 * Samples are kept in primitive arrays, as the Rust tool keeps them in a
 * `Vec` of tuples: boxing each one would charge the JVM for garbage that is
 * the tool's, not the engine's.
 */
package io.github.jfernand.yabiir.loadtest

import io.github.jfernand.yabiir.Engine
import io.github.jfernand.yabiir.KeyOrder
import io.github.jfernand.yabiir.Options
import io.github.jfernand.yabiir.nowUnix
import java.io.BufferedWriter
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.io.path.deleteRecursively
import kotlin.io.path.exists
import kotlin.system.exitProcess

private class Args(
    val dir: Path,
    val durationSecs: Long = 20,
    val warmupSecs: Long = 0,
    val threads: Int = 4,
    val keys: Long = 10_000,
    val valueSize: Int = 128,
    val readWeight: Long = 8,
    val writeWeight: Long = 2,
    val deleteWeight: Long = 1,
    val maxFileSize: Long = 64L * 1024 * 1024,
    val syncOnPut: Boolean = false,
    val sortedKeydir: Boolean = false,
    val mergeIntervalSecs: Long? = null,
    val reportIntervalSecs: Long = 1,
    val keep: Boolean = false,
    val csv: Path? = null,
    val summaryJson: Path? = null,
)

private const val USAGE = """Usage: loadtest <DIR> [--duration-secs N] [--warmup-secs N] [--threads N] [--keys N]
    [--value-size N] [--read-weight N] [--write-weight N] [--delete-weight N] [--max-file-size N]
    [--sync-on-put] [--sorted-keydir] [--merge-interval-secs N] [--report-interval-secs N] [--keep]
    [--csv PATH] [--summary-json PATH]"""

private fun parseArgs(argv: Array<String>): Args {
    var dir: Path? = null
    val values = HashMap<String, String>()
    val flags = HashSet<String>()
    val boolFlags = setOf("--sync-on-put", "--sorted-keydir", "--keep")
    var i = 0
    while (i < argv.size) {
        val a = argv[i]
        when {
            a == "-h" || a == "--help" -> { println(USAGE); exitProcess(0) }
            a in boolFlags -> flags.add(a)
            a.startsWith("--") -> {
                val (name, inline) = if ('=' in a) a.substringBefore('=') to a.substringAfter('=') else a to null
                values[name] = inline ?: argv.getOrNull(++i) ?: usage("$name needs a value")
            }
            dir == null -> dir = Path.of(a)
            else -> usage("unexpected argument $a")
        }
        i++
    }
    val known = setOf(
        "--duration-secs", "--warmup-secs", "--threads", "--keys", "--value-size", "--read-weight",
        "--write-weight", "--delete-weight", "--max-file-size", "--merge-interval-secs",
        "--report-interval-secs", "--csv", "--summary-json",
    )
    values.keys.firstOrNull { it !in known }?.let { usage("unknown option $it") }
    fun long(name: String, default: Long) = values[name]?.toLongOrNull() ?: values[name]?.let { usage("$name: not a number") } ?: default
    val d = Args(dir ?: usage("missing <DIR>"))
    return Args(
        dir = d.dir,
        durationSecs = long("--duration-secs", d.durationSecs),
        warmupSecs = long("--warmup-secs", d.warmupSecs),
        threads = long("--threads", d.threads.toLong()).toInt(),
        keys = long("--keys", d.keys),
        valueSize = long("--value-size", d.valueSize.toLong()).toInt(),
        readWeight = long("--read-weight", d.readWeight),
        writeWeight = long("--write-weight", d.writeWeight),
        deleteWeight = long("--delete-weight", d.deleteWeight),
        maxFileSize = long("--max-file-size", d.maxFileSize),
        syncOnPut = "--sync-on-put" in flags,
        sortedKeydir = "--sorted-keydir" in flags,
        mergeIntervalSecs = values["--merge-interval-secs"]?.let { long("--merge-interval-secs", 0) },
        reportIntervalSecs = long("--report-interval-secs", d.reportIntervalSecs),
        keep = "--keep" in flags,
        csv = values["--csv"]?.let(Path::of),
        summaryJson = values["--summary-json"]?.let(Path::of),
    )
}

private fun usage(message: String): Nothing {
    System.err.println("error: $message\n$USAGE")
    exitProcess(2)
}

private enum class OpKind(val label: String) { GET("get"), PUT("put"), DELETE("delete"), MERGE("merge") }

private val ALL_KINDS = OpKind.entries

/** A growable array of longs. */
private class LongList(capacity: Int = 1024) {
    var items = LongArray(capacity)
    var size = 0

    fun add(v: Long) {
        if (size == items.size) items = items.copyOf(size * 2)
        items[size++] = v
    }

    fun sorted(): LongArray = items.copyOf(size).also { it.sort() }
}

/** Samples, column by column: when each operation started, its kind, and how long it took. */
private class Samples(capacity: Int) {
    var startedAt = DoubleArray(capacity)
    var kinds = ByteArray(capacity)
    var latencyNs = LongArray(capacity)
    var size = 0

    fun add(t: Double, kind: OpKind, ns: Long) {
        if (size == latencyNs.size) {
            val n = size * 2
            startedAt = startedAt.copyOf(n)
            kinds = kinds.copyOf(n)
            latencyNs = latencyNs.copyOf(n)
        }
        startedAt[size] = t
        kinds[size] = kind.ordinal.toByte()
        latencyNs[size] = ns
        size++
    }

    fun addAll(other: Samples) {
        for (i in 0 until other.size) add(other.startedAt[i], ALL_KINDS[other.kinds[i].toInt()], other.latencyNs[i])
    }

    fun clear() {
        size = 0
    }
}

/** xorshift64, as the Rust tool's `Rng`: the same keys and operations from the same seed. */
private class Rng(seed: Long) {
    private var x = if (seed == 0L) 1L else seed

    fun nextU64(): Long {
        x = x xor (x shl 13)
        x = x xor (x ushr 7)
        x = x xor (x shl 17)
        return x
    }
}

/** `key-` and `k` in 12 digits, as the Rust tool's `format!("key-{k:012}")`, without `String.format`'s cost. */
private fun keyBytes(k: Long): ByteArray {
    val out = byteArrayOf('k'.code.toByte(), 'e'.code.toByte(), 'y'.code.toByte(), '-'.code.toByte()) + ByteArray(12)
    var n = k
    for (i in out.size - 1 downTo 4) {
        out[i] = ('0'.code + (n % 10).toInt()).toByte()
        n /= 10
    }
    return out
}

private fun percentile(sortedNs: LongArray, p: Double): Long {
    if (sortedNs.isEmpty()) return 0
    val idx = Math.round((sortedNs.size - 1).toDouble() * p).toInt()
    return sortedNs[idx]
}

private fun fmtNs(ns: Long): String =
    when {
        ns >= 1_000_000_000 -> "%.2fs".format(ns / 1e9)
        ns >= 1_000_000 -> "%.2fms".format(ns / 1e6)
        ns >= 1_000 -> "%.2fus".format(ns / 1e3)
        else -> "${ns}ns"
    }

private fun sleepOrStop(durNs: Long, stop: AtomicBoolean): Boolean {
    val deadline = System.nanoTime() + durNs
    while (System.nanoTime() < deadline) {
        if (stop.get()) return true
        val left = (deadline - System.nanoTime()).coerceAtLeast(0)
        Thread.sleep(minOf(50L, left / 1_000_000).coerceAtLeast(1))
    }
    return stop.get()
}

/** What the worker, merge and reporting threads share during a run. */
private class Run(val db: Engine, recording: Boolean) {
    private val samples = Samples(1 shl 16)
    private val lock = Object()
    val stop = AtomicBoolean(false)
    val recording = AtomicBoolean(recording)
    val start = System.nanoTime()

    fun elapsedSecs(): Double = (System.nanoTime() - start) / 1e9

    /** Queue `batch`, emptying it. */
    fun record(batch: Samples) {
        synchronized(lock) { samples.addAll(batch) }
        batch.clear()
    }

    /** Take every queued sample. */
    fun drain(): Samples =
        synchronized(lock) {
            val window = Samples(maxOf(samples.size, 1))
            window.addAll(samples)
            samples.clear()
            window
        }

    fun startedAt(opStart: Long): Double = (opStart - start) / 1e9
}

private fun drainSamples(run: Run, all: Map<OpKind, LongList>, csv: BufferedWriter?): Samples {
    val window = run.drain()
    for (i in 0 until window.size) {
        val kind = ALL_KINDS[window.kinds[i].toInt()]
        all.getValue(kind).add(window.latencyNs[i])
        csv?.write("%.6f,%s,%d\n".format(window.startedAt[i], kind.label, window.latencyNs[i]))
    }
    return window
}

private fun printWindow(window: Samples, elapsedSecs: Double) {
    if (window.size == 0) return
    val byKind = ALL_KINDS.associateWith { LongList() }
    for (i in 0 until window.size) byKind.getValue(ALL_KINDS[window.kinds[i].toInt()]).add(window.latencyNs[i])
    for (kind in ALL_KINDS) {
        val list = byKind.getValue(kind)
        if (list.size == 0) continue
        val v = list.sorted()
        println(
            "%8.1fs %7s %8d %10s %10s %10s %10s %10s".format(
                elapsedSecs, kind.label, v.size, fmtNs(percentile(v, 0.50)), fmtNs(percentile(v, 0.90)),
                fmtNs(percentile(v, 0.99)), fmtNs(percentile(v, 0.999)), fmtNs(v.last()),
            ),
        )
    }
}

public fun main(argv: Array<String>) {
    val args = parseArgs(argv)
    require(args.readWeight + args.writeWeight + args.deleteWeight > 0) { "at least one op weight must be nonzero" }

    val db = openStore(args)
    val value = ByteArray(args.valueSize) { 0xAB.toByte() }
    prepopulate(db, args.keys, value)
    System.err.println(
        "done. running ${args.threads} threads for ${args.durationSecs}s (+${args.warmupSecs}s warmup), merge=" +
            (args.mergeIntervalSecs?.let { "every ${it}s" } ?: "disabled"),
    )

    val run = Run(db, recording = args.warmupSecs == 0L)
    val all = ALL_KINDS.associateWith { LongList(1 shl 16) }
    val csv = args.csv?.let { path ->
        Files.newBufferedWriter(path).also { it.write("elapsed_secs,op,latency_ns\n") }
    }

    val threads = ArrayList<Thread>()
    for (workerId in 0 until args.threads) {
        threads += Thread.ofPlatform().name("worker-$workerId").start { work(run, args, value, workerId.toLong()) }
    }
    args.mergeIntervalSecs?.let { secs ->
        threads += Thread.ofPlatform().name("merge").start { mergePeriodically(run, secs * 1_000_000_000) }
    }
    reportUntilDone(run, args, all, csv)
    run.stop.set(true)
    for (t in threads) t.join()

    // One more drain, now that every thread has returned (see the Rust tool).
    printWindow(drainSamples(run, all, csv), run.elapsedSecs())
    csv?.close()

    printSummary(all)
    args.summaryJson?.let { writeSummaryJson(it, args, all) }
    try {
        run.db.close()
    } catch (_: Exception) {
    }
}

@OptIn(kotlin.io.path.ExperimentalPathApi::class)
private fun openStore(args: Args): Engine {
    if (!args.keep && args.dir.exists()) args.dir.deleteRecursively()
    val options = Options(
        maxFileSize = args.maxFileSize,
        shouldSyncOnPut = args.syncOnPut,
        keyOrder = if (args.sortedKeydir) KeyOrder.SORTED else KeyOrder.HASHED,
    )
    return Engine.open(args.dir, options)
}

private fun prepopulate(db: Engine, keys: Long, value: ByteArray) {
    System.err.println("pre-populating $keys keys...")
    for (k in 0 until keys) db.put(keyBytes(k), value, nowUnix())
}

private fun work(run: Run, args: Args, value: ByteArray, workerId: Long) {
    val batchSize = 64
    val readW = args.readWeight
    val writeW = args.writeWeight
    val totalWeight = readW + writeW + args.deleteWeight
    val rng = Rng(0x9E3779B97F4A7C15uL.toLong() + workerId + 1)
    val batch = Samples(batchSize)

    while (!run.stop.get()) {
        val key = keyBytes(java.lang.Long.remainderUnsigned(rng.nextU64(), args.keys))
        val pick = java.lang.Long.remainderUnsigned(rng.nextU64(), totalWeight)

        val opStart = System.nanoTime()
        val kind =
            if (pick < readW) {
                run.db.get(key)
                OpKind.GET
            } else if (pick < readW + writeW) {
                run.db.put(key, value, nowUnix())
                OpKind.PUT
            } else {
                run.db.delete(key, nowUnix())
                OpKind.DELETE
            }
        val latency = System.nanoTime() - opStart

        if (run.recording.get()) {
            batch.add(run.startedAt(opStart), kind, latency)
            if (batch.size >= batchSize) run.record(batch)
        }
    }
    run.record(batch)
}

private fun mergePeriodically(run: Run, intervalNs: Long) {
    while (!sleepOrStop(intervalNs, run.stop)) {
        val mergeStart = System.nanoTime()
        run.db.merge()
        val latency = System.nanoTime() - mergeStart
        System.err.println("[merge] took ${fmtNs(latency)}")
        if (run.recording.get()) {
            run.record(Samples(1).also { it.add(run.startedAt(mergeStart), OpKind.MERGE, latency) })
        }
    }
}

private fun reportUntilDone(run: Run, args: Args, all: Map<OpKind, LongList>, csv: BufferedWriter?) {
    val warmupNs = args.warmupSecs * 1_000_000_000
    val totalNs = warmupNs + args.durationSecs * 1_000_000_000
    val intervalMs = maxOf(args.reportIntervalSecs, 1) * 1000
    println("%9s %7s %8s %10s %10s %10s %10s %10s".format("elapsed", "op", "count", "p50", "p90", "p99", "p99.9", "max"))

    while (System.nanoTime() - run.start < totalNs) {
        val remainingMs = (totalNs - (System.nanoTime() - run.start)) / 1_000_000
        Thread.sleep(minOf(intervalMs, maxOf(remainingMs, 1)))
        val isWarmupOver = System.nanoTime() - run.start >= warmupNs
        if (isWarmupOver && !run.recording.getAndSet(true)) System.err.println("--- warmup complete, recording ---")
        printWindow(drainSamples(run, all, csv), run.elapsedSecs())
    }
}

private fun printSummary(all: Map<OpKind, LongList>) {
    println("\n=== final summary (recorded, post-warmup samples only) ===")
    for (kind in ALL_KINDS) {
        val list = all.getValue(kind)
        if (list.size == 0) continue
        val v = list.sorted()
        println(
            "%-7s n=%-9d p50=%10s p90=%10s p99=%10s p99.9=%10s max=%10s".format(
                kind.label, v.size, fmtNs(percentile(v, 0.50)), fmtNs(percentile(v, 0.90)),
                fmtNs(percentile(v, 0.99)), fmtNs(percentile(v, 0.999)), fmtNs(v.last()),
            ),
        )
    }
}

/** The summary as JSON, in the Rust tool's `--summary-json` shape. */
private fun writeSummaryJson(path: Path, args: Args, all: Map<OpKind, LongList>) {
    val ops = ALL_KINDS.filter { all.getValue(it).size > 0 }.joinToString(",") { kind ->
        val v = all.getValue(kind).sorted()
        "\"${kind.label}\":{\"n\":${v.size},\"p50_ns\":${percentile(v, 0.50)},\"p90_ns\":${percentile(v, 0.90)}," +
            "\"p99_ns\":${percentile(v, 0.99)},\"p999_ns\":${percentile(v, 0.999)},\"max_ns\":${v.last()}}"
    }
    Files.writeString(path, "{\"engine\":\"kotlin\",\"duration_secs\":${args.durationSecs},\"ops\":{$ops}}\n")
}
