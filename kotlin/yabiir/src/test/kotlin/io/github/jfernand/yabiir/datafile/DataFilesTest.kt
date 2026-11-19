package io.github.jfernand.yabiir.datafile

import io.github.jfernand.yabiir.format.encodeEntry
import io.github.jfernand.yabiir.format.putU32
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.Locale
import java.util.concurrent.atomic.AtomicReference

/** Ports of `crates/yabiir/src/datafile/tests.rs`. */
class DataFilesTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    @Test
    fun `append then read back matches`() {
        val active = ActiveFile.create(dir, 1, 1)
        val cases = listOf(b("a") to b("1"), b("bb") to b("22"), b("ccc") to b(""))
        val locations = ArrayList<Triple<Long, Long, Long>>()
        for ((key, value) in cases) {
            val valuePos = active.appendValue(oneOp(key, value, false, 0))
            locations.add(Triple(active.fileId, valuePos, value.size.toLong()))
        }
        active.sync()
        active.close() // now immutable

        DataFileSet(dir).use { files ->
            for ((location, case) in locations.zip(cases)) {
                val (fileId, valuePos, valueLen) = location
                assertArrayEquals(case.second, files.readAt(fileId, valuePos, valueLen))
            }
        }
    }

    @Test
    fun `manual rotation produces files with increasing ids and expected sizes`() {
        val threshold = 200L
        var fileId = 1L
        var active = ActiveFile.create(dir, fileId, 1)
        var rotations = 0
        for (i in 0 until 50) {
            active.appendValue(oneOp(b("key-$i"), ByteArray(20) { 'x'.code.toByte() }, false, 0))
            if (active.len >= threshold) {
                active.sync()
                active.close()
                fileId += 1
                active = ActiveFile.create(dir, fileId, 1)
                rotations += 1
            }
        }
        active.sync()
        active.close()

        assertTrue(rotations >= 3, "expected several rotations, got $rotations")

        val ids = discover(dir)
        assertEquals((1L..fileId).toList(), ids)

        // every file except the last (still being written when the loop
        // ended) should be at or above the rotation threshold.
        for (id in ids.dropLast(1)) {
            val size = Files.size(dataPath(dir, id))
            assertTrue(size >= threshold, "file $id is $size bytes, expected >= $threshold")
        }
    }

    @Test
    fun `concurrent reads on same file id are correct`() {
        val active = ActiveFile.create(dir, 1, 1)
        val locations = ArrayList<Triple<Long, Long, ByteArray>>()
        for (i in 0 until 64) {
            val value = ByteArray(37) { (i % 256).toByte() } // distinct-ish content per index
            val valuePos = active.appendValue(oneOp(b("k$i"), value, false, 0))
            locations.add(Triple(active.fileId, valuePos, value))
        }
        active.sync()
        active.close()

        DataFileSet(dir).use { files ->
            val failure = AtomicReference<Throwable?>()
            val threads =
                (0 until 8).map { t ->
                    Thread {
                        try {
                            for ((idx, location) in locations.withIndex()) {
                                if (idx % 8 != t) continue // spread work across threads, all hitting the same file id
                                val (fileId, valuePos, expected) = location
                                val read = files.readAt(fileId, valuePos, expected.size.toLong())
                                assertArrayEquals(expected, read, "thread $t idx $idx")
                            }
                        } catch (e: Throwable) {
                            failure.compareAndSet(null, e)
                        }
                    }.also { it.start() }
                }
            threads.forEach { it.join() }
            failure.get()?.let { throw it }
        }
    }

    @Test
    fun `is empty reflects whether anything has been written`() {
        ActiveFile.create(dir, 1, 1).use { active ->
            assertTrue(active.isEmpty)
            active.appendValue(oneOp(b("k"), b("v"), false, 0))
            assertFalse(active.isEmpty)
        }
    }

    /**
     * The Kotlin [ActiveFile] has no `read_at` of its own: the engine reads
     * the still-active file through the handle [DataFileSet.createActive]
     * caches, so this reads the bytes just written through that.
     */
    @Test
    fun `active file read at returns the bytes just written`() {
        DataFileSet(dir).use { files ->
            files.createActive(1, 1).use { active ->
                val valuePos = active.appendValue(oneOp(b("k"), b("hello"), false, 0))
                assertArrayEquals(b("hello"), preadExact(files.handle(active.fileId), valuePos, 5))
            }
        }
    }

    @Test
    fun `flush makes buffered bytes visible on disk`() {
        ActiveFile.create(dir, 1, 1).use { active ->
            active.appendBuffered(oneOp(b("k"), b("v"), false, 0).bytes) // deliberately not flushed
            val path = dataPath(dir, 1)
            val before = Files.size(path)
            assertTrue(before < active.len, "bytes should still be buffered, not yet on disk")

            active.flush()

            assertEquals(active.len, Files.size(path), "flush should make buffered bytes visible on disk")
        }
    }

    /**
     * The rule [DataFileSet.handle] relies on: a file created through
     * `createActive`, or registered at open, has its handle cached before
     * anything points into it, so it stays readable through `handle` after
     * the file is unlinked — no `open` by path is ever needed.
     */
    @Test
    fun `create active and register cache handles that outlive unlink`() {
        val encoded = oneOp(b("k"), b("v"), false, 0)

        // A file that already exists when the set is created (recovery's case).
        val existing = ActiveFile.create(dir, 1, 1)
        val existingPos = existing.appendValue(encoded)
        existing.close()

        DataFileSet(dir).use { files ->
            files.register(listOf(1L))

            // A file created through the set (rotation's and merge output's case).
            files.createActive(2, 1).use { created ->
                val createdPos = created.appendValue(encoded)

                for ((fileId, pos) in listOf(1L to existingPos, 2L to createdPos)) {
                    Files.delete(dataPath(dir, fileId))
                    val file = files.handle(fileId)
                    assertArrayEquals(b("v"), preadExact(file, pos, 1))
                }
            }
        }
    }

    @Test
    fun `forget evicts the cached read handle`() {
        val active = ActiveFile.create(dir, 1, 1)
        val valuePos = active.appendValue(oneOp(b("k"), b("v"), false, 0))
        val fileId = active.fileId
        active.sync()
        active.close()

        DataFileSet(dir).use { files ->
            // Populate the handle cache.
            assertArrayEquals(b("v"), files.readAt(fileId, valuePos, 1))

            Files.delete(dataPath(dir, fileId))
            // Unix semantics: the cached handle stays valid even after the
            // directory entry is unlinked, so without forgetting it, reads
            // still (silently) succeed against the deleted file's old content.
            assertArrayEquals(b("v"), files.readAt(fileId, valuePos, 1))

            files.forget(fileId)
            // Forgetting evicts the cached handle, so the next read has to
            // open the file afresh, which now fails, since the file is gone.
            assertThrows(IOException::class.java) { files.readAt(fileId, valuePos, 1) }
        }
    }

    @Test
    fun `parse data file id requires both correct length and all digits`() {
        assertEquals(42L, parseDataFileId(String.format(Locale.ROOT, "%020d.bitcask.data", 42)))
        // Right suffix, all-digit stem, but wrong length — must still be
        // rejected (a real Bitcask file is always exactly 20 digits).
        assertNull(parseDataFileId("42.bitcask.data"))
        // Exactly 20 characters, but not all digits.
        assertNull(parseDataFileId(String.format(Locale.ROOT, "%019dx.bitcask.data", 42)))
        assertNull(parseDataFileId("not-a-data-file.txt"))
    }

    @Test
    fun `a scan stops at a header claiming more than the file holds`() {
        val good = encodeEntry(b("key"), b("value"), false, 0)
        val torn = encodeEntry(b("next"), b("value"), false, 0)
        torn.putU32(12, -1) // value_size = u32::MAX
        val path = dataPath(dir, 1)
        Files.write(path, good + torn)

        val scanned = Scan(path).use { it.asSequence().toList() }

        assertEquals(2, scanned.size, "got $scanned")
        val first = scanned[0]
        assertTrue(
            first is Scanned.Found && first.offset == 0L && first.record.ops[0].key.contentEquals(b("key")),
            "got $first",
        )
        val second = scanned[1]
        assertTrue(second is Scanned.Truncated && second.offset == good.size.toLong(), "got $second")
    }
}
