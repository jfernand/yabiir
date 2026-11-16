package io.github.jfernand.yabiir

import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE
import java.nio.file.StandardOpenOption.WRITE
import java.util.concurrent.ConcurrentHashMap

/**
 * The single-writer lock on a store's directory: an exclusive lock on its
 * `.bitcask.lock` file, held for the life of a read-write handle.
 *
 * The JVM's file locks are POSIX record locks (`fcntl`), not the `flock`
 * the Rust crate takes, and on Linux the two don't see each other: this
 * keeps two JVM processes from writing one store, but not a JVM and a Rust
 * process. Don't open one store from both at once.
 *
 * POSIX locks belong to the process, and closing *any* descriptor of the
 * file drops them. So a second handle in the same JVM must not even open
 * the lock file: a set of the directories this JVM has locked turns it away
 * first.
 */
internal class DirLock private constructor(
    private val dir: Path,
    private val channel: FileChannel,
    private val lock: FileLock,
) : AutoCloseable {
    override fun close() {
        try {
            runCatching { lock.release() }
            channel.close()
        } finally {
            lockedHere.remove(dir)
        }
    }

    companion object {
        private val lockedHere: MutableSet<Path> = ConcurrentHashMap.newKeySet()

        fun acquire(dir: Path): DirLock {
            val real = io { dir.toRealPath() }
            if (!lockedHere.add(real)) throw YabiirException.AlreadyLocked()
            try {
                // Not truncated: the content is never read, only the lock matters.
                val channel = io { FileChannel.open(real.resolve(".bitcask.lock"), CREATE, WRITE) }
                val lock =
                    try {
                        channel.tryLock()
                    } catch (e: OverlappingFileLockException) {
                        null
                    } catch (e: java.io.IOException) {
                        channel.close()
                        throw YabiirException.Io(e)
                    }
                if (lock == null) {
                    channel.close()
                    throw YabiirException.AlreadyLocked()
                }
                return DirLock(real, channel, lock)
            } catch (e: Throwable) {
                lockedHere.remove(real)
                throw e
            }
        }
    }
}
