package io.github.jfernand.yabiir.format

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

/**
 * Ports of the tests in `crates/yabiir/src/format/hint.rs`.
 *
 * The Kotlin port has no record-at-a-time `read_hint` or separate
 * `verify_hint_file`: a hint file is only ever read whole, through
 * [decodeHintFile], which checks the trailer and then parses every record.
 * So the tests of `read_hint` encode their record(s) with [encodeHintV1],
 * add the whole-file trailer, and decode that; "truncated" and "fails its
 * CRC" become a [BadHintFile] naming which.
 */
class HintTest {
    private fun b(s: String) = s.toByteArray()

    private fun withTrailer(content: ByteArray): ByteArray {
        val trailer = ByteArray(HINT_TRAILER_SIZE)
        trailer.putU32(0, crc32(content))
        return content + trailer
    }

    private fun assertRecord(
        record: HintRecord,
        seq: Long,
        timestamp: Int,
        valueSize: Long,
        tombstone: Boolean,
        valuePos: Long,
        key: ByteArray,
    ) {
        assertEquals(seq, record.seq, "seq")
        assertEquals(timestamp, record.timestamp, "timestamp")
        assertEquals(valueSize, record.valueSize, "valueSize")
        assertEquals(tombstone, record.tombstone, "tombstone")
        assertEquals(valuePos, record.valuePos, "valuePos")
        assertArrayEquals(key, record.key, "key")
    }

    private fun assertSameRecords(want: List<HintRecord>, got: List<HintRecord>) {
        assertEquals(want.size, got.size)
        for ((w, g) in want.zip(got)) assertRecord(g, w.seq, w.timestamp, w.valueSize, w.tombstone, w.valuePos, w.key)
    }

    @Test
    fun `hint round trip`() {
        val encoded = encodeHintV1(b("key"), tstamp = 42, valueSize = 10, tombstone = false, valuePos = 12345)
        val got = decodeHintFile(withTrailer(encoded)).single()
        assertRecord(got, seq = 1, timestamp = 42, valueSize = 10, tombstone = false, valuePos = 12345, key = b("key"))
    }

    @Test
    fun `clean eof at boundary is none`() {
        assertTrue(decodeHintFile(withTrailer(ByteArray(0))).isEmpty())
    }

    /**
     * Matches `entry.rs`'s `truncation_is_detected_at_every_boundary` —
     * hint records need the same guarantee.
     */
    @Test
    fun `truncation is detected at every boundary`() {
        val encoded = encodeHintV1(b("key"), tstamp = 42, valueSize = 10, tombstone = false, valuePos = 12345)
        for (cut in 1 until encoded.size) {
            val e = assertThrows<BadHintFile>("cut at $cut") { decodeHintFile(withTrailer(encoded.copyOfRange(0, cut))) }
            assertTrue(e.message!!.contains("truncated"), "cut at $cut: ${e.message}")
        }
    }

    @Test
    fun `verify hint file accepts a correctly trailered file`() {
        val content =
            encodeHintV1(b("a"), tstamp = 1, valueSize = 1, tombstone = false, valuePos = 0) +
                encodeHintV1(b("b"), tstamp = 1, valueSize = 1, tombstone = false, valuePos = 1)
        val got = decodeHintFile(withTrailer(content))
        assertEquals(2, got.size)
        assertRecord(got[0], seq = 1, timestamp = 1, valueSize = 1, tombstone = false, valuePos = 0, key = b("a"))
        assertRecord(got[1], seq = 1, timestamp = 1, valueSize = 1, tombstone = false, valuePos = 1, key = b("b"))
    }

    @Test
    fun `verify hint file rejects corrupted content`() {
        val file = withTrailer(encodeHintV1(b("a"), tstamp = 1, valueSize = 1, tombstone = false, valuePos = 0))
        file[0] = (file[0].toInt() xor 0xFF).toByte() // flip a bit inside a record, after computing the trailer
        val e = assertThrows<BadHintFile> { decodeHintFile(file) }
        assertTrue(e.message!!.contains("CRC"), e.message)
    }

    @Test
    fun `verify hint file rejects a file too short to hold a trailer`() {
        assertThrows<BadHintFile> { decodeHintFile(ByteArray(HINT_TRAILER_SIZE - 1)) }
    }

    @Test
    fun `verify hint file accepts an empty hint file`() {
        // A merge input with no live entries at all still finishes its
        // (empty) hint file with a trailer — the CRC of zero bytes.
        val trailer = withTrailer(ByteArray(0))
        assertEquals(HINT_TRAILER_SIZE, trailer.size)
        assertTrue(decodeHintFile(trailer).isEmpty())
    }

    @Test
    fun `encoded hint len and is empty match the real bytes`() {
        // Rust also checks that `as_ref` equals `as_bytes`; the Kotlin port
        // returns a plain ByteArray, so only the length checks are left.
        val encoded = encodeHintV1(b("key"), tstamp = 0, valueSize = 0, tombstone = false, valuePos = 0)
        assertEquals(HINT_HEADER_SIZE + 3, encoded.size)
        assertTrue(encoded.isNotEmpty())
    }

    private fun v2Record(key: ByteArray, seq: Long) =
        HintRecord(seq = seq, timestamp = 7, valueSize = 5, tombstone = false, valuePos = seq + 1000, key = key)

    @Test
    fun `a v2 hint file decodes to its records`() {
        // -1 is u64::MAX; its value_pos wraps, as Rust's `wrapping_add` does.
        val records = listOf(v2Record(b("a"), 1), v2Record(b("bb"), -1L), v2Record(b(""), 3))
        var content = HINT_MAGIC.copyOf()
        for (r in records) content += encodeHintV2(r)
        assertSameRecords(records, decodeHintFile(withTrailer(content)))
    }

    @Test
    fun `an empty v2 hint file decodes to nothing`() {
        assertTrue(decodeHintFile(withTrailer(HINT_MAGIC.copyOf())).isEmpty())
    }

    @Test
    fun `a v1 hint file decodes with every seq at one`() {
        val content = encodeHintV1(b("k"), tstamp = 4, valueSize = 2, tombstone = false, valuePos = 99)
        val got = decodeHintFile(withTrailer(content))
        assertEquals(1, got.size)
        assertRecord(got[0], seq = 1, timestamp = 4, valueSize = 2, tombstone = false, valuePos = 99, key = b("k"))
    }

    @Test
    fun `a v2 hint file ending inside a record is rejected`() {
        val record = encodeHintV2(v2Record(b("key"), 1))
        val content = HINT_MAGIC + record.copyOfRange(0, record.size - 1)
        assertThrows<BadHintFile> { decodeHintFile(withTrailer(content)) }
    }

    @Test
    fun `a hint file failing its crc is rejected`() {
        val file = withTrailer(HINT_MAGIC.copyOf())
        file[0] = (file[0].toInt() xor 1).toByte()
        assertThrows<BadHintFile> { decodeHintFile(file) }
    }
}
