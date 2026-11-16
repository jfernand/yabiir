package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.DataFileSet
import io.github.jfernand.yabiir.datafile.MAX_FILE_ID
import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.datafile.preadExact
import io.github.jfernand.yabiir.format.MAX_KEY_LEN
import io.github.jfernand.yabiir.format.MAX_VALUE_LEN
import io.github.jfernand.yabiir.format.OP_HEADER_SIZE
import io.github.jfernand.yabiir.keydir.Key
import io.github.jfernand.yabiir.keydir.Keydir
import io.github.jfernand.yabiir.keydir.KeydirEntry
import io.github.jfernand.yabiir.keydir.SharedKeydir
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.time.Duration
import java.util.concurrent.CompletableFuture
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.withLock

/**
 * A Bitcask store open in this process: a keydir and a directory of data
 * files, with one writer thread. A port of the Rust crate's `Engine`,
 * reading and writing the same files.
 */
public class Engine private constructor(
    private val keydir: SharedKeydir,
    private val files: DataFileSet,
    /** `null` for a read-only handle. */
    private val writer: Writer?,
    private val metrics: Metrics,
    private val dirLock: DirLock?,
) : Bitcask {
    private val mergeLock = ReentrantLock()

    /** How this store's keydir keeps its keys, as opened. */
    public val keyOrder: KeyOrder get() = keydir.withRead { it.order }

    private fun writer(): Writer = writer ?: throw YabiirException.ReadOnly()

    // ---- reads ---------------------------------------------------------------

    override fun get(key: ByteArray): ByteArray? = getVersioned(key)?.value

    /**
     * The current value of [key] and its version, or `null`. A version is
     * the sequence number of the write that stored the value: it changes on
     * every put and never repeats, not even after a delete. A value written
     * by yabiir 0.8 or earlier has version 1 until it's next written.
     */
    public fun getVersioned(key: ByteArray): Versioned? = readWithHook(key) {}

    internal fun readWithHook(key: ByteArray, betweenLookupAndRead: (KeydirEntry) -> Unit): Versioned? {
        val start = System.nanoTime()
        val found =
            io {
                keydir.withRead { kd -> kd[key]?.let { it to files.handle(it.fileId) } }
            }
        val value =
            found?.let { (entry, file) ->
                betweenLookupAndRead(entry)
                Versioned(readValue(file, entry), entry.version)
            }
        metrics.recordGet(Duration.ofNanos(System.nanoTime() - start))
        return value
    }

    override fun listKeys(): List<ByteArray> = keydir.withRead { kd -> kd.entries().map { it.key.bytes.copyOf() } }

    override fun <A> fold(init: A, f: (key: ByteArray, value: ByteArray, acc: A) -> A): A = foldWithHook(init, f) {}

    internal fun <A> foldWithHook(init: A, f: (ByteArray, ByteArray, A) -> A, afterSnapshot: () -> Unit): A {
        val pinned = keydir.withRead { kd -> Pinned.pin(files, kd.entries()) }
        afterSnapshot()
        var acc = init
        pinned.read { key, found -> acc = f(key, found.value, acc) }
        return acc
    }

    /**
     * Every live pair whose key lies between [start] and [end], in key
     * order. Needs [KeyOrder.SORTED]; throws [YabiirException.Unordered]
     * otherwise.
     */
    public fun scan(start: Bound, end: Bound): List<Pair<ByteArray, ByteArray>> {
        val pinned =
            keydir.withRead { kd ->
                val range = kd.range(start, end) ?: throw YabiirException.Unordered()
                Pinned.pin(files, range.entries)
            }
        val out = ArrayList<Pair<ByteArray, ByteArray>>()
        pinned.read { key, found -> out.add(key to found.value) }
        return out
    }

    /**
     * One page of a scan: up to [limit] live keys between [start] and
     * [end], with their values and versions, in key order or in reverse.
     * To read the next page, scan again from just past the last key.
     */
    public fun scanPage(start: Bound, end: Bound, direction: Direction, limit: Int): List<Pair<ByteArray, Versioned>> {
        val pinned =
            keydir.withRead { kd ->
                val range = kd.range(start, end) ?: throw YabiirException.Unordered()
                val ordered = if (direction == Direction.FORWARD) range else range.descendingMap()
                Pinned.pin(files, ordered.entries.asSequence().take(limit).toList())
            }
        val out = ArrayList<Pair<ByteArray, Versioned>>()
        pinned.read { key, found -> out.add(key to found) }
        return out
    }

    // ---- writes --------------------------------------------------------------

    /**
     * Apply [ops] atomically: every check is evaluated against the store as
     * it was just before the batch; if one fails, nothing is written and the
     * commit fails with [YabiirException.ConditionFailed]. Otherwise the
     * batch is one record, under one CRC, with one version for every key it
     * puts. The record takes the current time as its timestamp.
     */
    public fun writeBatch(ops: List<Op>): Commit = submit(ops, nowUnix(), null)

    /** [writeBatch] with the record's timestamp given, as [put] and [delete] take one. */
    public fun writeBatchAt(ops: List<Op>, timestamp: Long): Commit = submit(ops, timestamp, null)

    override fun put(key: ByteArray, value: ByteArray, timestamp: Long) {
        putWithHook(key, value, timestamp, null)
    }

    override fun delete(key: ByteArray, timestamp: Long) {
        deleteWithHook(key, timestamp, null)
    }

    internal fun putWithHook(key: ByteArray, value: ByteArray, timestamp: Long, hook: (() -> Unit)?) {
        val start = System.nanoTime()
        submit(listOf(Op.Put(key, value)), timestamp, hook).join()
        metrics.recordPut(Duration.ofNanos(System.nanoTime() - start))
    }

    internal fun deleteWithHook(key: ByteArray, timestamp: Long, hook: (() -> Unit)?) {
        val start = System.nanoTime()
        submit(listOf(Op.Delete(key)), timestamp, hook).join()
        metrics.recordDelete(Duration.ofNanos(System.nanoTime() - start))
    }

    private fun submit(ops: List<Op>, timestamp: Long, hook: (() -> Unit)?): Commit {
        val writer =
            try {
                writer()
            } catch (e: YabiirException) {
                return Commit.failed(e)
            }
        try {
            checkOps(ops)
        } catch (e: YabiirException) {
            return Commit.failed(e)
        }
        require(timestamp in 0..MAX_VALUE_LEN) { "a timestamp must fit in 32 unsigned bits: $timestamp" }
        val done = CompletableFuture<Long?>()
        writer.submit(Batch(ops.toList(), timestamp.toInt(), done, hook))
        return Commit(done)
    }

    // ---- maintenance -----------------------------------------------------------

    override fun merge() {
        mergeWithHook {}
    }

    internal fun mergeWithHook(beforeRepoint: (ByteArray) -> Unit) {
        val writer = writer()
        mergeLock.withLock {
            val start = System.nanoTime()
            io { merge(keydir, files, writer, metrics, beforeRepoint) }
            metrics.recordMerge(Duration.ofNanos(System.nanoTime() - start))
        }
    }

    override fun sync() {
        val writer = writer ?: return
        val start = System.nanoTime()
        writer.sync()
        metrics.recordSync(Duration.ofNanos(System.nanoTime() - start))
    }

    override fun close() {
        try {
            writer?.close()
        } finally {
            release()
        }
    }

    /**
     * Release this handle the way a process that stops does: whatever was
     * written is handed to the OS, but nothing is fsynced. For tests that
     * simulate a crash.
     */
    public fun closeWithoutSync() {
        try {
            writer?.stop()
        } finally {
            release()
        }
    }

    private fun release() {
        files.close()
        dirLock?.close()
    }

    internal val fsyncCallCount: Int get() = writer?.fsyncCallCount ?: 0

    public companion object {
        /**
         * Open the store in [dir], rebuilding the keydir from its files.
         * A read-write open creates the directory if it's missing and takes
         * its lock; a read-only one takes no lock and creates nothing.
         */
        @JvmStatic
        @JvmOverloads
        public fun open(dir: Path, opts: Options = Options()): Engine {
            val dirLock =
                if (opts.isReadWrite) {
                    io { Files.createDirectories(dir) }
                    DirLock.acquire(dir)
                } else {
                    null
                }
            try {
                val fileIds = io { discover(dir) }
                val keydir = Keydir(opts.keyOrder)
                val nextSeq = io { recover(dir, fileIds, keydir) }
                val files = DataFileSet(dir)
                io { files.register(fileIds) }
                val shared = SharedKeydir(keydir)
                val writer =
                    if (opts.isReadWrite) {
                        val nextFileId =
                            when (val last = fileIds.lastOrNull()) {
                                null -> 0L
                                MAX_FILE_ID -> throw YabiirException.Io(IOException("the store has used every data file id"))
                                else -> last + 1
                            }
                        Writer(files, shared, nextFileId, nextSeq, opts.maxFileSize, opts.shouldSyncOnPut)
                    } else {
                        null
                    }
                return Engine(shared, files, writer, opts.metrics ?: object : Metrics {}, dirLock)
            } catch (e: Throwable) {
                dirLock?.close()
                throw e
            }
        }
    }
}

/** Check every key and value of a batch against the format's limits, and the batch against what one record holds. */
private fun checkOps(ops: List<Op>) {
    var bodyLen = 0L
    for (op in ops) {
        val valueLen = if (op is Op.Put) op.value.size.toLong() else 0L
        if (op.key.isEmpty()) throw YabiirException.EmptyKey()
        if (op.key.size > MAX_KEY_LEN) throw YabiirException.KeyTooLarge()
        if (valueLen > MAX_VALUE_LEN) throw YabiirException.ValueTooLarge()
        if (op !is Op.Check) bodyLen += OP_HEADER_SIZE + op.key.size + valueLen
    }
    if (bodyLen > MAX_VALUE_LEN) throw YabiirException.BatchTooLarge()
}

private fun readValue(file: FileChannel, entry: KeydirEntry): ByteArray = io { preadExact(file, entry.valuePos, entry.valueSize) }

/** Keydir entries copied out under the keydir's read lock, with a read handle for every file they point into. */
private class Pinned(private val entries: List<Pair<ByteArray, KeydirEntry>>, private val handles: Map<Long, FileChannel>) {
    fun read(each: (ByteArray, Versioned) -> Unit) {
        for ((key, entry) in entries) each(key, Versioned(readValue(handles.getValue(entry.fileId), entry), entry.version))
    }

    companion object {
        fun pin(files: DataFileSet, entries: Collection<Map.Entry<Key, KeydirEntry>>): Pinned = pin(files, entries.toList())

        fun pin(files: DataFileSet, entries: List<Map.Entry<Key, KeydirEntry>>): Pinned {
            val handles = HashMap<Long, FileChannel>()
            val copied = ArrayList<Pair<ByteArray, KeydirEntry>>(entries.size)
            for ((key, entry) in entries) {
                if (entry.fileId !in handles) handles[entry.fileId] = io { files.handle(entry.fileId) }
                copied.add(key.bytes.copyOf() to entry)
            }
            return Pinned(copied, handles)
        }
    }
}
