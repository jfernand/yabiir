package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.dataPath
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** A port of `crates/yabiir/src/engine/tests/races.rs`. */
class RacesTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = s.toByteArray()

    private fun open() = Engine.open(dir)

    /**
     * `get` looks `K` up, then — before it reads the value — a whole `merge`
     * runs, repointing `K` and removing the file the lookup returned.
     */
    @Test
    fun `race get survives merge removing its file between lookup and read`() {
        storeForMergeRace(dir).use { db ->
            val value =
                db.readWithHook(b("K")) { entry ->
                    db.merge()
                    // The precondition of the race, checked so the test can't pass vacuously.
                    assertFalse(Files.exists(dataPath(dir, entry.fileId)), "merge should have removed file ${entry.fileId}")
                }
            assertArrayEquals(b("v"), value?.value)
        }
    }

    /** The same race for `fold`: the snapshot is taken, then a whole `merge` runs before any value is read. */
    @Test
    fun `race fold survives merge removing files after its snapshot`() {
        storeForMergeRace(dir).use { db ->
            val rows =
                db.foldWithHook(
                    mutableListOf<Pair<String, String>>(),
                    { key, value, rows -> rows.apply { add(String(key) to String(value)) } },
                ) { db.merge() }
            assertEquals(listOf("K" to "v", "other" to "x"), rows.sortedBy { it.first })
        }
    }

    /**
     * `put` appends `N`, and before it publishes `N`, a merge tries to run.
     * The publish happens on the writer thread, which a merge needs to
     * start, so the merge waits.
     */
    @Test
    fun `race put survives merge between append and publish`() {
        storeForMergeRace(dir).use { db ->
            val window = mergeInsideWindow(db)
            db.putWithHook(b("N"), b("new"), nowUnix(), window.hook)
            window.awaitDone()
            assertArrayEquals(b("new"), db.get(b("N")))
        }
        open().use { assertArrayEquals(b("new"), it.get(b("N"))) }
    }

    /** The same window for `delete`: after a reopen, `K` must not be back. */
    @Test
    fun `race delete survives merge between append and publish`() {
        storeForMergeRace(dir).use { db ->
            val window = mergeInsideWindow(db)
            db.deleteWithHook(b("K"), nowUnix(), window.hook)
            window.awaitDone()
            assertNull(db.get(b("K")))
        }
        open().use { assertNull(it.get(b("K"))) }
    }

    /**
     * Two puts of the same key, the second one entirely inside the first's
     * append-to-publish window. The log holds `v1` then `v2`, so `v2` is
     * the last write, and the keydir must agree.
     */
    @Test
    fun `race two puts publish in log order`() {
        val inSession: ByteArray?
        open().use { db ->
            val window = writeInsideWindow { db.put(b("K"), b("v2"), nowUnix()) }
            db.putWithHook(b("K"), b("v1"), nowUnix(), window.hook)
            // The second put may still be in flight: a write after it settles it.
            db.sync()
            window.awaitDone()
            inSession = db.get(b("K"))
        }
        val afterReopen = open().use { it.get(b("K")) }
        assertArrayEquals(b("v2"), afterReopen, "the log's last write")
        assertArrayEquals(afterReopen, inSession, "the keydir must agree with the log")
    }

    /** The same window, with a delete as the second write: the key stays gone. */
    @Test
    fun `race put then delete publish in log order`() {
        val inSession: ByteArray?
        open().use { db ->
            db.put(b("K"), b("v0"), nowUnix())
            val window = writeInsideWindow { db.delete(b("K"), nowUnix()) }
            db.putWithHook(b("K"), b("v1"), nowUnix(), window.hook)
            db.sync()
            window.awaitDone()
            inSession = db.get(b("K"))
        }
        val afterReopen = open().use { it.get(b("K")) }
        assertNull(afterReopen, "the log's last write is the tombstone")
        assertArrayEquals(afterReopen, inSession, "the keydir must agree with the log")
    }

    /**
     * Merge is paused just before it repoints `K` to the copy it made;
     * another thread overwrites `K`; merge resumes. Its repoint must lose to
     * the write. The pause is a hook, not a sleep.
     */
    @Test
    fun `race concurrent put during merge does not lose the write`() {
        // every put rotates into its own file
        Engine.open(dir, Options(maxFileSize = 1)).use { db ->
            db.put(b("K"), b("old"), nowUnix()) // lands in its own already-rotated-out file

            val paused = CountDownLatch(1)
            val resume = CountDownLatch(1)
            val executor = Executors.newSingleThreadExecutor()
            try {
                val merging =
                    executor.submit {
                        db.mergeWithHook { key ->
                            // runs when K is about to be repointed, after it has been copied forward
                            if (key.contentEquals(b("K"))) {
                                paused.countDown()
                                resume.await()
                            }
                        }
                    }
                // merge has copied K forward, about to repoint
                check(paused.await(10, TimeUnit.SECONDS)) { "merge never reached K" }
                db.put(b("K"), b("new"), nowUnix()) // race: overwrite K while merge is paused
                resume.countDown() // let merge's (now-stale) CAS attempt run
                merging.get(10, TimeUnit.SECONDS)
            } finally {
                executor.shutdownNow()
            }
            assertArrayEquals(b("new"), db.get(b("K")))
        }
    }

    /**
     * A write that lands while a merge is running must still be the last
     * write after a reopen: the file it went to has to sort after every
     * file the merge wrote.
     */
    @Test
    fun `race put during merge is still the last write after reopen`() {
        storeForMergeRace(dir).use { db ->
            db.mergeWithHook { key ->
                if (key.contentEquals(b("K"))) {
                    // merge has copied K forward and is about to repoint it
                    db.put(b("K"), b("new"), nowUnix())
                }
            }
            assertArrayEquals(b("new"), db.get(b("K")))
        }
        open().use {
            assertArrayEquals(b("new"), it.get(b("K")), "the reopened store must not go back to the value merge copied")
        }
    }

    /** The same, with a delete: the key must stay deleted after a reopen. */
    @Test
    fun `race delete during merge stays deleted after reopen`() {
        storeForMergeRace(dir).use { db ->
            db.mergeWithHook { key ->
                if (key.contentEquals(b("K"))) db.delete(b("K"), nowUnix())
            }
            assertNull(db.get(b("K")))
        }
        open().use { assertNull(it.get(b("K")), "the reopened store must not bring back the value merge copied") }
    }
}
