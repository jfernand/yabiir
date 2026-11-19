package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.ActiveFile
import io.github.jfernand.yabiir.datafile.DataFileSet
import io.github.jfernand.yabiir.datafile.appendValue
import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.datafile.hintPath
import io.github.jfernand.yabiir.datafile.oneOp
import io.github.jfernand.yabiir.format.crc32
import io.github.jfernand.yabiir.format.encodeHintV1
import io.github.jfernand.yabiir.keydir.Keydir
import io.github.jfernand.yabiir.keydir.KeydirEntry
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.WRITE

/** Ports of `crates/yabiir/src/recovery/tests.rs`. */
class RecoveryTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    /** The Rust `format::EntryHeader`, as far as these tests use it. */
    private class EntryHeader(val timestamp: Int, val keySize: Int, val valueSize: Int, val tombstone: Boolean)

    private class Written(val valuePos: Long, val totalLen: Long, val header: EntryHeader)

    /** Write one entry into [active] and return where its value is, its length and its header. */
    private fun writeEntry(active: ActiveFile, key: ByteArray, value: ByteArray, tombstone: Boolean, tstamp: Int): Written {
        val encoded = oneOp(key, value, tombstone, tstamp)
        val header = EntryHeader(tstamp, key.size, value.size, tombstone)
        val valuePos = active.appendValue(encoded)
        return Written(valuePos, encoded.size.toLong(), header)
    }

    private fun readValue(entry: KeydirEntry): ByteArray = DataFileSet(dir).use { it.readAt(entry.fileId, entry.valuePos, entry.valueSize) }

    private fun le32(v: Int): ByteArray = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(v).array()

    private fun hintBytes(key: ByteArray, valuePos: Long, header: EntryHeader): ByteArray =
        encodeHintV1(key, header.timestamp, header.valueSize, header.tombstone, valuePos)

    /**
     * Write a well-formed hint file (records + a correct trailing CRC) for
     * [fileId], so a test can drive the hint-based fast path directly
     * instead of the full-scan fallback.
     */
    private fun writeValidHintFile(fileId: Long, records: List<Triple<ByteArray, Long, EntryHeader>>) {
        val content = records.fold(ByteArray(0)) { acc, (key, pos, header) -> acc + hintBytes(key, pos, header) }
        Files.write(hintPath(dir, fileId), content + le32(crc32(content)))
    }

    private fun truncate(path: Path, len: Long) {
        FileChannel.open(path, WRITE).use { it.truncate(len) }
    }

    private fun recovered(): Keydir {
        val keydir = Keydir()
        recover(dir, discover(dir), keydir)
        return keydir
    }

    @Test
    fun `full scan recovery is last write wins`() {
        ActiveFile.create(dir, 0, 1).use { active0 ->
            writeEntry(active0, b("a"), b("a-v1"), false, 1)
            writeEntry(active0, b("b"), b("b-v1"), false, 2)
            active0.sync()
        }
        ActiveFile.create(dir, 1, 1).use { active1 ->
            writeEntry(active1, b("a"), b("a-v2"), false, 3) // overwrites file 0's "a"
            writeEntry(active1, b("c"), b("c-v1"), false, 4)
            active1.sync()
        }

        val fileIds = discover(dir)
        assertEquals(listOf(0L, 1L), fileIds)

        val keydir = Keydir()
        recover(dir, fileIds, keydir)

        assertArrayEquals(b("a-v2"), readValue(keydir[b("a")]!!))
        assertArrayEquals(b("b-v1"), readValue(keydir[b("b")]!!))
        assertArrayEquals(b("c-v1"), readValue(keydir[b("c")]!!))
        assertEquals(3, keydir.size)
    }

    @Test
    fun `hint based recovery matches full scan`() {
        // Build the exact same two-file layout as the full-scan test above,
        // but this time also write a hint file for file 0, and assert the
        // recovered keydir is equivalent (same live values for every key).
        val a: Written
        val bw: Written
        ActiveFile.create(dir, 0, 1).use { active0 ->
            a = writeEntry(active0, b("a"), b("a-v1"), false, 1)
            bw = writeEntry(active0, b("b"), b("b-v1"), false, 2)
            active0.sync()
        }

        // Hand-write file 0's hint file describing exactly what's in it,
        // with a valid trailer so this actually exercises the hint-based
        // fast path, not the fallback.
        writeValidHintFile(0, listOf(Triple(b("a"), a.valuePos, a.header), Triple(b("b"), bw.valuePos, bw.header)))

        ActiveFile.create(dir, 1, 1).use { active1 ->
            writeEntry(active1, b("a"), b("a-v2"), false, 3)
            writeEntry(active1, b("c"), b("c-v1"), false, 4)
            active1.sync()
        }

        val keydir = recovered()

        assertArrayEquals(b("a-v2"), readValue(keydir[b("a")]!!))
        assertArrayEquals(b("b-v1"), readValue(keydir[b("b")]!!))
        assertArrayEquals(b("c-v1"), readValue(keydir[b("c")]!!))
        assertEquals(3, keydir.size)
    }

    /**
     * A hint file that fails its whole-file CRC check must not be trusted
     * at all — recovery falls back to a full scan of the data file for
     * that file id and still gets the exactly correct result, not a
     * partially-applied mix of bad hint data and real data.
     */
    @Test
    fun `hint file with invalid crc falls back to full scan`() {
        ActiveFile.create(dir, 0, 1).use { active ->
            writeEntry(active, b("a"), b("a-v1"), false, 1)
            writeEntry(active, b("b"), b("b-v1"), false, 2)
            active.sync()
        }

        // A hint file whose content doesn't match its own trailer at all —
        // and, worse, describes a key ("x") that was never really written,
        // at a bogus offset. If this were trusted, recovery would produce
        // a wrong keydir; falling back must ignore it completely.
        val bogusHeader = EntryHeader(timestamp = 99, keySize = 1, valueSize = 4, tombstone = false)
        // wrong trailer, not this content's real CRC
        Files.write(hintPath(dir, 0), hintBytes(b("x"), 0, bogusHeader) + le32(0))

        val keydir = recovered()

        assertNull(keydir[b("x")], "bogus hint-only key must not appear")
        assertArrayEquals(b("a-v1"), readValue(keydir[b("a")]!!))
        assertArrayEquals(b("b-v1"), readValue(keydir[b("b")]!!))
        assertEquals(2, keydir.size)
    }

    /**
     * A hint file that passes its CRC (so it wasn't corrupted or torn) but
     * contains a pointer past the end of the data file — internally
     * inconsistent in a way a checksum alone can't catch (e.g. a hint file
     * misattributed to the wrong data file) — must also fall back rather
     * than hand out a [KeydirEntry] a read would fail (or worse, silently
     * misread neighboring bytes) on.
     */
    @Test
    fun `hint file with out of bounds pointer falls back to full scan`() {
        val a: Written
        ActiveFile.create(dir, 0, 1).use { active ->
            a = writeEntry(active, b("a"), b("a-v1"), false, 1)
            active.sync()
        }

        val dataLen = Files.size(dataPath(dir, 0))
        val outOfBoundsHeader = EntryHeader(timestamp = 1, keySize = 1, valueSize = 4, tombstone = false)
        // A pointer that starts exactly at EOF — value_size alone already
        // pushes it past the file's real length.
        writeValidHintFile(0, listOf(Triple(b("a"), a.valuePos, a.header), Triple(b("z"), dataLen, outOfBoundsHeader)))

        val keydir = recovered()

        assertNull(keydir[b("z")], "out-of-bounds hint key must not appear")
        assertArrayEquals(b("a-v1"), readValue(keydir[b("a")]!!))
        assertEquals(1, keydir.size)
    }

    @Test
    fun `truncated tail keeps earlier entries and does not error`() {
        val last: Written
        ActiveFile.create(dir, 0, 1).use { active ->
            writeEntry(active, b("a"), b("a-v1"), false, 1)
            writeEntry(active, b("b"), b("b-v1"), false, 2)
            last = writeEntry(active, b("c"), b("c-v1"), false, 3)
            active.sync()
        }

        // Truncate a few bytes into the last entry's value.
        val path = dataPath(dir, 0)
        truncate(path, Files.size(path) - minOf(last.totalLen, 3))

        val keydir = recovered() // must not throw

        assertArrayEquals(b("a-v1"), readValue(keydir[b("a")]!!))
        assertArrayEquals(b("b-v1"), readValue(keydir[b("b")]!!))
        assertNull(keydir[b("c")]) // torn write, correctly dropped
    }

    @Test
    fun `corrupt entry mid file is skipped but scan continues`() {
        val bw: Written
        ActiveFile.create(dir, 0, 1).use { active ->
            writeEntry(active, b("a"), b("a-v1"), false, 1)
            bw = writeEntry(active, b("b"), b("b-v1"), false, 2)
            writeEntry(active, b("c"), b("c-v1"), false, 3)
            active.sync()
        }

        // Flip a bit inside "b"'s value bytes — CRC will no longer match.
        val path = dataPath(dir, 0)
        val bytes = Files.readAllBytes(path)
        val at = bw.valuePos.toInt()
        bytes[at] = (bytes[at].toInt() xor 0xFF).toByte()
        Files.write(path, bytes)

        val keydir = recovered()

        assertArrayEquals(b("a-v1"), readValue(keydir[b("a")]!!))
        assertNull(keydir[b("b")]) // corrupt entry skipped, not applied
        assertArrayEquals(b("c-v1"), readValue(keydir[b("c")]!!))
    }

    @Test
    fun `orphaned hint file is ignored`() {
        // A hint file with no matching data file.
        Files.createFile(hintPath(dir, 0))

        val fileIds = discover(dir)
        assertTrue(fileIds.isEmpty()) // discover only ever lists *.bitcask.data

        val keydir = Keydir()
        recover(dir, fileIds, keydir) // must not throw
        assertEquals(0, keydir.size)
    }

    @Test
    fun `truncation on a non last file still recovers what it can`() {
        // Two files; truncate the OLDER (non-last) one. Recovery should
        // still keep everything decodable before the truncation point, log
        // a warning (not asserted here — stderr), and not error.
        val last: Written
        ActiveFile.create(dir, 0, 1).use { active0 ->
            writeEntry(active0, b("a"), b("a-v1"), false, 1)
            last = writeEntry(active0, b("b"), b("b-v1"), false, 2)
            active0.sync()
        }

        val path0 = dataPath(dir, 0)
        truncate(path0, Files.size(path0) - minOf(last.totalLen, 3))

        ActiveFile.create(dir, 1, 1).use { active1 ->
            writeEntry(active1, b("c"), b("c-v1"), false, 3)
            active1.sync()
        }

        val keydir = recovered()

        assertArrayEquals(b("a-v1"), readValue(keydir[b("a")]!!))
        assertNull(keydir[b("b")])
        assertArrayEquals(b("c-v1"), readValue(keydir[b("c")]!!))
    }
}
