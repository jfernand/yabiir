package io.github.jfernand.yabiir.format

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertInstanceOf
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.ByteArrayInputStream
import java.io.FilterInputStream
import java.io.InputStream

/**
 * Ports of the tests in `crates/yabiir/src/format/entry.rs`. The Kotlin
 * port reads a version 1 entry as a [Record] of one operation, so an
 * `Entry` is `record.ops[0]` here, and `CrcMismatch` is [RecordRead.Corrupt].
 */
class EntryTest {
    private fun b(s: String) = s.toByteArray()

    private fun readAll(bytes: ByteArray): RecordRead? = readEntry(ByteArrayInputStream(bytes), bytes.size.toLong())

    @Test
    fun `entry round trip`() {
        val cases =
            listOf(
                Triple(b("k"), b("v"), false),
                Triple(b("empty-value"), ByteArray(0), false),
                Triple(b("tombstoned"), ByteArray(0), true),
                Triple(b("large"), ByteArray(1024 * 1024) { 7 }, false),
            )
        for ((key, value, tombstone) in cases) {
            val encoded = encodeEntry(key, value, tombstone, 1_700_000_000)
            // Rust's `value_len()`: the value is the tail of the entry.
            assertEquals(ENTRY_HEADER_SIZE + key.size + value.size, encoded.size)
            val ok = assertInstanceOf(RecordRead.Ok::class.java, readAll(encoded))
            val entry = ok.record.ops.single()
            assertArrayEquals(key, entry.key)
            assertArrayEquals(value, entry.value)
            assertEquals(tombstone, entry.tombstone)
            assertEquals(1_700_000_000, ok.record.timestamp)
            assertEquals(encoded.size.toLong(), ok.totalLen)
        }
    }

    @Test
    fun `corrupt crc is detected`() {
        val encoded = encodeEntry(b("key"), b("value"), false, 0)
        // flip a bit inside the body, past the header, leaving the header
        // (and hence declared lengths) intact so this is a same-length,
        // same-shape but corrupt entry.
        val last = encoded.size - 1
        encoded[last] = (encoded[last].toInt() xor 0xFF).toByte()
        val corrupt = assertInstanceOf(RecordRead.Corrupt::class.java, readAll(encoded))
        assertEquals(ENTRY_HEADER_SIZE.toLong() + 8, corrupt.totalLen)
    }

    @Test
    fun `truncation is detected at every boundary`() {
        val encoded = encodeEntry(b("key"), b("value"), false, 0)
        for (cut in 1 until encoded.size) {
            val read = readEntry(ByteArrayInputStream(encoded.copyOfRange(0, cut)), cut.toLong())
            assertSame(RecordRead.Truncated, read, "cut at $cut")
        }
    }

    /** A stream that counts the bytes it hands out. */
    private class Counting(input: InputStream) : FilterInputStream(input) {
        var nRead = 0L

        override fun read(): Int = super.read().also { if (it >= 0) nRead++ }

        override fun read(b: ByteArray, off: Int, len: Int): Int =
            super.read(b, off, len).also { if (it > 0) nRead += it }
    }

    /**
     * An entry whose header claims a value of `valueSize` bytes, followed
     * by the real (much shorter) body: a torn or corrupt header.
     */
    private fun lyingEntry(valueSize: Int): ByteArray {
        val bytes = encodeEntry(b("key"), b("value"), false, 0)
        bytes.putU32(12, valueSize)
        return bytes
    }

    @Test
    fun `a body longer than the available bytes is truncated without reading it`() {
        // 6, 1 MiB, and u32::MAX (-1 as a signed Int)
        for (valueSize in listOf(6, 1 shl 20, -1)) {
            val bytes = lyingEntry(valueSize)
            val reader = Counting(ByteArrayInputStream(bytes))
            val read = readEntry(reader, bytes.size.toLong())
            assertSame(RecordRead.Truncated, read, "value_size ${valueSize.unsigned()}: expected Truncated")
            assertEquals(
                ENTRY_HEADER_SIZE.toLong(),
                reader.nRead,
                "value_size ${valueSize.unsigned()}: read past the header",
            )
        }
    }

    @Test
    fun `the available bytes bound the entry not the reader`() {
        // The reader holds two entries, but only the first is available:
        // the second must read as truncated, not be taken from beyond the
        // bound.
        val first = encodeEntry(b("a"), b("1"), false, 0)
        val second = encodeEntry(b("b"), b("2"), false, 0)
        val input = ByteArrayInputStream(first + second)

        assertInstanceOf(RecordRead.Ok::class.java, readEntry(input, first.size.toLong()))
        assertSame(RecordRead.Truncated, readEntry(input, second.size.toLong() - 1))
    }

    @Test
    fun `clean eof at boundary is none`() {
        assertNull(readEntry(ByteArrayInputStream(ByteArray(0)), 0))
    }

    @Test
    fun `is tombstone reflects the header flag`() {
        val live = encodeEntry(b("k"), b("v"), false, 0)
        val dead = encodeEntry(b("k"), ByteArray(0), true, 0)
        val liveEntry = assertInstanceOf(RecordRead.Ok::class.java, readAll(live)).record.ops.single()
        val deadEntry = assertInstanceOf(RecordRead.Ok::class.java, readAll(dead)).record.ops.single()
        assertFalse(liveEntry.tombstone)
        assertTrue(deadEntry.tombstone)
    }

    @Test
    fun `encoded entry len and is empty match the real bytes`() {
        // Rust also checks that `as_ref` equals `as_bytes`; the Kotlin port
        // returns a plain ByteArray, so only the length checks are left.
        val encoded = encodeEntry(b("key"), b("value"), false, 0)
        val expectedLen = ENTRY_HEADER_SIZE + 3 + 5
        assertEquals(expectedLen, encoded.size)
        assertTrue(encoded.isNotEmpty())
    }
}
