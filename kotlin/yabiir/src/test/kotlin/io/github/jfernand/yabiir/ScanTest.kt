package io.github.jfernand.yabiir

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path

/** A port of `crates/yabiir/src/engine/tests/scan.rs`. */
class ScanTest {
    @TempDir
    lateinit var dir: Path

    private fun b(s: String) = bytesOf(s)

    private val sorted = Options(keyOrder = KeyOrder.SORTED)

    private fun computers(db: Engine) {
        for ((k, v) in listOf(
            "c64" to "1982",
            "apple-ii" to "1977",
            "zx-spectrum" to "1982",
            "bbc-micro" to "1981",
            "ibm-pc" to "1981",
        )) {
            db.put(b(k), b(v))
        }
    }

    private fun rowsOf(rows: List<Pair<ByteArray, ByteArray>>) = rows.map { (k, v) -> k.utf8() to v.utf8() }

    private fun keysOf(rows: List<Pair<ByteArray, *>>) = rows.map { it.first.utf8() }

    @Test
    fun `scan returns the keys in range in order with their values`() {
        openEngine(dir, sorted.copy(maxFileSize = 1)).use { db -> // every put in its own file
            computers(db)

            val rows = db.scan(Bound.Included(b("b")), Bound.Excluded(b("j")))
            assertEquals(listOf("bbc-micro", "c64", "ibm-pc"), keysOf(rows))
            assertArrayEquals(b("1982"), rows[1].second)

            // After a merge has moved every value into new files, the same answer.
            db.merge()
            assertEquals(rowsOf(rows), rowsOf(db.scan(Bound.Included(b("b")), Bound.Excluded(b("j")))))
            // And listKeys follows the keydir's order too.
            assertEquals(
                listOf("apple-ii", "bbc-micro", "c64", "ibm-pc", "zx-spectrum"),
                db.listKeys().map { it.utf8() },
            )
        }
    }

    @Test
    fun `scan on a hashed store is an unordered error`() {
        openEngine(dir).use { db -> // Options(): KeyOrder.HASHED
            computers(db)
            assertThrows<YabiirException.Unordered> { db.scan(Bound.Unbounded, Bound.Unbounded) }
        }
    }

    @Test
    fun `the same store opens with either order`() {
        openEngine(dir).use { hashed -> computers(hashed) }

        openEngine(dir, sorted).use { s ->
            assertEquals(
                listOf("apple-ii", "bbc-micro", "c64", "ibm-pc", "zx-spectrum"),
                keysOf(s.scan(Bound.Unbounded, Bound.Unbounded)),
            )
            s.put(b("amiga"), b("1985"))
        }

        openEngine(dir).use { hashed ->
            assertArrayEquals(b("1985"), hashed.get(b("amiga")))
            assertEquals(6, hashed.listKeys().size)
        }
    }

    /** Every key between [start] and [end], a page of [limit] at a time. */
    private fun allPages(db: Engine, start: Bound, end: Bound, direction: Direction, limit: Int): List<List<String>> {
        val pages = ArrayList<List<String>>()
        var after: ByteArray? = null
        while (true) {
            val a = after
            val (s, e) =
                when {
                    a == null -> start to end
                    direction == Direction.FORWARD -> Bound.Excluded(a) to end
                    else -> start to Bound.Excluded(a)
                }
            val page = db.scanPage(s, e, direction, limit)
            val isLast = page.size < limit
            after = page.lastOrNull()?.first
            pages.add(keysOf(page))
            if (isLast) return pages
        }
    }

    @Test
    fun `scan page pages through a range forward and in reverse`() {
        openEngine(dir, sorted).use { db ->
            computers(db)

            assertEquals(
                listOf(listOf("bbc-micro", "c64"), listOf("ibm-pc")),
                allPages(db, Bound.Included(b("b")), Bound.Excluded(b("z")), Direction.FORWARD, 2),
            )
            assertEquals(
                listOf(listOf("zx-spectrum", "ibm-pc"), listOf("c64", "bbc-micro"), listOf("apple-ii")),
                allPages(db, Bound.Unbounded, Bound.Unbounded, Direction.REVERSE, 2),
            )
            // An exact multiple of the limit ends with an empty page.
            assertEquals(
                listOf(listOf("apple-ii", "bbc-micro", "c64"), listOf()),
                allPages(db, Bound.Unbounded, Bound.Excluded(b("d")), Direction.FORWARD, 3),
            )
        }
    }

    @Test
    fun `scan page returns values and versions`() {
        openEngine(dir, sorted).use { db ->
            computers(db)
            db.put(b("c64"), b("1982!"))
            val page = db.scanPage(Bound.Included(b("c64")), Bound.Included(b("c64")), Direction.FORWARD, 10)
            assertEquals(1, page.size)
            assertEquals(db.getVersioned(b("c64")), page[0].second)
            assertArrayEquals(b("1982!"), page[0].second.value)
        }
    }

    @Test
    fun `scan page needs a sorted keydir and honours a zero limit`() {
        openEngine(dir).use { db ->
            assertThrows<YabiirException.Unordered> {
                db.scanPage(Bound.Unbounded, Bound.Unbounded, Direction.FORWARD, 1)
            }
        }
        openEngine(dir, sorted).use { db ->
            computers(db)
            assertTrue(db.scanPage(Bound.Unbounded, Bound.Unbounded, Direction.REVERSE, 0).isEmpty())
        }
    }
}
