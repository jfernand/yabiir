package io.github.jfernand.yabiir.keydir

import io.github.jfernand.yabiir.Bound
import io.github.jfernand.yabiir.KeyOrder
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.util.concurrent.atomic.AtomicReference

/** Ports of `crates/yabiir/src/keydir/tests.rs`. */
class KeydirTest {
    private fun b(s: String) = s.toByteArray()

    private fun entry(n: Int) = KeydirEntry(fileId = n.toLong(), valueSize = n.toLong(), valuePos = n.toLong(), timestamp = n, version = n.toLong())

    private fun sortedWith(vararg keys: String): Keydir {
        val kd = Keydir(KeyOrder.SORTED)
        for ((i, k) in keys.withIndex()) kd.insert(b(k), entry(i))
        return kd
    }

    private fun keysOf(entries: Map<Key, KeydirEntry>): List<String> = entries.keys.map { String(it.bytes) }

    @Test
    fun `both orders agree on every point operation`() {
        for (order in listOf(KeyOrder.HASHED, KeyOrder.SORTED)) {
            val kd = Keydir(order)
            assertEquals(order, kd.order)
            kd.insert(b("a"), entry(1))
            kd.insert(b("b"), entry(2))
            kd.insert(b("a"), entry(3)) // overwrite
            assertEquals(entry(3), kd[b("a")], "$order")
            assertEquals(2, kd.size, "$order")
            assertFalse(kd.casRepoint(b("a"), entry(1), entry(9)), "$order")
            assertTrue(kd.casRepoint(b("a"), entry(3), entry(9)), "$order")
            assertEquals(entry(9), kd[b("a")], "$order")
            assertEquals(entry(2), kd.remove(b("b")), "$order")
            assertNull(kd[b("b")], "$order")
            assertEquals(1, kd.size, "$order")
        }
    }

    @Test
    fun `a sorted keydir iterates in key order`() {
        val kd = sortedWith("c64", "apple-ii", "zx-spectrum", "bbc-micro", "ibm-pc")
        assertEquals(
            listOf("apple-ii", "bbc-micro", "c64", "ibm-pc", "zx-spectrum"),
            kd.entries().map { String(it.key.bytes) },
        )
    }

    @Test
    fun `range honours each kind of bound in key order`() {
        val kd = sortedWith("a", "b", "c", "d", "e")
        fun r(s: Bound, e: Bound) = keysOf(kd.range(s, e)!!)
        fun inc(k: String) = Bound.Included(b(k))
        fun exc(k: String) = Bound.Excluded(b(k))
        assertEquals(listOf("b", "c", "d"), r(inc("b"), inc("d")))
        assertEquals(listOf("b", "c"), r(inc("b"), exc("d")))
        assertEquals(listOf("c", "d"), r(exc("b"), inc("d")))
        assertEquals(listOf("a", "b"), r(Bound.Unbounded, exc("c")))
        assertEquals(listOf("d", "e"), r(inc("d"), Bound.Unbounded))
        // Bounds between stored keys still select the keys between them.
        assertEquals(listOf("c", "d"), r(inc("bb"), inc("dd")))
    }

    @Test
    fun `range with bounds that select nothing is empty rather than a panic`() {
        // TreeMap.subMap itself throws on these.
        val kd = sortedWith("a", "b", "c")
        for ((s, e) in listOf<Pair<Bound, Bound>>(
            Bound.Included(b("c")) to Bound.Included(b("a")),
            Bound.Excluded(b("b")) to Bound.Excluded(b("b")),
            Bound.Included(b("b")) to Bound.Excluded(b("b")),
            Bound.Excluded(b("b")) to Bound.Included(b("b")),
        )) {
            assertEquals(0, kd.range(s, e)!!.size)
        }
        assertEquals(1, kd.range(Bound.Included(b("b")), Bound.Included(b("b")))!!.size)
    }

    @Test
    fun `a hashed keydir has no range`() {
        val kd = Keydir()
        kd.insert(b("a"), entry(1))
        assertNull(kd.range(Bound.Unbounded, Bound.Unbounded))
    }

    @Test
    fun `insert then get returns same entry`() {
        val kd = Keydir()
        kd.insert(b("k"), entry(1))
        assertEquals(entry(1), kd[b("k")])
    }

    @Test
    fun `insert twice is last write wins`() {
        val kd = Keydir()
        kd.insert(b("k"), entry(1))
        kd.insert(b("k"), entry(2))
        assertEquals(entry(2), kd[b("k")])
        assertEquals(1, kd.size)
    }

    @Test
    fun `remove then get returns none`() {
        val kd = Keydir()
        kd.insert(b("k"), entry(1))
        assertEquals(entry(1), kd.remove(b("k")))
        assertNull(kd[b("k")])
        assertEquals(0, kd.size)
    }

    @Test
    fun `remove missing key returns none`() {
        val kd = Keydir()
        assertNull(kd.remove(b("missing")))
    }

    @Test
    fun `cas repoint succeeds when expectation matches`() {
        val kd = Keydir()
        kd.insert(b("k"), entry(1))
        assertTrue(kd.casRepoint(b("k"), entry(1), entry(2)))
        assertEquals(entry(2), kd[b("k")])
    }

    @Test
    fun `cas repoint fails when expectation is stale`() {
        val kd = Keydir()
        kd.insert(b("k"), entry(1))
        kd.insert(b("k"), entry(2)) // a "concurrent write" raced ahead
        assertFalse(kd.casRepoint(b("k"), entry(1), entry(3)))
        // the stale CAS must not have mutated anything
        assertEquals(entry(2), kd[b("k")])
    }

    @Test
    fun `cas repoint fails when key is absent`() {
        val kd = Keydir()
        assertFalse(kd.casRepoint(b("missing"), entry(1), entry(2)))
        assertNull(kd[b("missing")])
    }

    @Test
    fun `iter yields all entries`() {
        val kd = Keydir()
        kd.insert(b("a"), entry(1))
        kd.insert(b("b"), entry(2))
        val seen = kd.entries().map { String(it.key.bytes) to it.value }.sortedBy { it.first }
        assertEquals(listOf("a" to entry(1), "b" to entry(2)), seen)
    }

    /** Rust's `SharedKeydir::snapshot` is, in Kotlin, a copy of the entries taken under the read lock. */
    @Test
    fun `snapshot matches iter`() {
        val kd = Keydir()
        kd.insert(b("a"), entry(1))
        kd.insert(b("b"), entry(2))
        val shared = SharedKeydir(kd)

        val snap = shared.withRead { k -> k.entries().map { String(it.key.bytes) to it.value } }.sortedBy { it.first }
        assertEquals(listOf("a" to entry(1), "b" to entry(2)), snap)
    }

    /**
     * N reader threads calling `get` in a loop while 1 writer thread
     * inserts/removes the same key for a fixed duration. Every value a
     * reader observes must be exactly the sentinel entry the writer always
     * uses — never a torn/partial/garbage struct — and nothing should
     * throw or deadlock.
     */
    @Test
    fun `concurrent reads and writes stay consistent`() {
        val keydir = SharedKeydir(Keydir())
        val key = b("shared-key")
        val sentinel = entry(42)
        val deadline = System.nanoTime() + 200_000_000L
        val failure = AtomicReference<Throwable?>()

        fun spawn(body: () -> Unit) =
            Thread {
                try {
                    body()
                } catch (e: Throwable) {
                    failure.compareAndSet(null, e)
                }
            }.also { it.start() }

        val writer =
            spawn {
                while (System.nanoTime() < deadline) {
                    keydir.withWrite { it.insert(key, sentinel) }
                    keydir.withWrite { it.remove(key) }
                }
            }
        val readers =
            (0 until 4).map {
                spawn {
                    while (System.nanoTime() < deadline) {
                        keydir[key]?.let { observed -> assertEquals(sentinel, observed) }
                    }
                }
            }

        writer.join()
        readers.forEach { it.join() }
        failure.get()?.let { throw it }
    }

    @Test
    fun `shared keydir len and is empty reflect contents`() {
        val shared = SharedKeydir(Keydir())
        assertEquals(0, shared.withRead { it.size })

        shared.withWrite { it.insert(b("k"), entry(1)) }
        assertEquals(1, shared.withRead { it.size })

        shared.withWrite { it.remove(b("k")) }
        assertEquals(0, shared.withRead { it.size })
    }
}
