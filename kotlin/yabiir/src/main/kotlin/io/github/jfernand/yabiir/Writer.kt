/**
 * The single writer: one thread, the only one that appends to the log. A
 * port of `crates/yabiir/src/writer.rs`, whose module docs explain the
 * design.
 *
 * Every write is a batch sent to the writer thread. It takes whatever
 * batches are queued as one group and, for the whole group: checks each
 * batch's conditions against the keydir and the batches before it; appends
 * each batch that passes as one record; hands the records to the OS in one
 * `write` (and with `shouldSyncOnPut`, one fsync); publishes them to the
 * keydir in log order under one lock; then completes each batch's commit.
 */
package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.ActiveFile
import io.github.jfernand.yabiir.datafile.DataFileSet
import io.github.jfernand.yabiir.datafile.MAX_FILE_ID
import io.github.jfernand.yabiir.datafile.discover
import io.github.jfernand.yabiir.format.FILE_HEADER_SIZE
import io.github.jfernand.yabiir.format.OP_HEADER_SIZE
import io.github.jfernand.yabiir.format.OpRef
import io.github.jfernand.yabiir.format.RECORD_HEADER_SIZE
import io.github.jfernand.yabiir.format.encodeRecord
import io.github.jfernand.yabiir.keydir.Key
import io.github.jfernand.yabiir.keydir.Keydir
import io.github.jfernand.yabiir.keydir.KeydirEntry
import io.github.jfernand.yabiir.keydir.SharedKeydir
import java.io.IOException
import java.util.ArrayDeque
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.atomic.AtomicInteger

/** A batch on its way to the writer. */
internal class Batch(
    val ops: List<Op>,
    val timestamp: Int,
    val done: CompletableFuture<Long?>,
    /** Run on the writer thread after the record is written and before it is published. Tests only. */
    var betweenAppendAndPublish: (() -> Unit)? = null,
)

/** What a merge may read and write, fixed at the instant it starts. */
internal class MergeScope(
    /** Every data file there was when the merge started, ascending. */
    val inputIds: List<Long>,
    /** The ids reserved for the merge's output: `[firstOutputId, endOfOutputIds)`. */
    val firstOutputId: Long,
    val endOfOutputIds: Long,
    /** The sequence number the next write will get; the output files' floor. */
    val seqFloor: Long,
)

private sealed interface Command {
    class Write(val batch: Batch) : Command

    class Sync(val reply: CompletableFuture<Unit>) : Command

    class Seal(val reply: CompletableFuture<MergeScope?>) : Command

    data object Stop : Command
}

/** The handle on the writer thread. */
internal class Writer(
    files: DataFileSet,
    keydir: SharedKeydir,
    nextFileId: Long,
    nextSeq: Long,
    val maxFileSize: Long,
    shouldSyncOnWrite: Boolean,
) {
    private val queue = LinkedBlockingQueue<Command>()
    private val stopLock = Any()
    private var isStopped = false
    private val worker = Worker(files, keydir, queue, nextFileId, nextSeq, maxFileSize, shouldSyncOnWrite)
    private val thread = Thread(worker::run, "yabiir-writer").apply { start() }

    val fsyncCallCount: Int get() = worker.nFsyncs.get()

    /** Queue [batch]. Its future completes once the writer has decided it, or with an error if the writer is gone. */
    fun submit(batch: Batch) {
        if (!send(Command.Write(batch))) batch.done.completeExceptionally(writerGone())
    }

    /** Flush and fsync the active file. Also fails if the fsync of a file closed since the last sync did. */
    fun sync() {
        ask<Unit> { Command.Sync(it) }
    }

    /** Close the active file and reserve ids for a merge's output. `null` if there are no data files. */
    fun sealForMerge(): MergeScope? = ask { Command.Seal(it) }

    /** [sync], then stop the thread. */
    fun close() {
        try {
            sync()
        } finally {
            stop()
        }
    }

    /** Stop the thread once it has carried out everything sent before, without an fsync. */
    fun stop() {
        synchronized(stopLock) {
            if (!isStopped) {
                isStopped = true
                queue.add(Command.Stop)
            }
        }
        if (Thread.currentThread() != thread) thread.join()
    }

    private fun send(command: Command): Boolean =
        synchronized(stopLock) {
            if (isStopped || !thread.isAlive) return false
            queue.add(command)
            true
        }

    private fun <T> ask(make: (CompletableFuture<T>) -> Command): T {
        val reply = CompletableFuture<T>()
        if (!send(make(reply))) throw writerGone()
        try {
            return reply.get()
        } catch (e: ExecutionException) {
            throw e.cause as? YabiirException ?: YabiirException.Io(IOException(e.cause))
        }
    }
}

private fun writerGone() = YabiirException.Io(IOException("the writer thread has stopped"))

/** A batch of the group being written, after its conditions held. `record` is `null` if it had nothing to write. */
private class Written(val batch: Batch, val record: RecordAt?)

private class RecordAt(val seq: Long, val fileId: Long, val valuePositions: List<Long?>)

private class Worker(
    private val files: DataFileSet,
    private val keydir: SharedKeydir,
    private val queue: LinkedBlockingQueue<Command>,
    private var nextFileId: Long,
    private var nextSeq: Long,
    private val maxFileSize: Long,
    private val shouldSyncOnWrite: Boolean,
) {
    val nFsyncs = AtomicInteger()
    private val postponed = ArrayDeque<Command>()
    private var active: ActiveFile? = null
    private var hasUnreportedSyncFailure = false

    fun run() {
        try {
            while (true) {
                when (val command = nextCommand()) {
                    is Command.Write -> writeGroup(command.batch)
                    is Command.Sync -> reply(command.reply) { sync() }
                    is Command.Seal -> reply(command.reply) { sealForMerge() }
                    Command.Stop -> break
                }
            }
        } finally {
            // Every write so far was handed to the OS when its group ended.
            active?.let { runCatching { it.close() } }
            active = null
            // Anything still queued (only after a crash of this thread) fails.
            val left = ArrayList<Command>(postponed).also { queue.drainTo(it) }
            for (command in left) {
                when (command) {
                    is Command.Write -> command.batch.done.completeExceptionally(writerGone())
                    is Command.Sync -> command.reply.completeExceptionally(writerGone())
                    is Command.Seal -> command.reply.completeExceptionally(writerGone())
                    Command.Stop -> {}
                }
            }
        }
    }

    private fun <T> reply(reply: CompletableFuture<T>, block: () -> T) {
        try {
            reply.complete(block())
        } catch (e: YabiirException) {
            reply.completeExceptionally(e)
        } catch (e: IOException) {
            reply.completeExceptionally(YabiirException.Io(e))
        }
    }

    /** The next command, spinning briefly before waiting for one. */
    private fun nextCommand(): Command {
        postponed.pollFirst()?.let { return it }
        val spinUntil = System.nanoTime() + SPIN_BEFORE_PARKING_NANOS
        while (System.nanoTime() < spinUntil) {
            queue.poll()?.let { return it }
            Thread.onSpinWait()
        }
        return queue.take()
    }

    private fun tryNextCommand(): Command? = postponed.pollFirst() ?: queue.poll()

    private fun writeGroup(first: Batch) {
        val overlay = Overlay(keydir)
        val written = ArrayList<Written>()
        var next: Batch? = first
        while (next != null) {
            val batch = next
            next = null
            val failedAt = overlay.check(batch.ops)
            if (failedAt != null) {
                batch.done.completeExceptionally(YabiirException.ConditionFailed(failedAt))
            } else {
                val ops = overlay.effectiveOps(batch.ops)
                if (ops.isEmpty()) {
                    written.add(Written(batch, null))
                } else {
                    try {
                        val record = append(ops, batch.timestamp)
                        overlay.apply(ops, record.seq)
                        written.add(Written(batch, record))
                    } catch (e: IOException) {
                        // The file may now end in part of a record: nothing more goes into it,
                        // and no batch appended to it with this one is published.
                        dropActive()
                        batch.done.completeExceptionally(YabiirException.Io(e))
                        failAll(written, "a write before this one in its group failed")
                        break
                    }
                }
            }
            if (isActiveFull()) break
            when (val command = tryNextCommand()) {
                is Command.Write -> next = command.batch
                null -> {}
                else -> postponed.addFirst(command)
            }
        }
        try {
            makeGroupDurable(written)
        } catch (e: IOException) {
            for (w in written) w.batch.done.completeExceptionally(YabiirException.Io(IOException(e.message, e)))
            return
        }
        publish(written)
        if (isActiveFull()) runCatching { retireActive() }
    }

    private fun append(ops: List<OpRef>, timestamp: Int): RecordAt {
        val seq = nextSeq
        // Spent whether or not the append succeeds: the record may reach the file in part.
        nextSeq += 1
        val encoded = encodeRecord(seq, timestamp, ops)
        val file = activeFile()
        val recordPos = file.appendBuffered(encoded.bytes)
        val positions = ops.mapIndexed { i, op -> if (op.tombstone) null else recordPos + encoded.valueOffset(i) }
        return RecordAt(seq, file.fileId, positions)
    }

    private fun makeGroupDurable(written: List<Written>) {
        if (written.none { it.record != null }) return
        val file = active ?: return
        try {
            if (shouldSyncOnWrite) {
                nFsyncs.incrementAndGet()
                file.sync()
            } else {
                file.flush()
            }
        } catch (e: IOException) {
            dropActive()
            throw e
        }
    }

    private fun publish(written: List<Written>) {
        for (w in written) {
            w.batch.betweenAppendAndPublish?.let {
                w.batch.betweenAppendAndPublish = null
                it()
            }
        }
        keydir.withWrite { kd ->
            for (w in written) w.record?.let { publishBatch(kd, w.batch, it) }
        }
        for (w in written) w.batch.done.complete(w.record?.seq)
    }

    fun sync() {
        try {
            active?.sync()
        } catch (e: IOException) {
            dropActive()
            hasUnreportedSyncFailure = false
            throw e
        }
        if (hasUnreportedSyncFailure) {
            hasUnreportedSyncFailure = false
            throw IOException("the fsync of a data file closed earlier failed; its writes may not be durable")
        }
    }

    fun sealForMerge(): MergeScope? {
        retireActive()
        val inputIds = discover(files.dir)
        val newestInputId = inputIds.lastOrNull() ?: return null
        val inputBytes = inputIds.sumOf { files.lenOf(it) }
        val maxOutputFiles = maxOutputFiles(inputBytes, maxFileSize)
        val firstOutputId = maxOf(nextFileId, newestInputId + 1)
        val end = firstOutputId + maxOutputFiles
        if (maxOutputFiles > MAX_FILE_ID || end > MAX_FILE_ID) throw outOfFileIds()
        nextFileId = end
        return MergeScope(inputIds, firstOutputId, end, nextSeq)
    }

    private fun isActiveFull(): Boolean = active?.let { it.len >= maxFileSize } ?: false

    /** The active file, created first if there is none. */
    private fun activeFile(): ActiveFile {
        active?.let { return it }
        val fileId = nextFileId
        // Spent whether or not what follows succeeds: its file may be left behind.
        if (fileId >= MAX_FILE_ID) throw outOfFileIds()
        nextFileId = fileId + 1
        val created = files.createActive(fileId, nextSeq)
        active = created
        // The file's name has to be durable before any record in it is.
        files.syncDir()
        return created
    }

    /** Stop appending to the active file: fsync and close it. Whatever the fsync's result, it is closed. */
    private fun retireActive() {
        val file = active ?: return
        active = null
        try {
            file.sync()
        } catch (e: IOException) {
            hasUnreportedSyncFailure = true
            throw e
        } finally {
            runCatching { file.close() }
        }
    }

    private fun dropActive() {
        active?.let { runCatching { it.close() } }
        active = null
    }
}

private fun outOfFileIds() = IOException("the store has used every data file id")

private fun failAll(written: MutableList<Written>, reason: String) {
    for (w in written) w.batch.done.completeExceptionally(YabiirException.Io(IOException(reason)))
    written.clear()
}

/** Point the keydir at the operations of [batch], whose record is [record]. */
private fun publishBatch(keydir: Keydir, batch: Batch, record: RecordAt) {
    val positions = record.valuePositions.filterNotNull().iterator()
    for (op in batch.ops) {
        when (op) {
            is Op.Check -> {}
            is Op.Delete -> keydir.remove(op.key)
            is Op.Put ->
                keydir.insert(
                    op.key,
                    KeydirEntry(record.fileId, op.value.size.toLong(), positions.next(), batch.timestamp, record.seq),
                )
        }
    }
}

/**
 * How many files a merge of [inputBytes] of data files can write, at most:
 * see `max_output_files` in `writer.rs`.
 */
internal fun maxOutputFiles(inputBytes: Long, maxFileSize: Long): Long {
    val minOpLen = OP_HEADER_SIZE + 1L
    val maxOps = inputBytes / minOpLen + 1
    val header = FILE_HEADER_SIZE.toLong()
    val bySize =
        if (maxFileSize > header) {
            val recordBytes = inputBytes + maxOps * RECORD_HEADER_SIZE
            Math.ceilDiv(recordBytes + maxFileSize, maxFileSize - header)
        } else {
            Long.MAX_VALUE
        }
    return minOf(maxOps, bySize) + 1
}

/**
 * The versions keys will have once the group being written is published:
 * the keydir's, as changed by the group's batches so far.
 */
private class Overlay(private val keydir: SharedKeydir) {
    private val versions = HashMap<Key, Long?>()

    fun version(key: ByteArray): Long? {
        val k = Key(key)
        return if (versions.containsKey(k)) versions[k] else keydir[key]?.version
    }

    /** The index of the first check of [ops] that doesn't hold, or `null`. */
    fun check(ops: List<Op>): Int? {
        for ((i, op) in ops.withIndex()) {
            if (op is Op.Check && !op.cond.holds(version(op.key))) return i
        }
        return null
    }

    /** Every put, and every delete of a key that has a value at that point of the batch. */
    fun effectiveOps(ops: List<Op>): List<OpRef> {
        val within = HashMap<Key, Boolean>()
        val effective = ArrayList<OpRef>()
        for (op in ops) {
            when (op) {
                is Op.Check -> {}
                is Op.Put -> {
                    within[Key(op.key)] = true
                    effective.add(OpRef(op.key, op.value, false))
                }
                is Op.Delete -> {
                    val k = Key(op.key)
                    val isPresent = within[k] ?: (version(op.key) != null)
                    if (isPresent) {
                        within[k] = false
                        effective.add(OpRef(op.key, ByteArray(0), true))
                    }
                }
            }
        }
        return effective
    }

    fun apply(ops: List<OpRef>, seq: Long) {
        for (op in ops) versions[Key(op.key)] = if (op.tombstone) null else seq
    }
}
