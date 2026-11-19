package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.datafile.hintPath
import io.github.jfernand.yabiir.format.FILE_HEADER_SIZE
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
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

    /** Every put rotates into its own file. */
    private val tiny = Options(maxFileSize = 1)

    @Test
    fun `basic compaction keeps only latest version`() {
        Engine.open(dir, tiny).use { db ->
            db.put(b("A"), b("v1"))
            db.put(b("A"), b("v2"))
            db.put(b("A"), b("v3"))

            val beforeIds = discover(dir)
            assertEquals(3, beforeIds.size, "one file per put: $beforeIds")

            db.merge()

            assertArrayEquals(b("v3"), db.get(b("A")))
            assertEquals(1, countEntriesForKey(dir, b("A")), "only the latest version survives anywhere on disk")

            // Every file there was is gone, and what replaced it is merge output: a data file with its hint file.
            val afterIds = discover(dir)
            assertEquals(1, afterIds.size, "one live entry, one output file")
            for (id in afterIds) {
                assertFalse(id in beforeIds, "old file $id should be gone")
                assertTrue(Files.exists(hintPath(dir, id)), "merge output file $id is missing its hint file")
            }
            for (id in beforeIds) assertFalse(Files.exists(hintPath(dir, id)))
        }
    }

    /** With a tiny `maxFileSize`, the output of a merge spans several data files, each with its hint file. */
    @Test
    fun `merge output rotates when it exceeds max file size`() {
        Engine.open(dir, tiny).use { db ->
            for (i in 0 until 10) db.put(b("k$i"), b("v$i"))

            db.merge()

            val mergeOutputIds = discover(dir).filter { Files.exists(hintPath(dir, it)) }
            assertTrue(
                mergeOutputIds.size > 1,
                "expected merge's own output to rotate into multiple files under a tiny maxFileSize, got $mergeOutputIds",
            )
            for (i in 0 until 10) assertArrayEquals(b("v$i"), db.get(b("k$i")))
        }
    }

    /**
     * Many keys land in many input files, and the store is reopened with the
     * default `maxFileSize` before merging: the output is one file that never
     * fills, and only the final flush makes its entries readable.
     */
    @Test
    fun `merge flushes a partial final batch that never rotated`() {
        Engine.open(dir, tiny).use { db -> for (i in 0 until 20) db.put(b("k$i"), b("v$i")) }

        val beforeIds = discover(dir)
        assertTrue(beforeIds.size >= 20, "expected many small rotated files, got $beforeIds")

        Engine.open(dir).use { db ->
            db.merge()
            for (i in 0 until 20) {
                assertArrayEquals(
                    b("v$i"),
                    db.get(b("k$i")),
                    "key k$i missing or wrong after merge: the final-batch flush likely didn't happen",
                )
            }
            val mergeOutputIds = discover(dir).filter { it !in beforeIds && Files.exists(hintPath(dir, it)) }
            assertEquals(1, mergeOutputIds.size, "expected exactly one un-rotated merge-output file, got $mergeOutputIds")
        }

        Engine.open(dir).use { reopened -> for (i in 0 until 20) assertArrayEquals(b("v$i"), reopened.get(b("k$i"))) }
    }

    @Test
    fun `merge reports merge summary metrics`() {
        val metrics = GaugeRecordingMetrics()
        Engine.open(dir, Options(maxFileSize = 1, metrics = metrics)).use { db ->
            for (i in 0 until 10) db.put(b("k$i"), b("v$i"))
            db.merge()
        }
        val summaries = metrics.mergeSummaries.toList()
        assertEquals(1, summaries.size, "expected exactly one merge summary recorded")
        val (inputFiles, liveEntriesCopied) = summaries[0]
        assertEquals(10, inputFiles, "expected merge to report all 10 rotated-out input files")
        assertEquals(10, liveEntriesCopied, "expected merge to report all 10 live entries it copied forward")
    }

    /**
     * Many small input files reopened under the default file size, so that
     * the output never rotates and all 20 copies are repointed together,
     * by the final flush.
     */
    @Test
    fun `merge reports pending queue depth for the final batch`() {
        Engine.open(dir, Options(maxFileSize = 1)).use { db ->
            for (i in 0 until 20) db.put(b("k$i"), b("v$i"))
        }
        val metrics = GaugeRecordingMetrics()
        Engine.open(dir, Options(metrics = metrics)).use { db -> db.merge() }
        assertEquals(
            listOf(20),
            metrics.pendingQueueDepths.toList(),
            "expected exactly one pending-queue-depth sample, for the whole un-rotated final batch",
        )
    }

    /**
     * A live entry after a corrupt one in the same input file is still
     * copied: the scan keeps the right offset across the corrupt entry.
     */
    @Test
    fun `merge correctly resumes scanning after a corrupt entry in an input file`() {
        Engine.open(dir).use { db ->
            db.put(b("a"), b("a-v1"))
            db.put(b("b"), b("b-v1"))
            db.put(b("c"), b("c-v1"))
        }

        // Corrupt "b"'s value bytes in file 0: a CRC mismatch that leaves "c" intact.
        val path = dataPath(dir, 0)
        val bytes = Files.readAllBytes(path)
        val corruptAt = String(bytes, Charsets.ISO_8859_1).indexOf("b-v1")
        assertTrue(corruptAt >= 0)
        bytes[corruptAt] = (bytes[corruptAt].toInt() xor 0xFF).toByte()
        Files.write(path, bytes)

        Engine.open(dir).use { db ->
            assertArrayEquals(b("a-v1"), db.get(b("a")))
            assertNull(db.get(b("b")))
            assertArrayEquals(b("c-v1"), db.get(b("c")))

            db.merge()

            assertArrayEquals(b("a-v1"), db.get(b("a")), "a (before the corrupt entry) must survive merge")
            assertNull(db.get(b("b")))
            assertArrayEquals(b("c-v1"), db.get(b("c")), "c (after the corrupt entry, same input file) must survive merge")
        }
    }

    @Test
    fun `tombstone reclamation`() {
        Engine.open(dir, tiny).use { db ->
            db.put(b("B"), b("v"))
            db.delete(b("B"))
            db.merge()
            assertNull(db.get(b("B")))
            assertEquals(0, countEntriesForKey(dir, b("B")), "no trace, live or tombstone")
        }
    }

    /**
     * A merge takes every data file, the active one included, and whatever is
     * written afterwards goes to a file whose id is above every output file's.
     */
    @Test
    fun `merge takes every file and later writes sort after its output`() {
        Engine.open(dir, Options(maxFileSize = 40)).use { db ->
            for (i in 0 until 30) db.put(b("k$i"), b("v$i"))
            val beforeIds = discover(dir)
            assertTrue(beforeIds.size > 2, "test needs several files, got $beforeIds")

            db.merge()

            val outputIds = discover(dir)
            assertTrue(outputIds.size > 1, "the output spans several files")
            for (id in beforeIds) assertFalse(id in outputIds, "input file $id should be gone")

            db.put(b("later"), b("write"))
            val newestId = discover(dir).last()
            assertTrue(
                outputIds.all { it < newestId },
                "a write after the merge must land above the output ($outputIds), got $newestId",
            )
            for (i in 0 until 30) assertArrayEquals(b("v$i"), db.get(b("k$i")))
        }
    }

    /** `merge_of_only_dead_entries_leaves_one_empty_file` */
    @Test
    fun `a merge that keeps nothing leaves one empty file`() {
        Engine.open(dir).use { db ->
            db.merge()
            assertTrue(discover(dir).isEmpty())
            db.put(b("k"), b("v"))
            db.delete(b("k"))
            db.merge()
            val ids = discover(dir)
            assertEquals(1, ids.size, "$ids")
            assertEquals(FILE_HEADER_SIZE.toLong(), Files.size(dataPath(dir, ids[0])), "a header and no records")
            assertNull(db.get(b("k")))
            // Merging it again leaves the same: one empty file.
            db.merge()
            assertEquals(1, discover(dir).size)
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

    /** Merges are serialized: several threads merge while others write, and every key still reads back. */
    @Test
    fun `concurrent merges do not lose data`() {
        val options = Options(maxFileSize = 256)
        val check = { db: Engine ->
            for (writer in 0 until 2) {
                for (k in 0 until 40) {
                    val lastWrite = (0 until 300).reversed().first { it % 40 == k }
                    assertArrayEquals(b("v$lastWrite"), db.get(b("w$writer-k$k")))
                }
            }
        }
        Engine.open(dir, options).use { db ->
            val writers =
                (0 until 2).map { writer ->
                    { for (i in 0 until 300) db.put(b("w$writer-k${i % 40}"), b("v$i")) }
                }
            val mergers = (0 until 3).map { { repeat(20) { db.merge() } } }
            runConcurrently(writers + mergers)
            check(db)
        }
        Engine.open(dir, options).use(check)
    }

    @Test
    fun `hint files match full scan recovery post merge`() {
        Engine.open(dir, tiny).use { db ->
            db.put(b("a"), b("1"))
            db.put(b("a"), b("2"))
            db.put(b("b"), b("3"))
            db.delete(b("b"))
            db.put(b("c"), b("4"))
            db.merge()
            db.sync()

            val expectedKeys = db.listKeys().map { String(it) }.sorted()

            // A fresh, independent, read-only open: the keydir is rebuilt purely by recovery, from the hint files.
            Engine.open(dir, Options(isReadWrite = false)).use { reopened ->
                val gotKeys = reopened.listKeys().map { String(it) }.sorted()
                assertEquals(expectedKeys, gotKeys)
                for (key in gotKeys) assertArrayEquals(db.get(b(key)), reopened.get(b(key)))
            }
        }
    }

    @Test
    fun `repeated merges are idempotent`() {
        Engine.open(dir, tiny).use { db ->
            db.put(b("a"), b("1"))
            db.put(b("a"), b("2"))
            db.put(b("b"), b("3"))

            db.merge()
            assertArrayEquals(b("2"), db.get(b("a")))
            assertArrayEquals(b("3"), db.get(b("b")))

            db.merge() // merging an already-merged, unchanged datastore again
            assertArrayEquals(b("2"), db.get(b("a")))
            assertArrayEquals(b("3"), db.get(b("b")))
        }
    }

    /**
     * One thread merging in a loop while several others put/delete/get on a
     * small, overlapping keyspace, every put/delete mirrored (under one
     * shared lock) into a reference map; the final state must match it.
     */
    @Test
    fun `stress concurrent put delete and merge match reference model`() {
        Engine.open(dir, Options(maxFileSize = 256)).use { db ->
            val reference = HashMap<String, String>()
            val lock = Any()
            val keys = (0 until 8).map { "k$it" }
            val deadline = System.nanoTime() + 500_000_000L
            val isBeforeDeadline = { System.nanoTime() - deadline < 0 }

            val merger = { while (isBeforeDeadline()) db.merge() }
            val workers =
                (0 until 4).map { workerId ->
                    {
                        // A small LCG, as in the Rust test.
                        var state = -0x61c8864680b583ebL + workerId // 0x9E3779B97F4A7C15 + workerId
                        while (isBeforeDeadline()) {
                            state = state * 6364136223846793005L + 1
                            val key = keys[java.lang.Long.remainderUnsigned(state, keys.size.toLong()).toInt()]
                            val op = (state ushr 32) % 3
                            // Hold the reference lock across both the real mutation and the model update.
                            synchronized(lock) {
                                when (op) {
                                    0L -> {
                                        val value = "v${java.lang.Long.toUnsignedString(state)}"
                                        db.put(b(key), b(value))
                                        reference[key] = value
                                    }
                                    1L -> {
                                        db.delete(b(key))
                                        reference.remove(key)
                                    }
                                    else -> db.get(b(key)) // extra concurrent read pressure
                                }
                            }
                        }
                    }
                }
            runConcurrently(workers + merger)

            synchronized(lock) {
                for (key in keys) assertEquals(reference[key], db.get(b(key))?.let { String(it) }, "mismatch for $key")
            }
        }
    }

    /**
     * A data file can exist under the very id the writer would use next. The
     * merge takes it as input, so it must not also write its output there.
     */
    @Test
    fun `merge output never shares an id with an input file`() {
        val check = { db: Engine -> for (i in 0 until 10) assertArrayEquals(b("v"), db.get(b("k$i"))) }
        Engine.open(dir).use { db ->
            for (i in 0 until 10) db.put(b("k$i"), b("v"))
            // The puts are in file 0; the writer's next file would be 1.
            Files.write(dataPath(dir, 1), ByteArray(0))
            db.merge()
            check(db)
        }
        Engine.open(dir).use(check)
    }
}
