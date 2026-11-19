package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.format.MAX_KEY_LEN
import io.github.jfernand.yabiir.format.MAX_VALUE_LEN
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

/** A port of `crates/yabiir/src/engine/tests/basics.rs`, and of the test and doc example in `lib.rs`. */
class BasicsTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = bytesOf(s)

    @Test
    fun `put then get returns value`() {
        openEngine(dir).use { db ->
            db.put(b("k"), b("v"))
            assertArrayEquals(b("v"), db.get(b("k")))
        }
    }

    @Test
    fun `put twice returns second value only`() {
        openEngine(dir).use { db ->
            db.put(b("k"), b("v1"))
            db.put(b("k"), b("v2"))
            assertArrayEquals(b("v2"), db.get(b("k")))
        }
    }

    @Test
    fun `delete then get returns none`() {
        openEngine(dir).use { db ->
            db.put(b("k"), b("v"))
            db.delete(b("k"))
            assertNull(db.get(b("k")))
        }
    }

    @Test
    fun `delete missing key is a silent no-op`() {
        openEngine(dir).use { db ->
            db.delete(b("missing")) // must not throw
            assertNull(db.get(b("missing")))
        }
    }

    @Test
    fun `get missing key returns none`() {
        openEngine(dir).use { db -> assertNull(db.get(b("missing"))) }
    }

    @Test
    fun `various value sizes round trip`() {
        openEngine(dir).use { db ->
            for ((key, len) in listOf("empty" to 0, "one" to 1, "big" to 3 * 1024 * 1024)) {
                val value = ByteArray(len) { 0x5A }
                db.put(b(key), value)
                assertArrayEquals(value, db.get(b(key)), key)
            }
        }
    }

    @Test
    fun `many distinct keys round trip`() {
        openEngine(dir).use { db ->
            for (i in 0 until 10_000) db.put(b("key-$i"), b("value-$i"))
            for (i in 0 until 10_000) assertArrayEquals(b("value-$i"), db.get(b("key-$i")))
        }
    }

    @Test
    fun `put rejects empty key`() {
        openEngine(dir).use { db ->
            assertThrows<YabiirException.EmptyKey> { db.put(ByteArray(0), b("v")) }
        }
    }

    @Test
    fun `delete rejects empty key`() {
        openEngine(dir).use { db ->
            assertThrows<YabiirException.EmptyKey> { db.delete(ByteArray(0)) }
        }
    }

    @Test
    fun `put and delete are rejected on a read-only handle`() {
        openEngine(dir).use { db -> db.put(b("k"), b("v")) } // create the directory as a writer first
        Engine.open(dir, Options().readOnly()).use { ro ->
            assertThrows<YabiirException.ReadOnly> { ro.put(b("k2"), b("v2")) }
            assertThrows<YabiirException.ReadOnly> { ro.delete(b("k")) }
        }
    }

    @Test
    fun `list keys and fold reflect live keys only`() {
        openEngine(dir).use { db ->
            db.put(b("a"), b("1"))
            db.put(b("b"), b("2"))
            db.put(b("c"), b("3"))
            db.delete(b("b"))

            assertEquals(listOf("a", "c"), db.listKeys().map { it.utf8() }.sorted())

            val total = db.fold(0) { _, v, acc -> acc + v.size }
            assertEquals(2, total) // "1".length + "3".length
        }
    }

    @Test
    fun `rotation keeps every key readable across both active and rotated files`() {
        openEngine(dir, Options(maxFileSize = 300)).use { db ->
            for (i in 0 until 200) db.put(b("k$i"), b("value-$i"))

            // Must have actually rotated for this test to be exercising what it claims to.
            val fileCount = discover(dir).size
            assertTrue(fileCount > 1, "expected multiple data files, got $fileCount")

            for (i in 0 until 200) assertArrayEquals(b("value-$i"), db.get(b("k$i")))
        }
    }

    /**
     * The entry header has 31 bits for the key's length and 32 for the
     * value's. Rust checks the lengths alone (`check_key_len`,
     * `check_value_len`); the Kotlin checks are private to `Engine.kt`, and
     * no JVM array can be longer than 2^31 - 1 bytes, so the over-limit
     * cases can't be reached. What can be checked: the limits themselves,
     * that an empty key is refused, and that a one-byte key and an empty
     * value are accepted.
     */
    @Test
    fun `keys and values too large for the header are refused`() {
        assertEquals((1L shl 31) - 1, MAX_KEY_LEN)
        assertEquals((1L shl 32) - 1, MAX_VALUE_LEN)
        openEngine(dir).use { db ->
            assertThrows<YabiirException.EmptyKey> { db.put(ByteArray(0), b("v")) }
            db.put(b("k"), ByteArray(0))
            assertArrayEquals(ByteArray(0), db.get(b("k")))
        }
    }

    // ---- lib.rs ----------------------------------------------------------------

    @Test
    fun `now unix returns a plausible current timestamp`() {
        // Loosely bounded: catches a stub returning a constant without being
        // sensitive to test speed. 1_700_000_000 is 2023-11-14.
        val t = nowUnix()
        assertTrue(t > 1_700_000_000, "nowUnix() returned implausibly small $t")
    }

    /** The crate-level doc example in `lib.rs`, through the [Bitcask] interface. */
    @Test
    fun `the crate doc example works`() {
        val store: Bitcask = Engine.open(dir, Options())
        store.put(b("name"), b("yabiir"), nowUnix())
        assertArrayEquals(b("yabiir"), store.get(b("name")))
        store.delete(b("name"), nowUnix())
        assertNull(store.get(b("name")))
        store.close()
    }
}
