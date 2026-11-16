/**
 * Merge (compaction): a port of `crates/yabiir/src/merge`, whose module docs
 * explain the steps and why they're safe.
 *
 * 1. Seal: on the writer thread, close the active file and reserve the
 *    output's ids, below every file written from then on.
 * 2. Copy: read each input file in order; copy each put the keydir still
 *    points at, as a record of its own with its original sequence number.
 * 3. Repoint: once a copy is flushed, point its key at it, unless a write
 *    replaced the key since.
 * 4. Finish: fsync the output and the directory.
 * 5. Remove the input files, oldest first, up to the first one the keydir
 *    still points into (a damaged file whose live values step 2 couldn't
 *    read): that one and every newer input stay.
 */
package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.ActiveFile
import io.github.jfernand.yabiir.datafile.DataFileSet
import io.github.jfernand.yabiir.datafile.Scan
import io.github.jfernand.yabiir.datafile.Scanned
import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.hintPath
import io.github.jfernand.yabiir.format.HINT_MAGIC
import io.github.jfernand.yabiir.format.HintRecord
import io.github.jfernand.yabiir.format.OpRef
import io.github.jfernand.yabiir.format.Record
import io.github.jfernand.yabiir.format.encodeHintV2
import io.github.jfernand.yabiir.format.encodeRecord
import io.github.jfernand.yabiir.format.putU32
import io.github.jfernand.yabiir.keydir.KeydirEntry
import io.github.jfernand.yabiir.keydir.SharedKeydir
import java.io.BufferedOutputStream
import java.io.OutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.util.zip.CRC32

/** How many entries merge writes between flushes, unless a file fills up first. */
private const val FLUSH_BATCH_SIZE = 256

internal fun merge(
    keydir: SharedKeydir,
    files: DataFileSet,
    writer: Writer,
    metrics: Metrics,
    beforeRepoint: (ByteArray) -> Unit,
) {
    val scope = writer.sealForMerge() ?: return
    val output = OutputWriter(files, scope.firstOutputId, scope.endOfOutputIds, scope.seqFloor, writer.maxFileSize)
    val copier = Copier(keydir, files, output, beforeRepoint)
    for (fileId in scope.inputIds) copier.copyLiveEntriesOf(fileId)
    val nCopied = copier.finish()
    removeInputFiles(keydir, files, scope.inputIds)
    metrics.recordMergeSummary(scope.inputIds.size, nCopied)
}

/**
 * Remove the input files oldest first, stopping at the first one the keydir
 * still points into, so that a file is never removed while an older one
 * remains.
 */
private fun removeInputFiles(keydir: SharedKeydir, files: DataFileSet, inputIds: List<Long>) {
    // No write since the seal points into the input, so this can't change while the files go.
    val inputs = inputIds.toHashSet()
    val oldestInUse = keydir.withRead { kd -> kd.entries().map { it.value.fileId }.filter { it in inputs }.minOrNull() }
    for (fileId in inputIds) {
        if (oldestInUse != null && fileId >= oldestInUse) {
            warn(
                "merge kept ${dataPath(files.dir, oldestInUse)} and every newer input file: the keydir still points " +
                    "into it, at values merge couldn't read from it",
            )
            break
        }
        files.remove(fileId)
    }
    files.syncDir()
}

private class Repoint(val key: ByteArray, val old: KeydirEntry, val new: KeydirEntry)

private class Copier(
    private val keydir: SharedKeydir,
    private val files: DataFileSet,
    private val output: OutputWriter,
    private val beforeRepoint: (ByteArray) -> Unit,
) {
    private val pending = ArrayList<Repoint>()
    private var nCopied = 0

    fun copyLiveEntriesOf(fileId: Long) {
        val path = dataPath(files.dir, fileId)
        Scan(path).use { scan ->
            for (scanned in scan) {
                when (scanned) {
                    is Scanned.Found -> copyLiveOps(fileId, scanned.offset, scanned.record)
                    is Scanned.Corrupt ->
                        warn("$path has a corrupt entry at offset ${scanned.offset} (CRC mismatch); merge is dropping it")
                    is Scanned.Truncated -> {}
                }
            }
        }
    }

    private fun copyLiveOps(fileId: Long, offset: Long, record: Record) {
        for (op in record.ops) {
            if (op.tombstone) continue
            val valuePos = offset + op.valueOffset
            val old = keydir[op.key]
            if (old == null || old.fileId != fileId || old.valuePos != valuePos) continue
            val written = output.write(record.seq, record.timestamp, op.key, op.value)
            pending.add(Repoint(op.key, old, old.copy(fileId = written.fileId, valuePos = written.valuePos)))
            nCopied++
            if (written.isFlushed) applyPendingRepoints()
        }
    }

    private fun applyPendingRepoints() {
        for (r in pending) {
            beforeRepoint(r.key)
            keydir.casRepoint(r.key, r.old, r.new)
        }
        pending.clear()
    }

    fun finish(): Int {
        output.finish()
        applyPendingRepoints()
        return nCopied
    }
}

private class WrittenCopy(val fileId: Long, val valuePos: Long, val isFlushed: Boolean)

/** Writes a merge's output: data files with a hint file each, from the reserved ids. */
private class OutputWriter(
    private val files: DataFileSet,
    private var nextId: Long,
    private val endOfIds: Long,
    private val seqFloor: Long,
    private val maxFileSize: Long,
) {
    private var data: ActiveFile? = null
    private var hint: HintWriter? = null
    private var nUnflushed = 0
    private var hasCreatedAFile = false

    fun write(seq: Long, timestamp: Int, key: ByteArray, value: ByteArray): WrittenCopy {
        val encoded = encodeRecord(seq, timestamp, listOf(OpRef(key, value, false)))
        val out = currentFile()
        val fileId = out.fileId
        val valuePos = out.appendBuffered(encoded.bytes) + encoded.valueOffset(0)
        hint!!.write(HintRecord(seq, timestamp, value.size.toLong(), false, valuePos, key))
        nUnflushed++
        val isFlushed =
            if (out.len >= maxFileSize) {
                closeCurrentFile()
                true
            } else if (nUnflushed >= FLUSH_BATCH_SIZE) {
                out.flush()
                nUnflushed = 0
                true
            } else {
                false
            }
        return WrittenCopy(fileId, valuePos, isFlushed)
    }

    /**
     * The output is never empty: its header carries the sequence floor, and
     * after a merge that kept nothing it may be the only file that does.
     */
    fun finish() {
        if (!hasCreatedAFile) currentFile()
        closeCurrentFile()
        files.syncDir()
    }

    private fun currentFile(): ActiveFile {
        data?.let { return it }
        if (nextId >= endOfIds) throw java.io.IOException("merge ran out of its reserved file ids")
        val fileId = nextId++
        val created = files.createActive(fileId, seqFloor)
        data = created
        hint = HintWriter(hintPath(files.dir, fileId))
        hasCreatedAFile = true
        return created
    }

    private fun closeCurrentFile() {
        data?.let { d ->
            d.sync()
            d.close()
            hint!!.finish()
        }
        data = null
        hint = null
        nUnflushed = 0
    }
}

/** Writes one version 2 hint file: the magic, a record per entry, then a CRC32 of everything before it. */
private class HintWriter(path: Path) {
    private val out: OutputStream = BufferedOutputStream(Files.newOutputStream(path))
    private val crc = CRC32()

    init {
        crc.update(HINT_MAGIC)
        out.write(HINT_MAGIC)
    }

    fun write(record: HintRecord) {
        val bytes = encodeHintV2(record)
        crc.update(bytes)
        out.write(bytes)
    }

    /** Write the trailing CRC and close. Not fsynced: a hint that didn't reach the disk fails its CRC. */
    fun finish() {
        val trailer = ByteArray(4)
        trailer.putU32(0, crc.value.toInt())
        out.write(trailer)
        out.close()
    }
}
