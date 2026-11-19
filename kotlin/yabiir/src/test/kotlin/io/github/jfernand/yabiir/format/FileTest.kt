package io.github.jfernand.yabiir.format

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ports of the tests in `crates/yabiir/src/format/file.rs`. */
class FileTest {
    @Test
    fun `a header identifies as v2 with its floor`() {
        val header = encodeFileHeader(123_456)
        assertEquals(FileFormat.V2(123_456), identify(header))
    }

    @Test
    fun `a v1 file identifies as v1`() {
        val entry = encodeEntry("key".toByteArray(), "value".toByteArray(), false, 1_700_000_000)
        assertEquals(FileFormat.V1, identify(entry.copyOfRange(0, FILE_HEADER_SIZE)))
        // Shorter than a header too: a v1 file holding one small entry.
        val small = encodeEntry("k".toByteArray(), ByteArray(0), false, 0)
        assertTrue(small.size < FILE_HEADER_SIZE)
        assertEquals(FileFormat.V1, identify(small))
    }

    @Test
    fun `an empty or torn header is an empty v2 file`() {
        val header = encodeFileHeader(9)
        for (cut in 0 until FILE_HEADER_SIZE) {
            assertEquals(FileFormat.V2(0), identify(header.copyOfRange(0, cut)), "cut at $cut")
        }
    }

    @Test
    fun `a header failing its crc keeps the format but loses the floor`() {
        val header = encodeFileHeader(9)
        header[10] = (header[10].toInt() xor 1).toByte()
        assertEquals(FileFormat.V2(0), identify(header))
    }

    @Test
    fun `data starts after the header only in v2`() {
        assertEquals(0L, FileFormat.V1.dataStart)
        assertEquals(FILE_HEADER_SIZE.toLong(), FileFormat.V2(0).dataStart)
    }
}
