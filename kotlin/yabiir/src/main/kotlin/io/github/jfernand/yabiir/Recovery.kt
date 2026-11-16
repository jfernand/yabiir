/**
 * Rebuilding the keydir at open: a port of `crates/yabiir/src/recovery`.
 *
 * Files are replayed in ascending id order and each file's records in file
 * order, and each operation replaces or removes its key: last write wins. A
 * file with a valid hint file is replayed from the hint instead.
 */
package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.datafile.Scan
import io.github.jfernand.yabiir.datafile.Scanned
import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.datafile.hintPath
import io.github.jfernand.yabiir.datafile.preadExact
import io.github.jfernand.yabiir.format.BadHintFile
import io.github.jfernand.yabiir.format.FILE_HEADER_SIZE
import io.github.jfernand.yabiir.format.FileFormat
import io.github.jfernand.yabiir.format.HintRecord
import io.github.jfernand.yabiir.format.Record
import io.github.jfernand.yabiir.format.decodeHintFile
import io.github.jfernand.yabiir.format.identify
import io.github.jfernand.yabiir.keydir.Keydir
import io.github.jfernand.yabiir.keydir.KeydirEntry
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ

/**
 * Replay data files [fileIds] (ascending) of [dir] into [keydir], and return
 * the sequence number the next write must get: above every one in the
 * files, and at least every file's sequence floor.
 */
internal fun recover(dir: Path, fileIds: List<Long>, keydir: Keydir): Long {
    val newest = fileIds.lastOrNull()
    val nextSeq = NextSeq()
    for (fileId in fileIds) {
        val data = dataPath(dir, fileId)
        val hint = hintPath(dir, fileId)
        val isReplayedFromHint = Files.exists(hint) && replayHint(hint, data, fileId, keydir, nextSeq)
        if (!isReplayedFromHint) replayData(data, fileId, fileId == newest, keydir, nextSeq)
    }
    return nextSeq.value
}

/** The lowest sequence number the next write can get. Compared unsigned, as the Rust `u64` is. */
internal class NextSeq {
    var value: Long = 1
        private set

    fun saw(seq: Long) {
        atLeast(if (seq == -1L) seq else seq + 1)
    }

    fun atLeast(floor: Long) {
        if (java.lang.Long.compareUnsigned(floor, value) > 0) value = floor
    }
}

private fun replayData(path: Path, fileId: Long, isNewest: Boolean, keydir: Keydir, nextSeq: NextSeq) {
    Scan(path).use { scan ->
        (scan.format as? FileFormat.V2)?.let { nextSeq.atLeast(it.seqFloor) }
        for (scanned in scan) {
            when (scanned) {
                is Scanned.Found -> {
                    nextSeq.saw(scanned.record.seq)
                    apply(keydir, fileId, scanned.offset, scanned.record)
                }
                is Scanned.Corrupt -> warn("$path has a corrupt entry at offset ${scanned.offset} (CRC mismatch), skipping it")
                is Scanned.Truncated ->
                    if (!isNewest) {
                        warn("$path ends in a truncated entry at offset ${scanned.offset}, though it is not the newest file")
                    }
            }
        }
    }
}

private fun apply(keydir: Keydir, fileId: Long, offset: Long, record: Record) {
    for (op in record.ops) {
        if (op.tombstone) {
            keydir.remove(op.key)
            continue
        }
        keydir.insert(
            op.key,
            KeydirEntry(
                fileId = fileId,
                valueSize = op.value.size.toLong(),
                valuePos = offset + op.valueOffset,
                timestamp = record.timestamp,
                version = record.seq,
            ),
        )
    }
}

/** Apply the hint file at [hint], or return `false` with [keydir] untouched if it can't be trusted. */
private fun replayHint(hint: Path, data: Path, fileId: Long, keydir: Keydir, nextSeq: NextSeq): Boolean {
    FileChannel.open(data, READ).use { dataFile ->
        val dataLen = dataFile.size()
        val records =
            try {
                validate(Files.readAllBytes(hint), dataLen)
            } catch (e: BadHintFile) {
                warn("$hint ${e.message}; scanning the data file instead")
                return false
            } catch (e: IOException) {
                warn("$hint can't be read ($e); scanning the data file instead")
                return false
            }
        val prefix = preadExact(dataFile, 0, minOf(dataLen, FILE_HEADER_SIZE.toLong()))
        (identify(prefix) as? FileFormat.V2)?.let { nextSeq.atLeast(it.seqFloor) }
        for (record in records) {
            nextSeq.saw(record.seq)
            if (record.tombstone) {
                keydir.remove(record.key)
            } else {
                keydir.insert(
                    record.key,
                    KeydirEntry(fileId, record.valueSize, record.valuePos, record.timestamp, record.seq),
                )
            }
        }
        return true
    }
}

private fun validate(bytes: ByteArray, dataLen: Long): List<HintRecord> {
    val records = decodeHintFile(bytes)
    for (r in records) {
        // value_pos is unsigned; one past Long.MAX_VALUE is past any file.
        val end = r.valuePos + r.valueSize
        if (r.valuePos < 0 || end < 0 || end > dataLen) {
            throw BadHintFile(
                "points past the end of its data file (value at ${r.valuePos}, ${r.valueSize} bytes, in a file of $dataLen)",
            )
        }
    }
    return records
}

internal fun warn(message: String) {
    System.err.println("warning: $message")
}
