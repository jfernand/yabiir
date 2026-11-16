/**
 * The bytes of yabiir's files: a port of `crates/yabiir/src/format`, whose
 * module docs are the specification. Everything is little-endian.
 *
 * Format version 2, written by yabiir 0.9 and later:
 *
 * ```text
 * data file header (24 bytes)
 * 0   8  magic       "ybcdat02"
 * 8   8  seq_floor   every later write has a sequence number >= this
 * 16  4  crc32       of bytes [0..16)
 * 20  4  reserved
 *
 * record (24-byte header)
 * 0   4  crc         CRC32 of bytes [4..end) of this record
 * 4   4  body_len    bytes after the header
 * 8   8  seq         the write's sequence number: the version of every key it puts
 * 16  4  tstamp
 * 20  4  n_ops
 * 24  .. n_ops times: ksz_and_flags (4), value_sz (4), key, value
 *
 * hint file
 * "ybchnt02", then per record: seq (8), tstamp (4), ksz_and_flags (4),
 * value_sz (4), value_pos (8), key; then a CRC32 of everything before it.
 * ```
 *
 * Format version 1 (yabiir 0.8 and earlier) has no file header; a data file
 * is a run of 16-byte-header entries (crc, tstamp, ksz_and_flags, value_sz,
 * key, value), and a hint file is 20-byte-header records (tstamp,
 * ksz_and_flags, value_sz, value_pos, key) with the same CRC trailer. Both
 * stay readable; neither is written any more, except by tests.
 *
 * Bit 31 of `ksz_and_flags` is the tombstone flag; the low 31 bits are the
 * key's length.
 */
package io.github.jfernand.yabiir.format

import java.io.IOException
import java.io.InputStream
import java.util.zip.CRC32

public const val FILE_HEADER_SIZE: Int = 24
public const val RECORD_HEADER_SIZE: Int = 24
public const val OP_HEADER_SIZE: Int = 8
public const val ENTRY_HEADER_SIZE: Int = 16
public const val HINT_HEADER_SIZE: Int = 20
public const val HINT_V2_HEADER_SIZE: Int = 28
public const val HINT_TRAILER_SIZE: Int = 4

public val DATA_MAGIC: ByteArray = "ybcdat02".toByteArray(Charsets.US_ASCII)
public val HINT_MAGIC: ByteArray = "ybchnt02".toByteArray(Charsets.US_ASCII)

private const val TOMBSTONE_BIT: Int = 1 shl 31
private const val KEYSIZE_MASK: Int = TOMBSTONE_BIT.inv()

/** The longest key a record can hold: 2³¹ − 1 bytes. */
public const val MAX_KEY_LEN: Long = 0x7fff_ffffL

/** The longest value a record can hold: 2³² − 1 bytes. */
public const val MAX_VALUE_LEN: Long = 0xffff_ffffL

internal fun encodeKeySize(keyLen: Int, tombstone: Boolean): Int =
    keyLen or (if (tombstone) TOMBSTONE_BIT else 0)

/** The key length, as an unsigned 31-bit value, and the tombstone flag. */
internal fun decodeKeySize(raw: Int): Pair<Int, Boolean> =
    (raw and KEYSIZE_MASK) to (raw and TOMBSTONE_BIT != 0)

public fun crc32(bytes: ByteArray, from: Int = 0, to: Int = bytes.size): Int {
    val crc = CRC32()
    crc.update(bytes, from, to - from)
    return crc.value.toInt()
}

// ---- little-endian helpers -------------------------------------------------

internal fun ByteArray.u32At(at: Int): Int =
    (this[at].toInt() and 0xff) or
        ((this[at + 1].toInt() and 0xff) shl 8) or
        ((this[at + 2].toInt() and 0xff) shl 16) or
        ((this[at + 3].toInt() and 0xff) shl 24)

internal fun ByteArray.u64At(at: Int): Long =
    (u32At(at).toLong() and 0xffff_ffffL) or (u32At(at + 4).toLong() shl 32)

internal fun ByteArray.putU32(at: Int, v: Int) {
    this[at] = v.toByte()
    this[at + 1] = (v ushr 8).toByte()
    this[at + 2] = (v ushr 16).toByte()
    this[at + 3] = (v ushr 24).toByte()
}

internal fun ByteArray.putU64(at: Int, v: Long) {
    putU32(at, v.toInt())
    putU32(at + 4, (v ushr 32).toInt())
}

/** An unsigned 32-bit field, widened. */
internal fun Int.unsigned(): Long = toLong() and 0xffff_ffffL

/**
 * Read up to `buf.size - from` bytes into `buf`, stopping early only at the
 * end of the stream. Returns how many were read.
 */
internal fun InputStream.readUpTo(buf: ByteArray, from: Int = 0, len: Int = buf.size - from): Int {
    var total = 0
    while (total < len) {
        val n = read(buf, from + total, len - total)
        if (n < 0) break
        total += n
    }
    return total
}

// ---- file header -----------------------------------------------------------

public fun encodeFileHeader(seqFloor: Long): ByteArray {
    val header = ByteArray(FILE_HEADER_SIZE)
    DATA_MAGIC.copyInto(header)
    header.putU64(8, seqFloor)
    header.putU32(16, crc32(header, 0, 16))
    return header
}

/** Which format a data file is in. */
public sealed interface FileFormat {
    public val dataStart: Long

    public data object V1 : FileFormat {
        override val dataStart: Long = 0
    }

    public data class V2(val seqFloor: Long) : FileFormat {
        override val dataStart: Long = FILE_HEADER_SIZE.toLong()
    }
}

/**
 * The format of a data file whose first bytes are `prefix` (up to
 * [FILE_HEADER_SIZE] of them). A file is version 2 if it starts with the
 * magic, or with a prefix of it: a header cut short by a crash makes an
 * empty version 2 file, not a version 1 file. A header whose CRC fails
 * keeps the format but loses its floor.
 */
public fun identify(prefix: ByteArray): FileFormat {
    val magicLen = minOf(prefix.size, DATA_MAGIC.size)
    for (i in 0 until magicLen) {
        if (prefix[i] != DATA_MAGIC[i]) return FileFormat.V1
    }
    val seqFloor =
        if (prefix.size >= FILE_HEADER_SIZE && crc32(prefix, 0, 16) == prefix.u32At(16)) {
            prefix.u64At(8)
        } else {
            0L
        }
    return FileFormat.V2(seqFloor)
}

// ---- records (version 2) ---------------------------------------------------

/** One operation to encode: a put of `value`, or a tombstone. */
public class OpRef(public val key: ByteArray, public val value: ByteArray, public val tombstone: Boolean)

public class EncodedRecord(public val bytes: ByteArray, private val valueOffsets: LongArray) {
    public val size: Int get() = bytes.size

    /** Where the `i`th operation's value starts, from the record's start. */
    public fun valueOffset(i: Int): Long = valueOffsets[i]
}

public fun bodyLen(ops: List<OpRef>): Long =
    ops.sumOf { OP_HEADER_SIZE.toLong() + it.key.size + it.value.size }

public fun encodeRecord(seq: Long, tstamp: Int, ops: List<OpRef>): EncodedRecord {
    val bodyLen = bodyLen(ops)
    require(bodyLen <= MAX_VALUE_LEN) { "record body too long" }
    val bytes = ByteArray(Math.toIntExact(RECORD_HEADER_SIZE + bodyLen))
    bytes.putU32(4, bodyLen.toInt())
    bytes.putU64(8, seq)
    bytes.putU32(16, tstamp)
    bytes.putU32(20, ops.size)
    val valueOffsets = LongArray(ops.size)
    var at = RECORD_HEADER_SIZE
    for ((i, op) in ops.withIndex()) {
        bytes.putU32(at, encodeKeySize(op.key.size, op.tombstone))
        bytes.putU32(at + 4, op.value.size)
        op.key.copyInto(bytes, at + OP_HEADER_SIZE)
        at += OP_HEADER_SIZE + op.key.size
        valueOffsets[i] = at.toLong()
        op.value.copyInto(bytes, at)
        at += op.value.size
    }
    bytes.putU32(0, crc32(bytes, 4, bytes.size))
    return EncodedRecord(bytes, valueOffsets)
}

public class RecordOp(
    public val key: ByteArray,
    public val value: ByteArray,
    public val tombstone: Boolean,
    /** Where the value starts, from the record's start. */
    public val valueOffset: Long,
)

public class Record(public val seq: Long, public val timestamp: Int, public val ops: List<RecordOp>)

public sealed interface RecordRead {
    public class Ok(public val record: Record, public val totalLen: Long) : RecordRead

    public data object Truncated : RecordRead

    public class Corrupt(public val totalLen: Long) : RecordRead
}

/**
 * Read the next record of `input`, which has `available` bytes left.
 * `null` at the end of the stream.
 */
public fun readRecord(input: InputStream, available: Long): RecordRead? {
    val header = ByteArray(RECORD_HEADER_SIZE)
    val n = input.readUpTo(header)
    if (n == 0) return null
    if (n < RECORD_HEADER_SIZE) return RecordRead.Truncated
    val crc = header.u32At(0)
    val bodyLen = header.u32At(4).unsigned()
    val totalLen = RECORD_HEADER_SIZE + bodyLen
    if (totalLen > available) return RecordRead.Truncated

    val covered = ByteArray(Math.toIntExact(RECORD_HEADER_SIZE - 4 + bodyLen))
    header.copyInto(covered, 0, 4, RECORD_HEADER_SIZE)
    val m = input.readUpTo(covered, RECORD_HEADER_SIZE - 4)
    if (m < bodyLen) return RecordRead.Truncated
    if (crc32(covered) != crc) return RecordRead.Corrupt(totalLen)

    val seq = header.u64At(8)
    val timestamp = header.u32At(16)
    val nOps = header.u32At(20).unsigned()
    val ops = parseOps(covered, RECORD_HEADER_SIZE - 4, nOps) ?: return RecordRead.Corrupt(totalLen)
    return RecordRead.Ok(Record(seq, timestamp, ops), totalLen)
}

/** The ops of a record body starting at `bodyStart` of `buf`, or `null` if they don't fill it exactly. */
private fun parseOps(buf: ByteArray, bodyStart: Int, nOps: Long): List<RecordOp>? {
    val bodyLen = buf.size - bodyStart
    val ops = ArrayList<RecordOp>(minOf(nOps, bodyLen / OP_HEADER_SIZE + 1L).toInt())
    var at = 0L
    for (i in 0 until nOps) {
        if (at + OP_HEADER_SIZE > bodyLen) return null
        val (keyLen, tombstone) = decodeKeySize(buf.u32At((bodyStart + at).toInt()))
        val valueLen = buf.u32At((bodyStart + at + 4).toInt()).unsigned()
        val keyStart = at + OP_HEADER_SIZE
        val valueStart = keyStart + keyLen
        val valueEnd = valueStart + valueLen
        if (valueEnd > bodyLen) return null
        ops.add(
            RecordOp(
                key = buf.copyOfRange((bodyStart + keyStart).toInt(), (bodyStart + valueStart).toInt()),
                value = buf.copyOfRange((bodyStart + valueStart).toInt(), (bodyStart + valueEnd).toInt()),
                tombstone = tombstone,
                valueOffset = RECORD_HEADER_SIZE + valueStart,
            ),
        )
        at = valueEnd
    }
    return if (at == bodyLen.toLong()) ops else null
}

// ---- entries (version 1) ---------------------------------------------------

public fun encodeEntry(key: ByteArray, value: ByteArray, tombstone: Boolean, tstamp: Int): ByteArray {
    val buf = ByteArray(ENTRY_HEADER_SIZE + key.size + value.size)
    buf.putU32(4, tstamp)
    buf.putU32(8, encodeKeySize(key.size, tombstone))
    buf.putU32(12, value.size)
    key.copyInto(buf, ENTRY_HEADER_SIZE)
    value.copyInto(buf, ENTRY_HEADER_SIZE + key.size)
    buf.putU32(0, crc32(buf, 4, buf.size))
    return buf
}

/**
 * Read the next version 1 entry of `input`, as a record of one operation
 * with sequence number 1: the version every value written before versions
 * existed has.
 */
public fun readEntry(input: InputStream, available: Long): RecordRead? {
    val header = ByteArray(ENTRY_HEADER_SIZE)
    val n = input.readUpTo(header)
    if (n == 0) return null
    if (n < ENTRY_HEADER_SIZE) return RecordRead.Truncated
    val crc = header.u32At(0)
    val tstamp = header.u32At(4)
    val (keyLen, tombstone) = decodeKeySize(header.u32At(8))
    val valueLen = header.u32At(12).unsigned()
    val bodyLen = keyLen + valueLen
    val totalLen = ENTRY_HEADER_SIZE + bodyLen
    if (totalLen > available) return RecordRead.Truncated
    val covered = ByteArray(Math.toIntExact(ENTRY_HEADER_SIZE - 4 + bodyLen))
    header.copyInto(covered, 0, 4, ENTRY_HEADER_SIZE)
    val m = input.readUpTo(covered, ENTRY_HEADER_SIZE - 4)
    if (m < bodyLen) return RecordRead.Truncated
    if (crc32(covered) != crc) return RecordRead.Corrupt(totalLen)
    val keyStart = ENTRY_HEADER_SIZE - 4
    val op = RecordOp(
        key = covered.copyOfRange(keyStart, keyStart + keyLen),
        value = covered.copyOfRange(keyStart + keyLen, covered.size),
        tombstone = tombstone,
        valueOffset = ENTRY_HEADER_SIZE.toLong() + keyLen,
    )
    return RecordRead.Ok(Record(1, tstamp, listOf(op)), totalLen)
}

// ---- hint files ------------------------------------------------------------

public class HintRecord(
    public val seq: Long,
    public val timestamp: Int,
    /** Unsigned 32 bits, widened. */
    public val valueSize: Long,
    public val tombstone: Boolean,
    public val valuePos: Long,
    public val key: ByteArray,
)

public fun encodeHintV2(record: HintRecord): ByteArray {
    val buf = ByteArray(HINT_V2_HEADER_SIZE + record.key.size)
    buf.putU64(0, record.seq)
    buf.putU32(8, record.timestamp)
    buf.putU32(12, encodeKeySize(record.key.size, record.tombstone))
    buf.putU32(16, record.valueSize.toInt())
    buf.putU64(20, record.valuePos)
    record.key.copyInto(buf, HINT_V2_HEADER_SIZE)
    return buf
}

public fun encodeHintV1(key: ByteArray, tstamp: Int, valueSize: Int, tombstone: Boolean, valuePos: Long): ByteArray {
    val buf = ByteArray(HINT_HEADER_SIZE + key.size)
    buf.putU32(0, tstamp)
    buf.putU32(4, encodeKeySize(key.size, tombstone))
    buf.putU32(8, valueSize)
    buf.putU64(12, valuePos)
    key.copyInto(buf, HINT_HEADER_SIZE)
    return buf
}

/** Thrown by [decodeHintFile] for a hint file that can't be trusted. */
public class BadHintFile(message: String) : IOException(message)

/**
 * The records of a whole hint file, version 1 or 2, after checking its CRC
 * trailer. Version 1 records get sequence number 1.
 */
public fun decodeHintFile(bytes: ByteArray): List<HintRecord> {
    if (bytes.size < HINT_TRAILER_SIZE) throw BadHintFile("failed its whole-file CRC check")
    val contentEnd = bytes.size - HINT_TRAILER_SIZE
    if (crc32(bytes, 0, contentEnd) != bytes.u32At(contentEnd)) {
        throw BadHintFile("failed its whole-file CRC check")
    }
    val isV2 = contentEnd >= HINT_MAGIC.size && (HINT_MAGIC.indices).all { bytes[it] == HINT_MAGIC[it] }
    val truncated = "has a truncated record despite a valid CRC"
    val records = ArrayList<HintRecord>()
    var at = if (isV2) HINT_MAGIC.size.toLong() else 0L
    val headerSize = if (isV2) HINT_V2_HEADER_SIZE else HINT_HEADER_SIZE
    while (at < contentEnd) {
        if (at + headerSize > contentEnd) throw BadHintFile(truncated)
        val h = at.toInt()
        val record =
            if (isV2) {
                val (keyLen, tombstone) = decodeKeySize(bytes.u32At(h + 12))
                val keyEnd = at + HINT_V2_HEADER_SIZE + keyLen
                if (keyEnd > contentEnd) throw BadHintFile(truncated)
                HintRecord(
                    seq = bytes.u64At(h),
                    timestamp = bytes.u32At(h + 8),
                    valueSize = bytes.u32At(h + 16).unsigned(),
                    tombstone = tombstone,
                    valuePos = bytes.u64At(h + 20),
                    key = bytes.copyOfRange(h + HINT_V2_HEADER_SIZE, keyEnd.toInt()),
                )
            } else {
                val (keyLen, tombstone) = decodeKeySize(bytes.u32At(h + 4))
                val keyEnd = at + HINT_HEADER_SIZE + keyLen
                if (keyEnd > contentEnd) throw BadHintFile(truncated)
                HintRecord(
                    seq = 1,
                    timestamp = bytes.u32At(h),
                    valueSize = bytes.u32At(h + 8).unsigned(),
                    tombstone = tombstone,
                    valuePos = bytes.u64At(h + 12),
                    key = bytes.copyOfRange(h + HINT_HEADER_SIZE, keyEnd.toInt()),
                )
            }
        records.add(record)
        at += headerSize + record.key.size
    }
    return records
}
