package io.github.jfernand.yabiir

import java.nio.file.Path
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * A store whose every `put` rotates into its own file, holding `K` and one
 * other key, so `merge` has immutable input files to compact and remove.
 */
internal fun storeForMergeRace(dir: Path): Engine {
    // maxFileSize = 1: every put rotates into its own file
    val db = Engine.open(dir, Options(maxFileSize = 1))
    db.put("K".toByteArray(), "v".toByteArray(), nowUnix())
    db.put("other".toByteArray(), "x".toByteArray(), nowUnix())
    return db
}

/**
 * A hook for a write's append-to-publish window, which runs on the writer
 * thread: it starts [action] on another thread and gives it up to a second
 * to finish *inside* the window. Port of Rust's `merge_inside_window` and
 * `write_inside_window`: the verdict doesn't depend on timing, since an
 * action that can't finish inside the window can't cause the race either.
 *
 * [awaitDone] stands in for Rust's `wait_for_other_handles`: it waits (up to
 * ten seconds) for the thread the window started, and rethrows what it threw.
 */
internal class InsideWindow(private val action: () -> Unit) {
    private val started = CountDownLatch(1)
    private val done = CountDownLatch(1)
    private val failure = AtomicReference<Throwable?>()
    private var thread: Thread? = null

    val hook: () -> Unit = {
        thread =
            Thread {
                try {
                    action()
                } catch (e: Throwable) {
                    failure.set(e)
                } finally {
                    done.countDown()
                }
            }.apply { start() }
        started.countDown()
        done.await(1, TimeUnit.SECONDS)
    }

    fun awaitDone() {
        check(started.await(10, TimeUnit.SECONDS)) { "the window never ran" }
        check(done.await(10, TimeUnit.SECONDS)) { "the second write never finished" }
        thread!!.join()
        failure.get()?.let { throw AssertionError("the action inside the window failed", it) }
    }
}

/** `merge_inside_window`: merge [db] from inside the window. */
internal fun mergeInsideWindow(db: Engine): InsideWindow = InsideWindow { db.merge() }

/** `write_inside_window`: run [write] from inside the window. */
internal fun writeInsideWindow(write: () -> Unit): InsideWindow = InsideWindow(write)
