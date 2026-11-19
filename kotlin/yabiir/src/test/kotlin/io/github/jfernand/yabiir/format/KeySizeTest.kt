package io.github.jfernand.yabiir.format

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/** Ports of the tests in `crates/yabiir/src/format/mod.rs`. */
class KeySizeTest {
    @Test
    fun `ksz flag round trip`() {
        val max = MAX_KEY_LEN.toInt()
        assertEquals(0 to false, decodeKeySize(encodeKeySize(0, false)))
        assertEquals(0 to true, decodeKeySize(encodeKeySize(0, true)))
        assertEquals(max to false, decodeKeySize(encodeKeySize(max, false)))
        assertEquals(max to true, decodeKeySize(encodeKeySize(max, true)))
    }
}
