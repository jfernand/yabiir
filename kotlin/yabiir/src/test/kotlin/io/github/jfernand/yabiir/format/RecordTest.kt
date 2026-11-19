package io.github.jfernand.yabiir.format

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream

/** Ports of the tests in `crates/yabiir/src/format/record.rs`. */
class RecordTest {
    private fun b(s: String) = s.toByteArray()

    private fun put(key: ByteArray, value: ByteArray) = OpRef(key, value, tombstone = false)

    private fun delete(key: ByteArray) = OpRef(key, ByteArray(0), tombstone = true)

    private fun readAll(bytes: ByteArray): RecordRead? = readRecord(ByteArrayInputStream(bytes), bytes.size.toLong())

    @Test
    fun `a record round trips with every operation in order`() {
        val big = ByteArray(100_000) { 7 }
        val ops = listOf(put(b("a"), b("1")), delete(b("b")), put(b("c"), b("")), put(b("d"), big))
        val encoded = encodeRecord(42, 1_700_000_000, ops)
        val ok = assertInstanceOf(RecordRead.Ok::class.java, readAll(encoded.bytes), "expected a record")
        val record = ok.record
        assertEquals(encoded.size.toLong(), ok.totalLen)
        assertEquals(42L, record.seq)
        assertEquals(1_700_000_000, record.timestamp)
        assertEquals(ops.size, record.ops.size)
        for ((i, pair) in record.ops.zip(ops).withIndex()) {
            val (got, want) = pair
            assertArrayEquals(want.key, got.key)
            assertArrayEquals(want.value, got.value)
            assertEquals(want.tombstone, got.tombstone)
            assertEquals(encoded.valueOffset(i), got.valueOffset)
            val at = got.valueOffset.toInt()
            assertArrayEquals(want.value, encoded.bytes.copyOfRange(at, at + want.value.size))
        }
    }

    @Test
    fun `an empty record round trips`() {
        val encoded = encodeRecord(1, 0, emptyList())
        assertEquals(RECORD_HEADER_SIZE, encoded.size)
        val ok = assertInstanceOf(RecordRead.Ok::class.java, readAll(encoded.bytes), "expected a record")
        assertTrue(ok.record.ops.isEmpty())
    }

    @Test
    fun `a record cut anywhere reads as truncated`() {
        val bytes = encodeRecord(9, 0, listOf(put(b("key"), b("value")), delete(b("gone")))).bytes
        for (cut in 1 until bytes.size) {
            val got = readRecord(ByteArrayInputStream(bytes.copyOfRange(0, cut)), cut.toLong())
            assertSame(RecordRead.Truncated, got, "cut at $cut")
        }
    }

    @Test
    fun `a flipped bit anywhere after the length reads as corrupt`() {
        val encoded = encodeRecord(9, 3, listOf(put(b("key"), b("value")), delete(b("gone"))))
        // The length field (bytes 4..8) decides how much is read, so a flip
        // there reads as truncated or as a different record; past it, every
        // flip has to fail the CRC.
        for (at in (0 until 4) + (8 until encoded.size)) {
            val bytes = encoded.bytes.copyOf()
            bytes[at] = (bytes[at].toInt() xor 0x10).toByte()
            val got = readAll(bytes)
            val corrupt = assertInstanceOf(RecordRead.Corrupt::class.java, got, "flip at $at")
            assertEquals(bytes.size.toLong(), corrupt.totalLen, "flip at $at")
        }
    }

    @Test
    fun `a length longer than the source is truncated without reading the body`() {
        val bytes = encodeRecord(1, 0, listOf(put(b("k"), b("v")))).bytes.copyOf()
        bytes.putU32(4, -1) // u32::MAX
        assertSame(RecordRead.Truncated, readAll(bytes))
    }

    @Test
    fun `a body that disagrees with its op count is corrupt even with a valid crc`() {
        val bytes = encodeRecord(1, 0, listOf(put(b("k"), b("v")))).bytes.copyOf()
        bytes.putU32(20, 2)
        bytes.putU32(0, crc32(bytes, 4, bytes.size))
        assertInstanceOf(RecordRead.Corrupt::class.java, readAll(bytes))
    }

    @Test
    fun `a clean end reads as none`() {
        assertNull(readAll(ByteArray(0)))
    }

    @Test
    fun `body len counts every op header key and value`() {
        val ops = listOf(put(b("ab"), b("cde")), delete(b("f")))
        assertEquals((OP_HEADER_SIZE * 2 + 2 + 3 + 1).toLong(), bodyLen(ops))
        assertEquals(RECORD_HEADER_SIZE.toLong() + bodyLen(ops), encodeRecord(0, 0, ops).size.toLong())
    }
}
