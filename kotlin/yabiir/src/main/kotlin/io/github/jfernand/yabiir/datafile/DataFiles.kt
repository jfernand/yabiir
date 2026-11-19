/**
 * The store's files: naming them, appending to the active one, reading any
 * of them, and scanning one record by record. A port of
 * `crates/yabiir/src/datafile`.
 */
package io.github.jfernand.yabiir.datafile

import io.github.jfernand.yabiir.format.FILE_HEADER_SIZE
import io.github.jfernand.yabiir.format.FileFormat
import io.github.jfernand.yabiir.format.RecordRead
import io.github.jfernand.yabiir.format.Record
import io.github.jfernand.yabiir.format.encodeFileHeader
import io.github.jfernand.yabiir.format.identify
import io.github.jfernand.yabiir.format.readEntry
import io.github.jfernand.yabiir.format.readRecord
import java.io.BufferedInputStream
import java.io.EOFException
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.APPEND
import java.nio.file.StandardOpenOption.CREATE
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap

private const val DATA_SUFFIX = ".bitcask.data"
private const val HINT_SUFFIX = ".bitcask.hint"
private const val ID_DIGITS = 20

/** Data file ids are unsigned 32-bit numbers, held in a `Long`. */
public const val MAX_FILE_ID: Long = 0xffff_ffffL

public fun dataPath(dir: Path, fileId: Long): Path = dir.resolve(String.format(Locale.ROOT, "%020d", fileId) + DATA_SUFFIX)

public fun hintPath(dir: Path, fileId: Long): Path = dir.resolve(String.format(Locale.ROOT, "%020d", fileId) + HINT_SUFFIX)

internal fun parseDataFileId(name: String): Long? {
    if (!name.endsWith(DATA_SUFFIX)) return null
    val stem = name.removeSuffix(DATA_SUFFIX)
    if (stem.length != ID_DIGITS || !stem.all { it in '0'..'9' }) return null
    val id = stem.toBigInteger()
    return if (id <= MAX_FILE_ID.toBigInteger()) id.toLong() else null
}

/** The id of every data file in [dir], ascending. Anything else in it is skipped. */
public fun discover(dir: Path): List<Long> =
    Files.list(dir).use { names ->
        names.toList().mapNotNull { parseDataFileId(it.fileName.toString()) }.sorted()
    }

/** Make the names of the files in [dir] durable. */
public fun syncDir(dir: Path) {
    FileChannel.open(dir, READ).use { it.force(true) }
}

/** Read exactly [len] bytes at [pos], or fail with [EOFException]. */
public fun preadExact(channel: FileChannel, pos: Long, len: Long): ByteArray {
    val buf = ByteBuffer.allocate(Math.toIntExact(len))
    var at = pos
    while (buf.hasRemaining()) {
        val n = channel.read(buf, at)
        if (n < 0) throw EOFException("failed to fill whole buffer")
        at += n
    }
    return buf.array()
}

/**
 * The data file being appended to. Appends are buffered until [flush],
 * which hands them to the OS in one `write`, as the Rust writer does once
 * per group of batches.
 */
public class ActiveFile private constructor(
    public val fileId: Long,
    private val channel: FileChannel,
    private var offset: Long,
) : AutoCloseable {
    private var buffer = ByteArray(8 * 1024)
    private var buffered = 0

    public val len: Long get() = offset

    /** Whether the file holds no record yet, only its header. */
    public val isEmpty: Boolean get() = offset <= FILE_HEADER_SIZE

    /** Buffer [bytes] for the end of the file and return where they start. */
    public fun appendBuffered(bytes: ByteArray): Long {
        val start = offset
        if (buffered + bytes.size > buffer.size) {
            buffer = buffer.copyOf(maxOf(buffer.size * 2, buffered + bytes.size))
        }
        bytes.copyInto(buffer, buffered)
        buffered += bytes.size
        offset += bytes.size
        return start
    }

    public fun flush() {
        if (buffered == 0) return
        val buf = ByteBuffer.wrap(buffer, 0, buffered)
        while (buf.hasRemaining()) channel.write(buf)
        buffered = 0
    }

    public fun sync() {
        flush()
        channel.force(false)
    }

    /** Flush whatever is buffered and close, without an fsync. */
    override fun close() {
        try {
            flush()
        } finally {
            channel.close()
        }
    }

    public companion object {
        /** Create data file [fileId] of [dir], starting it with a header that holds [seqFloor]. */
        public fun create(dir: Path, fileId: Long, seqFloor: Long): ActiveFile {
            val channel = FileChannel.open(dataPath(dir, fileId), CREATE, WRITE, APPEND)
            val active = ActiveFile(fileId, channel, channel.size())
            if (active.offset == 0L) {
                active.appendBuffered(encodeFileHeader(seqFloor))
                active.flush()
            }
            return active
        }
    }
}

/**
 * The store's data files, and a read handle for each that's being read. A
 * handle stays open after its file is removed, so a reader that took it
 * while a merge ran can still read the old copy.
 */
public class DataFileSet(public val dir: Path) : AutoCloseable {
    private val readers = ConcurrentHashMap<Long, FileChannel>()

    /** Create [fileId] as the new active file, removing any stale hint file of that id. */
    public fun createActive(fileId: Long, seqFloor: Long): ActiveFile {
        removeIfPresent(hintPath(dir, fileId))
        val active = ActiveFile.create(dir, fileId, seqFloor)
        unwrapped { readers.computeIfAbsent(fileId) { open(it) } }
        return active
    }

    /** Open read handles for [fileIds]. */
    public fun register(fileIds: List<Long>) {
        for (id in fileIds) unwrapped { readers.computeIfAbsent(id) { open(it) } }
    }

    public fun handle(fileId: Long): FileChannel =
        readers[fileId] ?: unwrapped { readers.computeIfAbsent(fileId) { open(it) } }

    public fun readAt(fileId: Long, pos: Long, len: Long): ByteArray = preadExact(handle(fileId), pos, len)

    public fun lenOf(fileId: Long): Long = Files.size(dataPath(dir, fileId))

    /** Remove data file [fileId] and its hint file. */
    public fun remove(fileId: Long) {
        removeIfPresent(hintPath(dir, fileId))
        removeIfPresent(dataPath(dir, fileId))
        forget(fileId)
    }

    /**
     * Drop the cached handle for [fileId]. It isn't closed: a reader may
     * still hold it. The JVM closes it once it's unreachable.
     */
    public fun forget(fileId: Long) {
        readers.remove(fileId)
    }

    public fun syncDir(): Unit = syncDir(dir)

    override fun close() {
        readers.values.forEach { runCatching { it.close() } }
        readers.clear()
    }

    private fun open(fileId: Long): FileChannel =
        try {
            FileChannel.open(dataPath(dir, fileId), READ)
        } catch (e: IOException) {
            throw java.io.UncheckedIOException(e)
        }

    // `computeIfAbsent` can't throw a checked exception; unwrap it again.
    private inline fun <T> unwrapped(block: () -> T): T =
        try {
            block()
        } catch (e: java.io.UncheckedIOException) {
            throw e.cause ?: e
        }
}

/** What a [scan] finds at an offset. */
public sealed interface Scanned {
    public val offset: Long

    public class Found(override val offset: Long, public val record: Record) : Scanned

    /** A record whose CRC fails. The scan goes on after it. */
    public class Corrupt(override val offset: Long) : Scanned

    /** A record cut short: the scan ends here. */
    public class Truncated(override val offset: Long) : Scanned
}

/**
 * Read the data file at [path], version 1 or 2, record by record. A
 * version 1 entry comes back as a record of one operation with sequence
 * number 1.
 */
public class Scan(path: Path) : Iterator<Scanned>, AutoCloseable {
    private val len: Long
    public val format: FileFormat
    private val input: BufferedInputStream
    private var offset: Long
    private var next: Scanned? = null
    private var isDone = false

    init {
        val channel = FileChannel.open(path, READ)
        try {
            len = channel.size()
            val prefix = preadExact(channel, 0, minOf(len, FILE_HEADER_SIZE.toLong()))
            format = identify(prefix)
            offset =
                when (format) {
                    FileFormat.V1 -> 0
                    is FileFormat.V2 -> prefix.size.toLong()
                }
            channel.position(offset)
            input = BufferedInputStream(Channels.newInputStream(channel), 256 * 1024)
        } catch (e: Throwable) {
            channel.close()
            throw e
        }
    }

    override fun hasNext(): Boolean {
        if (next == null && !isDone) next = advance()
        return next != null
    }

    override fun next(): Scanned {
        if (!hasNext()) throw NoSuchElementException()
        return next!!.also { next = null }
    }

    private fun advance(): Scanned? {
        val at = offset
        val available = maxOf(0, len - at)
        val read =
            when (format) {
                FileFormat.V1 -> readEntry(input, available)
                is FileFormat.V2 -> readRecord(input, available)
            }
        return when (read) {
            null -> {
                isDone = true
                null
            }
            RecordRead.Truncated -> {
                isDone = true
                Scanned.Truncated(at)
            }
            is RecordRead.Ok -> {
                offset += read.totalLen
                Scanned.Found(at, read.record)
            }
            is RecordRead.Corrupt -> {
                offset += read.totalLen
                Scanned.Corrupt(at)
            }
        }
    }

    override fun close() {
        input.close()
    }
}

/** Thrown where Rust's `fs::remove_file` fails with `IsADirectory`. */
internal class IsADirectoryException(path: Path) : java.nio.file.FileSystemException(path.toString(), null, "Is a directory")

/**
 * Remove the file at [path] if there is one, like Rust's `fs::remove_file`:
 * a directory there is an error, where `Files.deleteIfExists` would remove
 * an empty one.
 */
internal fun removeIfPresent(path: Path) {
    if (Files.isDirectory(path, java.nio.file.LinkOption.NOFOLLOW_LINKS)) throw IsADirectoryException(path)
    Files.deleteIfExists(path)
}
