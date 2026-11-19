package io.github.jfernand.yabiir

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertSame
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.IOException

/** Ports of the tests in `crates/yabiir/src/error.rs`. */
class ErrorsTest {
    @Test
    fun `display produces a nonempty message naming the variant`() {
        val cases: List<Pair<YabiirException, String>> =
            listOf(
                YabiirException.ConditionFailed(3) to "position 3",
                YabiirException.BatchTooLarge() to "batch too large",
                YabiirException.Io(IOException("boom")) to "boom",
                YabiirException.AlreadyLocked() to "locked",
                YabiirException.EmptyKey() to "empty",
                YabiirException.ReadOnly() to "read-only",
                YabiirException.KeyTooLarge() to "key too large",
                YabiirException.ValueTooLarge() to "value too large",
                YabiirException.Unordered() to "sorted keydir",
            )
        for ((err, needle) in cases) {
            val msg = err.message.orEmpty()
            assertFalse(msg.isEmpty(), "${err.variant} produced an empty message")
            assertTrue(msg.contains(needle), "${err.variant}'s message \"$msg\" should mention \"$needle\"")
        }
    }

    @Test
    fun `source chains to the inner io error only for the io variant`() {
        val inner = IOException("boom")
        assertSame(inner, YabiirException.Io(inner).cause)
        assertNull(YabiirException.EmptyKey().cause)
        assertNull(YabiirException.ReadOnly().cause)
    }
}
