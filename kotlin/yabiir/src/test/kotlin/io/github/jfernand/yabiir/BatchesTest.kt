package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.discover
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import kotlin.concurrent.thread

/** A port of `crates/yabiir/src/engine/tests/batches.rs`. */
class BatchesTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    private fun open(opts: Options = Options()) = Engine.open(dir, opts)

    @Test
    fun `a batch writes every op with one version`() {
        open().use { db ->
            db.put(b("gone"), b("x"))
            val v = db.writeBatch(listOf(Op.Put(b("a"), b("1")), Op.Put(b("b"), b("2")), Op.Delete(b("gone")))).join()!!
            assertArrayEquals(b("1"), db.get(b("a")))
            assertNull(db.get(b("gone")))
            assertEquals(v, db.getVersioned(b("a"))!!.version)
            assertEquals(v, db.getVersioned(b("b"))!!.version)
        }
    }

    @Test
    fun `a failed check writes nothing and names its position`() {
        open().use { db ->
            db.put(b("k"), b("old"))
            val v = db.getVersioned(b("k"))!!.version
            val e =
                assertThrows<YabiirException.ConditionFailed> {
                    db.writeBatch(
                        listOf(
                            Op.Put(b("other"), b("x")),
                            Op.Check(b("k"), Cond.Exists),
                            Op.Check(b("k"), Cond.Version(v + 1)),
                            Op.Put(b("k"), b("new")),
                        ),
                    ).join()
                }
            assertEquals(2, e.index)
            assertArrayEquals(b("old"), db.get(b("k")))
            assertNull(db.get(b("other")))
        }
    }

    @Test
    fun `a version check is a compare and swap, and a recreated key gets a new version`() {
        open().use { db ->
            val created = db.writeBatch(listOf(Op.Check(b("k"), Cond.Absent), Op.Put(b("k"), b("1")))).join()!!
            assertThrows<YabiirException.ConditionFailed> {
                db.writeBatch(listOf(Op.Check(b("k"), Cond.Absent), Op.Put(b("k"), b("x")))).join()
            }
            val updated = db.writeBatch(listOf(Op.Check(b("k"), Cond.Version(created)), Op.Put(b("k"), b("2")))).join()!!
            assertTrue(updated > created)
            db.delete(b("k"))
            db.put(b("k"), b("again"))
            assertThrows<YabiirException.ConditionFailed> {
                db.writeBatch(listOf(Op.Check(b("k"), Cond.Version(updated)), Op.Put(b("k"), b("4")))).join()
            }
        }
    }

    @Test
    fun `invalid batches fail before reaching the writer`() {
        open().use { db ->
            assertThrows<YabiirException.EmptyKey> { db.writeBatch(listOf(Op.Put(b("ok"), b("1")), Op.Put(ByteArray(0), b("2")))).join() }
            assertNull(db.get(b("ok")))
        }
        Engine.open(dir, Options().readOnly()).use { ro ->
            assertThrows<YabiirException.ReadOnly> { ro.writeBatch(listOf(Op.Put(b("k"), b("v")))).join() }
        }
    }

    @Test
    fun `a second read-write handle is refused`() {
        open().use {
            assertThrows<YabiirException.AlreadyLocked> { open() }
        }
        open().close()
    }

    @Test
    fun `concurrent compare and swaps lose no increment`() {
        open().use { db ->
            db.put(b("n"), b("0"))
            val threads =
                (0 until 8).map {
                    thread {
                        var committed = 0
                        while (committed < 25) {
                            val current = db.getVersioned(b("n"))!!
                            val next = (String(current.value).toLong() + 1).toString()
                            try {
                                db.writeBatch(listOf(Op.Check(b("n"), Cond.Version(current.version)), Op.Put(b("n"), b(next)))).join()
                                committed++
                            } catch (e: YabiirException.ConditionFailed) {
                            }
                        }
                    }
                }
            threads.forEach { it.join() }
            assertEquals("200", String(db.get(b("n"))!!))
        }
    }

    @Test
    fun `a batch cut anywhere by a crash recovers whole or not at all`() {
        val path: Path
        val batchStart: Long
        open().use { db ->
            db.put(b("a"), b("before"))
            path = dataPath(dir, discover(dir).last())
            batchStart = Files.size(path)
            db.writeBatch(
                listOf(Op.Put(b("a"), b("after")), Op.Put(b("b"), b("new")), Op.Delete(b("a")), Op.Put(b("c"), ByteArray(300) { 7 })),
            ).join()
        }
        val whole = Files.readAllBytes(path)
        for (cut in batchStart until whole.size) {
            val crashed = Files.createTempDirectory(dir, "crashed")
            Files.write(dataPath(crashed, 0), whole.copyOf(cut.toInt()))
            Engine.open(crashed).use { db ->
                assertArrayEquals(b("before"), db.get(b("a")), "cut at $cut")
                assertNull(db.get(b("b")), "cut at $cut")
                assertNull(db.get(b("c")), "cut at $cut")
            }
        }
        open().use { db ->
            assertNull(db.get(b("a")))
            assertArrayEquals(b("new"), db.get(b("b")))
        }
    }

    @Test
    fun `a batch survives merge and reopen`() {
        val v: Long
        open(Options(maxFileSize = 1)).use { db ->
            v = db.writeBatch(listOf(Op.Put(b("a"), b("1")), Op.Put(b("b"), b("2")), Op.Put(b("c"), b("3")))).join()!!
            db.put(b("b"), b("22"))
            db.merge()
            assertEquals(v, db.getVersioned(b("a"))!!.version)
        }
        open().use { db ->
            assertArrayEquals(b("22"), db.get(b("b")))
            assertEquals(v, db.getVersioned(b("c"))!!.version)
        }
    }

    @Test
    fun `writes during a merge win over its copies`() {
        open(Options(maxFileSize = 64)).use { db ->
            for (i in 0 until 50) db.put(b("k${i % 10}"), b("v$i"))
            db.mergeWithHook { key -> db.put(key, b("during")) }
            for (i in 0 until 10) assertArrayEquals(b("during"), db.get(b("k$i")))
        }
        open().use { db -> for (i in 0 until 10) assertArrayEquals(b("during"), db.get(b("k$i"))) }
    }
}
