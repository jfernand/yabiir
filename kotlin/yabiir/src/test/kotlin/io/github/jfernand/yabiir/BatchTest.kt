package io.github.jfernand.yabiir

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import java.util.concurrent.CompletableFuture
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Ports of the tests in `crates/yabiir/src/batch.rs`. Rust's `Commit` is
 * completed through a `Completer`; the Kotlin one wraps a
 * [CompletableFuture] that the writer completes, and its `Future` impl is
 * [Commit.future].
 */
class BatchTest {
    @Test
    fun `wait returns what another thread completes`() {
        val future = CompletableFuture<Long?>()
        val commit = Commit(future)
        val t = Thread { future.complete(7) }
        t.start()
        assertEquals(7L, commit.join())
        t.join()
    }

    @Test
    fun `polling registers a waker that completion wakes`() {
        val future = CompletableFuture<Long?>()
        val commit = Commit(future)
        val flag = AtomicBoolean(false)
        commit.future.thenRun { flag.set(true) }
        assertFalse(commit.future.isDone)
        future.complete(null)
        assertTrue(flag.get())
        assertTrue(commit.future.isDone)
        assertNull(commit.future.getNow(-1L))
    }

    /**
     * Rust: a `Completer` dropped without completing (the writer panicked)
     * fails the commit with `Error::Io`. Kotlin has no `Completer`; the
     * analogue is a writer that fails the future with something other than
     * a [YabiirException], which [Commit.join] reports as [YabiirException.Io].
     */
    @Test
    fun `a dropped completer fails the commit`() {
        val future = CompletableFuture<Long?>()
        val commit = Commit(future)
        future.completeExceptionally(IllegalStateException("the writer stopped"))
        assertThrows<YabiirException.Io> { commit.join() }
    }

    @Test
    fun `a ready commit needs no writer`() {
        assertThrows<YabiirException.EmptyKey> { Commit.failed(YabiirException.EmptyKey()).join() }
    }

    @Test
    fun `conditions compare against the current version`() {
        assertTrue(Cond.Absent.holds(null))
        assertFalse(Cond.Absent.holds(1))
        assertTrue(Cond.Exists.holds(1))
        assertFalse(Cond.Exists.holds(null))
        assertTrue(Cond.Version(3).holds(3))
        assertFalse(Cond.Version(3).holds(4))
        assertFalse(Cond.Version(3).holds(null))
    }
}
