package io.github.jfernand.yabiir.datafile

import io.github.jfernand.yabiir.format.EncodedRecord
import io.github.jfernand.yabiir.format.OpRef
import io.github.jfernand.yabiir.format.encodeRecord

/** A record of one operation, with sequence number 1: the Rust `testutil::one_op`. */
internal fun oneOp(key: ByteArray, value: ByteArray, tombstone: Boolean, tstamp: Int): EncodedRecord =
    encodeRecord(1, tstamp, listOf(OpRef(key, value, tombstone)))

/**
 * Append a record of one operation and hand it to the OS, and return where
 * its value starts: the Rust test-only `ActiveFile::append_value`.
 */
internal fun ActiveFile.appendValue(record: EncodedRecord): Long {
    val start = appendBuffered(record.bytes)
    flush()
    return start + record.valueOffset(0)
}
