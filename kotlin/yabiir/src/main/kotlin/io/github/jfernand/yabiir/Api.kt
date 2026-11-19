package io.github.jfernand.yabiir

import java.nio.file.Path
import java.time.Duration

/** How the keydir keeps its keys. Chosen per open; nothing about it is stored on disk. */
public enum class KeyOrder {
    /** A hash map: the fastest point lookups, keys in no order, no [Engine.scan]. */
    HASHED,

    /** A tree map in unsigned byte order: [Engine.scan] and [Engine.scanPage] work, and keys list in order. */
    SORTED,
}

/** Which way [Engine.scanPage] walks its range. */
public enum class Direction { FORWARD, REVERSE }

/** A value and its version: see [Engine.getVersioned]. */
public class Versioned(public val value: ByteArray, public val version: Long) {
    override fun equals(other: Any?): Boolean =
        other is Versioned && version == other.version && value.contentEquals(other.value)

    override fun hashCode(): Int = value.contentHashCode() * 31 + version.hashCode()

    override fun toString(): String = "Versioned(${value.size} bytes, version=$version)"
}

/** One end of a range of keys. */
public sealed interface Bound {
    public data object Unbounded : Bound

    public class Included(public val key: ByteArray) : Bound

    public class Excluded(public val key: ByteArray) : Bound
}

/** Observes how long calls take. Every method has a no-op default. */
public interface Metrics {
    public fun recordGet(elapsed: Duration) {}

    public fun recordPut(elapsed: Duration) {}

    public fun recordDelete(elapsed: Duration) {}

    public fun recordMerge(elapsed: Duration) {}

    public fun recordSync(elapsed: Duration) {}

    /**
     * How many copies merge repointed at once: called each time it flushes
     * its output, including the last time, when there are any to repoint.
     */
    public fun recordPendingQueueDepth(depth: Int) {}

    public fun recordMergeSummary(inputFiles: Int, entriesCopied: Int) {}
}

/** How to open a store. */
public data class Options(
    /** Open for reading and writing, or read-only. One read-write handle per directory at a time. */
    val isReadWrite: Boolean = true,
    /** Whether a write waits until its record is fsynced, before it returns and before readers see it. */
    val shouldSyncOnPut: Boolean = false,
    /** The size in bytes at which the active file is closed and a new one started. */
    val maxFileSize: Long = 64L * 1024 * 1024,
    val metrics: Metrics? = null,
    val keyOrder: KeyOrder = KeyOrder.HASHED,
) {
    /** These options, for a read-only handle. */
    public fun readOnly(): Options = copy(isReadWrite = false)
}

/**
 * The Bitcask API: a directory of append-only data files and an in-memory
 * index from each live key to its newest value. Mirrors the Rust crate's
 * `Bitcask` trait; see its docs for the details of each call.
 */
public interface Bitcask : AutoCloseable {
    /** The current value of [key], or `null`. */
    public fun get(key: ByteArray): ByteArray?

    /** Store [value] under [key]. [timestamp] is in Unix seconds and must fit in 32 unsigned bits. */
    public fun put(key: ByteArray, value: ByteArray, timestamp: Long = nowUnix())

    /** Remove [key]. Removing a key that has no value is not an error. */
    public fun delete(key: ByteArray, timestamp: Long = nowUnix())

    /** Every live key, as of one instant. */
    public fun listKeys(): List<ByteArray>

    /** Fold [f] over every live pair as of the start of the call. */
    public fun <A> fold(init: A, f: (key: ByteArray, value: ByteArray, acc: A) -> A): A

    /** Compact the store. Safe while other threads read and write. */
    public fun merge()

    /** Make every write so far durable. */
    public fun sync()

    /** Make every write durable and release the directory's lock. */
    override fun close()
}

/** The current time in Unix seconds. */
public fun nowUnix(): Long = System.currentTimeMillis() / 1000

