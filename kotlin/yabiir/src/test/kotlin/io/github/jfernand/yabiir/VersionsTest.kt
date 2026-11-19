package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.datafile.hintPath
import io.github.jfernand.yabiir.format.DATA_MAGIC
import io.github.jfernand.yabiir.format.ENTRY_HEADER_SIZE
import io.github.jfernand.yabiir.format.HINT_MAGIC
import io.github.jfernand.yabiir.format.crc32
import io.github.jfernand.yabiir.format.encodeEntry
import io.github.jfernand.yabiir.format.encodeHintV1
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.file.Files
import java.nio.file.Path

/**
 * A port of `crates/yabiir/src/engine/tests/versions.rs`. Its
 * `a_version_is_never_reused_after_a_merge_that_kept_nothing` is in
 * [MergeTest].
 */
class VersionsTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = bytesOf(s)

    @Test
    fun `every put gives its key a higher version`() {
        openEngine(dir).use { db ->
            db.put(b("a"), b("1"))
            val first = db.versionOf(b("a"))!!
            db.put(b("b"), b("1"))
            db.put(b("a"), b("2"))
            val second = db.versionOf(b("a"))!!
            assertTrue(second > first, "$first then $second")
            assertEquals(Versioned(b("2"), second), db.getVersioned(b("a")))
            assertNull(db.versionOf(b("missing")))
        }
    }

    @Test
    fun `versions survive a reopen from the data files and from hints`() {
        val before: List<Long?>
        openEngine(dir).use { db ->
            for ((k, v) in listOf("a" to "1", "b" to "2", "a" to "3")) db.put(b(k), b(v))
            before = listOf(db.versionOf(b("a")), db.versionOf(b("b")))
        }
        openEngine(dir).use { db ->
            assertEquals(before, listOf(db.versionOf(b("a")), db.versionOf(b("b"))))
            db.merge()
            assertEquals(before, listOf(db.versionOf(b("a")), db.versionOf(b("b"))))
        }
        // Now from the hint files merge wrote.
        openEngine(dir).use { db -> assertEquals(before, listOf(db.versionOf(b("a")), db.versionOf(b("b")))) }
    }

    /**
     * The version a deleted key had must not be handed out again, even once
     * merge has dropped the tombstone that was the only record of the delete
     * and the store has been reopened.
     */
    @Test
    fun `a version is never reused after a delete, a merge and a reopen`() {
        val old: Long
        openEngine(dir).use { db ->
            db.put(b("keep"), b("x"))
            db.put(b("k"), b("old"))
            old = db.versionOf(b("k"))!!
            db.delete(b("k"))
            db.merge()
        }
        openEngine(dir).use { db ->
            db.put(b("k"), b("new"))
            val new = db.versionOf(b("k"))!!
            // The delete took the version after `old`, so the new put is two past.
            assertTrue(new > old + 1, "old $old, new $new")
        }
    }

    /** Data and hint files as yabiir 0.8 wrote them: no header, entries instead of records. */
    private fun writeV1Store(dir: Path) {
        val file0 = ByteArrayOutputStream()
        for ((k, v, tombstone) in listOf(
            Triple("a", "a-old", false),
            Triple("b", "b-1", false),
            Triple("gone", "x", false),
            Triple("gone", "", true),
        )) {
            file0.write(encodeEntry(b(k), b(v), tombstone, 1_700_000_000))
        }
        Files.write(dataPath(dir, 0), file0.toByteArray())

        // File 1 as a 0.8 merge left it: a data file with a v1 hint beside it.
        Files.write(dataPath(dir, 1), encodeEntry(b("a"), b("a-new"), false, 1_700_000_001))
        val valuePos = (ENTRY_HEADER_SIZE + 1).toLong()
        val hint = encodeHintV1(b("a"), 1_700_000_001, 5, false, valuePos)
        val crc = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(crc32(hint)).array()
        Files.write(hintPath(dir, 1), hint + crc)
    }

    private fun assertV1Contents(db: Engine) {
        assertArrayEquals(b("a-new"), db.get(b("a")))
        assertArrayEquals(b("b-1"), db.get(b("b")))
        assertNull(db.get(b("gone")))
    }

    @Test
    fun `a store written in format 1 opens with every version at one`() {
        for (order in KeyOrder.entries) {
            val store = Files.createTempDirectory(dir, order.name)
            writeV1Store(store)
            openEngine(store, Options(keyOrder = order)).use { db ->
                assertV1Contents(db)
                assertEquals(1L, db.versionOf(b("a")), "$order")
                assertEquals(1L, db.versionOf(b("b")), "$order")

                db.put(b("b"), b("b-2"))
                assertTrue(db.versionOf(b("b"))!! > 1, "$order")
            }
        }
    }

    @Test
    fun `merge rewrites a format 1 store in format 2`() {
        writeV1Store(dir)
        openEngine(dir).use { db ->
            db.merge()
            assertV1Contents(db)
        }

        for (id in discover(dir)) {
            val data = Files.readAllBytes(dataPath(dir, id))
            assertArrayEquals(DATA_MAGIC, data.copyOf(8), "data file $id")
            val hint = Files.readAllBytes(hintPath(dir, id))
            assertArrayEquals(HINT_MAGIC, hint.copyOf(8), "hint file $id")
        }
        openEngine(dir).use { db ->
            assertV1Contents(db)
            assertEquals(1L, db.versionOf(b("a")))
        }
    }
}
