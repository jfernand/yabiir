package io.github.jfernand.yabiir

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path
import java.time.Duration
import java.util.concurrent.atomic.AtomicLong

/** Ports of `crates/yabiir/src/engine/tests/metrics.rs`. */
class MetricsTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    /** Counts how many times each callback fired: proves the engine calls into a caller-supplied [Metrics]. */
    private class CountingMetrics : Metrics {
        val puts = AtomicLong()
        val gets = AtomicLong()
        val deletes = AtomicLong()
        val merges = AtomicLong()
        val syncs = AtomicLong()

        override fun recordPut(elapsed: Duration) {
            puts.incrementAndGet()
        }

        override fun recordGet(elapsed: Duration) {
            gets.incrementAndGet()
        }

        override fun recordDelete(elapsed: Duration) {
            deletes.incrementAndGet()
        }

        override fun recordMerge(elapsed: Duration) {
            merges.incrementAndGet()
        }

        override fun recordSync(elapsed: Duration) {
            syncs.incrementAndGet()
        }
    }

    @Test
    fun `metrics are recorded for every op kind`() {
        val metrics = CountingMetrics()
        // Every put rotates: guarantees merge has something to do.
        Engine.open(dir, Options(maxFileSize = 1, metrics = metrics)).use { db ->
            db.put(b("a"), b("1"))
            db.put(b("b"), b("2"))
            assertArrayEquals(b("1"), db.get(b("a")))
            db.get(b("missing")) // a miss still counts as a get
            db.delete(b("a"))
            db.delete(b("missing")) // no-op delete still counts
            db.merge()
            db.sync()

            assertEquals(2, metrics.puts.get())
            assertEquals(2, metrics.gets.get())
            assertEquals(2, metrics.deletes.get())
            assertEquals(1, metrics.merges.get())
            assertEquals(1, metrics.syncs.get())
        }
    }

    /** With no `Options.metrics`, the engine falls back to a no-op and works normally. */
    @Test
    fun `engine works normally with no metrics configured`() {
        Engine.open(dir).use { db ->
            db.put(b("k"), b("v"))
            assertArrayEquals(b("v"), db.get(b("k")))
        }
    }
}
