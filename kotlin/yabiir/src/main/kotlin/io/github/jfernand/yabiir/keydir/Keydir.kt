/** The in-memory index from each live key to where its newest value is. A port of `crates/yabiir/src/keydir`. */
package io.github.jfernand.yabiir.keydir

import io.github.jfernand.yabiir.Bound
import io.github.jfernand.yabiir.KeyOrder
import java.util.Arrays
import java.util.NavigableMap
import java.util.TreeMap
import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write

/** A key: bytes with content equality, ordered as unsigned bytes. */
public class Key(public val bytes: ByteArray) : Comparable<Key> {
    private val hash = bytes.contentHashCode()

    override fun equals(other: Any?): Boolean = other is Key && bytes.contentEquals(other.bytes)

    override fun hashCode(): Int = hash

    override fun compareTo(other: Key): Int = Arrays.compareUnsigned(bytes, other.bytes)

    override fun toString(): String = bytes.joinToString("") { "%02x".format(it) }
}

/** Where a key's newest value is. */
public data class KeydirEntry(
    val fileId: Long,
    /** Unsigned 32 bits, widened. */
    val valueSize: Long,
    val valuePos: Long,
    val timestamp: Int,
    val version: Long,
)

public class Keydir(public val order: KeyOrder = KeyOrder.HASHED) {
    private val map: MutableMap<Key, KeydirEntry> =
        when (order) {
            KeyOrder.HASHED -> HashMap()
            KeyOrder.SORTED -> TreeMap()
        }

    public operator fun get(key: ByteArray): KeydirEntry? = map[Key(key)]

    public fun insert(key: ByteArray, entry: KeydirEntry) {
        map[Key(key)] = entry
    }

    public fun remove(key: ByteArray): KeydirEntry? = map.remove(Key(key))

    public val size: Int get() = map.size

    public fun entries(): Collection<Map.Entry<Key, KeydirEntry>> = map.entries

    /**
     * The entries whose keys lie between [start] and [end], in key order, or
     * `null` for a hashed keydir. A range whose start is past its end is
     * empty.
     */
    public fun range(start: Bound, end: Bound): NavigableMap<Key, KeydirEntry>? {
        val sorted = map as? TreeMap<Key, KeydirEntry> ?: return null
        val s = start.key()
        val e = end.key()
        if (s != null && e != null) {
            val c = s.compareTo(e)
            val isEmpty = if (start is Bound.Included && end is Bound.Included) c > 0 else c >= 0
            if (isEmpty) return TreeMap()
        }
        return when {
            s == null && e == null -> sorted
            s == null -> sorted.headMap(e!!, end is Bound.Included)
            e == null -> sorted.tailMap(s, start is Bound.Included)
            else -> sorted.subMap(s, start is Bound.Included, e, end is Bound.Included)
        }
    }

    /** Point [key] at [new] if it still points at [expectedOld]. */
    public fun casRepoint(key: ByteArray, expectedOld: KeydirEntry, new: KeydirEntry): Boolean {
        val k = Key(key)
        if (map[k] != expectedOld) return false
        map[k] = new
        return true
    }
}

private fun Bound.key(): Key? =
    when (this) {
        Bound.Unbounded -> null
        is Bound.Included -> Key(key)
        is Bound.Excluded -> Key(key)
    }

/** A [Keydir] behind a read-write lock, shared by readers, the writer thread and merge. */
public class SharedKeydir(private val keydir: Keydir) {
    private val lock = ReentrantReadWriteLock()

    public operator fun get(key: ByteArray): KeydirEntry? = lock.read { keydir[key] }

    public fun casRepoint(key: ByteArray, expectedOld: KeydirEntry, new: KeydirEntry): Boolean =
        lock.write { keydir.casRepoint(key, expectedOld, new) }

    public fun <R> withRead(f: (Keydir) -> R): R = lock.read { f(keydir) }

    public fun <R> withWrite(f: (Keydir) -> R): R = lock.write { f(keydir) }
}
