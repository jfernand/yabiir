package io.github.jfernand.yabiir

import java.io.IOException
import java.nio.file.AccessDeniedException
import java.nio.file.DirectoryNotEmptyException
import java.nio.file.FileAlreadyExistsException
import java.nio.file.NoSuchFileException
import java.nio.file.NotDirectoryException

/** The errors yabiir reports: one subclass per variant of the Rust crate's `Error`. */
public sealed class YabiirException(message: String, cause: Throwable? = null) : RuntimeException(message, cause) {
    /** The name of the matching Rust variant, as the differential tests compare it. */
    public abstract val variant: String

    /** An underlying I/O failure. */
    public class Io(public val io: IOException) : YabiirException("io error: ${io.message}", io) {
        override val variant: String get() = "Io"

        /** The closest Rust `io::ErrorKind`. */
        public val kind: String
            get() =
                when (io) {
                    is NoSuchFileException -> "NotFound"
                    is AccessDeniedException -> "PermissionDenied"
                    is FileAlreadyExistsException -> "AlreadyExists"
                    is NotDirectoryException -> "NotADirectory"
                    is io.github.jfernand.yabiir.datafile.IsADirectoryException -> "IsADirectory"
                    is DirectoryNotEmptyException -> "DirectoryNotEmpty"
                    is java.io.EOFException -> "UnexpectedEof"
                    else -> "Other"
                }
    }

    /** Another read-write handle already has the directory open. */
    public class AlreadyLocked : YabiirException("directory already locked for writing by another handle") {
        override val variant: String get() = "AlreadyLocked"
    }

    public class EmptyKey : YabiirException("key must not be empty") {
        override val variant: String get() = "EmptyKey"
    }

    public class KeyTooLarge : YabiirException("key too large: the limit is 2^31 - 1 bytes") {
        override val variant: String get() = "KeyTooLarge"
    }

    public class ValueTooLarge : YabiirException("value too large: the limit is 2^32 - 1 bytes") {
        override val variant: String get() = "ValueTooLarge"
    }

    public class ReadOnly : YabiirException("datastore is not open for writing (read-only handle)") {
        override val variant: String get() = "ReadOnly"
    }

    public class Unordered :
        YabiirException("a range of keys needs a sorted keydir: open with keyOrder = KeyOrder.SORTED") {
        override val variant: String get() = "Unordered"
    }

    /** A write batch's check at position [index] didn't hold, so nothing in the batch was written. */
    public class ConditionFailed(public val index: Int) :
        YabiirException(
            "condition failed: the check at position $index of the batch didn't hold, so nothing was written",
        ) {
        override val variant: String get() = "ConditionFailed"
    }

    public class BatchTooLarge : YabiirException("batch too large: its operations add up to more than 2^32 - 1 bytes") {
        override val variant: String get() = "BatchTooLarge"
    }
}

/** Run [block], turning an [IOException] into [YabiirException.Io]. */
internal inline fun <T> io(block: () -> T): T =
    try {
        block()
    } catch (e: IOException) {
        throw YabiirException.Io(e)
    }
