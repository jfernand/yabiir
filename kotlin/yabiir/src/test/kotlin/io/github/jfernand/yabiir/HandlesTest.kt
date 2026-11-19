package io.github.jfernand.yabiir

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

/**
 * A port of `crates/yabiir/src/engine/tests/handles.rs`. Rust's "drop the
 * handle" is closing it here: a JVM object has no destructor, and
 * [Engine.close] is what `Drop` does in Rust.
 */
class HandlesTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = bytesOf(s)

    /** A second read-write handle on a directory that already has one open must fail cleanly. */
    @Test
    fun `second read-write open is rejected while first is held`() {
        openEngine(dir).use {
            assertThrows<YabiirException.AlreadyLocked> { Engine.open(dir, Options()) }
        }
    }

    /** A read-only open succeeds alongside a read-write handle: it never takes the write lock. */
    @Test
    fun `read-only open succeeds concurrently with a read-write open`() {
        openEngine(dir).use { writer ->
            writer.put(b("k"), b("v"))
            Engine.open(dir, Options().readOnly()).use { reader ->
                assertArrayEquals(b("v"), reader.get(b("k")))
                // The writer is still usable too: the read-only open didn't disturb it.
                writer.put(b("k2"), b("v2"))
                assertArrayEquals(b("v2"), writer.get(b("k2")))
            }
        }
    }

    /** Letting go of the read-write handle releases the lock, so the next read-write open succeeds. */
    @Test
    fun `dropping the write handle releases the lock for the next open`() {
        openEngine(dir).use { db -> db.put(b("k"), b("v")) } // released at the end of the block
        openEngine(dir).use { db -> assertArrayEquals(b("v"), db.get(b("k"))) } // must not throw AlreadyLocked
    }

    /** An explicit `close()` releases the lock too. */
    @Test
    fun `close releases the lock for the next open`() {
        val db = openEngine(dir)
        db.put(b("k"), b("v"))
        db.close()
        openEngine(dir).use { again -> assertArrayEquals(b("v"), again.get(b("k"))) } // must not throw AlreadyLocked
    }

    /** Read-only handles never touch the write lock, so two of them coexist with no writer present. */
    @Test
    fun `multiple read-only opens coexist`() {
        openEngine(dir).use { db -> db.put(b("k"), b("v")) }
        Engine.open(dir, Options().readOnly()).use { r1 ->
            Engine.open(dir, Options().readOnly()).use { r2 ->
                assertArrayEquals(b("v"), r1.get(b("k")))
                assertArrayEquals(b("v"), r2.get(b("k")))
            }
        }
    }

    /** A read-only handle can't mutate the store through `merge` either. */
    @Test
    fun `merge is rejected on a read-only handle`() {
        openEngine(dir).use { db -> db.put(b("k"), b("v")) }
        Engine.open(dir, Options().readOnly()).use { ro ->
            assertThrows<YabiirException.ReadOnly> { ro.merge() }
        }
    }
}
