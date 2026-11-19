package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.format.encodeEntry
import io.github.jfernand.yabiir.format.putU32
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.lang.management.ManagementFactory
import java.nio.file.Files
import java.nio.file.Path

/**
 * A port of `crates/yabiir/tests/corrupt_header.rs`: a data file ending in a
 * header that claims gigabytes. Opening the store must recover the entries
 * before it without allocating anything near the claimed size.
 *
 * The Rust test records the largest single allocation through a global
 * allocator. The JVM has no such hook, so this measures every byte the
 * opening thread allocates (an upper bound on the largest allocation)
 * through `com.sun.management.ThreadMXBean`, against the same 16 MiB bound.
 */
class CorruptHeaderTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    @Test
    fun `open recovers past a header claiming gigabytes without allocating them`() {
        val good = encodeEntry(b("key"), b("value"), false, 1)
        val torn = encodeEntry(b("next"), b("value"), false, 1)
        torn.putU32(8, 0x7FFF_FFFF) // key_size
        torn.putU32(12, -1) // value_size = u32::MAX
        Files.write(dataPath(dir, 1), good + torn)

        val threads = ManagementFactory.getThreadMXBean() as com.sun.management.ThreadMXBean
        val thread = Thread.currentThread().threadId()
        val before = threads.getThreadAllocatedBytes(thread)
        val db = Engine.open(dir, Options())
        val allocated = threads.getThreadAllocatedBytes(thread) - before

        db.use {
            assertArrayEquals(b("value"), it.get(b("key")))
            assertNull(it.get(b("next")))
        }
        assertTrue(
            allocated < 16L shl 20,
            "open allocated $allocated bytes for a ${good.size + torn.size}-byte file",
        )
    }
}
