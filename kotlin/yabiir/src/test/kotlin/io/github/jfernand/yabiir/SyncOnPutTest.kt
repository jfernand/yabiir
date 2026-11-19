package io.github.jfernand.yabiir

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

/** Ports of `crates/yabiir/src/engine/tests/sync_on_put.rs`. */
class SyncOnPutTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    @Test
    fun `sync on put round trips correctly`() {
        Engine.open(dir, Options(shouldSyncOnPut = true)).use { db ->
            db.put(b("k"), b("v1"))
            db.put(b("k"), b("v2"))
            db.delete(b("k"))
            db.put(b("k2"), b("v"))
            assertNull(db.get(b("k")))
            assertArrayEquals(b("v"), db.get(b("k2")))
        }
    }

    /**
     * Several `shouldSyncOnPut` writers that arrive concurrently share a small
     * number of real fsyncs, not one each. A warm-up write returns first, so
     * the batch's fsyncs are counted in isolation.
     */
    @Test
    fun `concurrent sync on put writers share fsyncs`() {
        Engine.open(dir, Options(shouldSyncOnPut = true)).use { db ->
            db.put(b("warmup"), b("v"))
            val before = db.fsyncCallCount

            val n = 32
            val threads = (0 until n).map { i -> Thread { db.put(b("k$i"), b("v")) }.also { it.start() } }
            threads.forEach { it.join() }

            val fsyncsForBatch = db.fsyncCallCount - before
            assertTrue(
                fsyncsForBatch in 1 until n,
                "expected the $n concurrent writers to share well under $n fsyncs, got $fsyncsForBatch",
            )
            for (i in 0 until n) assertArrayEquals(b("v"), db.get(b("k$i")))
        }
    }
}
