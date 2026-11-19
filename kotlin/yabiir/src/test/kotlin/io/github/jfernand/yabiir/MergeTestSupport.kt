package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.Scan
import io.github.jfernand.yabiir.datafile.Scanned
import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.discover
import java.nio.file.Path
import java.util.Collections
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors

/**
 * Raw scan of every data file currently in [dir] for how many entries (live
 * or tombstone) exist anywhere for [key]: checks that merge removed
 * superseded and dead bytes from disk, not just that the keydir looks right.
 */
internal fun countEntriesForKey(dir: Path, key: ByteArray): Int {
    var count = 0
    for (fileId in discover(dir)) {
        Scan(dataPath(dir, fileId)).use { scan ->
            for (scanned in scan) {
                if (scanned is Scanned.Found) count += scanned.record.ops.count { it.key.contentEquals(key) }
            }
        }
    }
    return count
}

/** Run every task on a thread of its own, wait for all of them, and rethrow the first failure. */
internal fun runConcurrently(tasks: List<() -> Unit>) {
    val pool = Executors.newFixedThreadPool(tasks.size)
    try {
        val futures = tasks.map { task -> pool.submit { task() } }
        val failures = futures.mapNotNull { f -> runCatching { f.get() }.exceptionOrNull() }
        failures.firstOrNull()?.let { throw (it as? ExecutionException)?.cause ?: it }
    } finally {
        pool.shutdownNow()
    }
}

/** A [Metrics] recording every merge summary it is given, as (input files, live entries copied). */
internal class GaugeRecordingMetrics : Metrics {
    val pendingQueueDepths: MutableList<Int> = Collections.synchronizedList(ArrayList())

    override fun recordPendingQueueDepth(depth: Int) {
        pendingQueueDepths.add(depth)
    }

    val mergeSummaries: MutableList<Pair<Int, Int>> = Collections.synchronizedList(ArrayList())

    override fun recordMergeSummary(inputFiles: Int, entriesCopied: Int) {
        mergeSummaries.add(inputFiles to entriesCopied)
    }
}
