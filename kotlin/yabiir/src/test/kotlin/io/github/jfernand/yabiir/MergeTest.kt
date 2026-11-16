package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.format.FILE_HEADER_SIZE
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path

/** Ports of the merge regressions in `crates/yabiir/src/merge/tests.rs` and `engine/tests/versions.rs`. */
class MergeTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    @Test
    fun `a merge that keeps nothing leaves one empty file`() {
        Engine.open(dir).use { db ->
            db.merge()
            assertTrue(discover(dir).isEmpty())
            db.put(b("k"), b("v"))
            db.delete(b("k"))
            db.merge()
            val ids = discover(dir)
            assertEquals(1, ids.size)
            assertEquals(FILE_HEADER_SIZE.toLong(), Files.size(dataPath(dir, ids[0])))
            assertNull(db.get(b("k")))
        }
    }

    @Test
    fun `a version is never reused after a merge that kept nothing`() {
        val old: Long
        Engine.open(dir).use { db ->
            db.put(b("k"), b("old"))
            old = db.getVersioned(b("k"))!!.version
            db.delete(b("k"))
            db.merge()
        }
        Engine.open(dir).use { db ->
            db.put(b("k"), b("new"))
            val new = db.getVersioned(b("k"))!!.version
            assertTrue(new > old + 1, "old $old, new $new")
        }
    }

    @Test
    fun `merge keeps an input file it could not copy the live values of`() {
        val opts = Options(maxFileSize = 1)
        Engine.open(dir, opts).use { db ->
            db.put(b("a"), b("1"))
            db.put(b("b"), b("2"))
            db.merge()
        }
        val damaged = discover(dir)[0]
        val path = dataPath(dir, damaged)
        val bytes = Files.readAllBytes(path)
        bytes[0] = (bytes[0].toInt() xor 0x10).toByte()
        Files.write(path, bytes)

        val expected = listOf("a" to "1", "b" to "2", "c" to "3")
        Engine.open(dir, opts).use { db ->
            assertArrayEquals(b("1"), db.get(b("a")))
            db.put(b("c"), b("3"))
            db.merge()
            assertTrue(damaged in discover(dir), "the damaged file stays")
            for ((k, v) in expected) assertArrayEquals(b(v), db.get(b(k)))
        }
        Engine.open(dir, opts).use { db -> for ((k, v) in expected) assertArrayEquals(b(v), db.get(b(k)), "after reopen") }
    }
}
