package io.github.jfernand.yabiir

import java.io.IOException
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException

/** One operation of a write batch. */
public sealed interface Op {
    public val key: ByteArray

    /** Store [value] under [key]. */
    public class Put(override val key: ByteArray, public val value: ByteArray) : Op

    /** Remove [key]. Removing a key that isn't there does nothing. */
    public class Delete(override val key: ByteArray) : Op

    /** Write nothing, but fail the whole batch unless [key] meets [cond]. */
    public class Check(override val key: ByteArray, public val cond: Cond) : Op
}

/** A condition on a key, for [Op.Check]. */
public sealed interface Cond {
    /** The key has no value. */
    public data object Absent : Cond

    /** The key has a value. */
    public data object Exists : Cond

    /** The key has a value, and its version is exactly [version]. */
    public data class Version(val version: Long) : Cond

    /** Whether a key whose current version is [version] (`null`: no value) meets this condition. */
    public fun holds(version: Long?): Boolean =
        when (this) {
            Absent -> version == null
            Exists -> version != null
            is Version -> version == this.version
        }
}

/**
 * The result of a write batch, once the writer thread has decided it.
 *
 * [join] blocks until then and returns the version every key the batch put
 * now has, or `null` if its conditions held but it had nothing to write. It
 * throws [YabiirException.ConditionFailed] if a check failed, or another
 * [YabiirException]. [future] is the same result for asynchronous callers.
 * Dropping a commit doesn't cancel the batch.
 */
public class Commit internal constructor(
    /** Completes with the version, or `null`; or exceptionally with a [YabiirException]. */
    public val future: CompletableFuture<Long?>,
) {
    /** Block until the batch is decided, and return how it ended. */
    public fun join(): Long? {
        // A write without an fsync is decided within microseconds, sooner
        // than this thread could be parked and woken again.
        val spinUntil = System.nanoTime() + SPIN_BEFORE_PARKING_NANOS
        while (!future.isDone && System.nanoTime() < spinUntil) {
            Thread.onSpinWait()
        }
        try {
            return future.get()
        } catch (e: ExecutionException) {
            throw e.cause as? YabiirException ?: YabiirException.Io(IOException(e.cause))
        }
    }

    internal companion object {
        fun failed(e: YabiirException): Commit = Commit(CompletableFuture.failedFuture(e))
    }
}

/** How long [Commit.join] spins before it parks, and the writer thread before it waits for the next batch. */
internal const val SPIN_BEFORE_PARKING_NANOS: Long = 50_000
