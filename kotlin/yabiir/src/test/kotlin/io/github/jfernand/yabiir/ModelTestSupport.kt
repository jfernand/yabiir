package io.github.jfernand.yabiir

import net.jqwik.api.Arbitraries
import net.jqwik.api.Arbitrary
import net.jqwik.api.Combinators
import net.jqwik.api.Tuple
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.assertThrows
import java.nio.file.Files
import java.nio.file.Path
import java.util.TreeMap

/** One step of a model run: Rust's `Op` in `tests/model.rs`. */
internal sealed interface ModelOp {
    data class Put(val key: String, val value: List<Byte>) : ModelOp

    data class Delete(val key: String) : ModelOp

    data class Get(val key: String) : ModelOp

    data object Reopen : ModelOp

    data object Merge : ModelOp

    data class Batch(val ops: List<ModelBatchOp>) : ModelOp
}

/** One operation of a write batch, before versions are known: a version check is against the key's current version, or a stale one. */
internal sealed interface ModelBatchOp {
    val key: String

    data class Put(override val key: String, val value: List<Byte>) : ModelBatchOp

    data class Delete(override val key: String) : ModelBatchOp

    data class CheckAbsent(override val key: String) : ModelBatchOp

    data class CheckExists(override val key: String) : ModelBatchOp

    data class CheckCurrentVersion(override val key: String) : ModelBatchOp

    data class CheckStaleVersion(override val key: String) : ModelBatchOp
}

/** How the store is opened across the run: always one order, or switching order at every reopen. */
internal sealed interface Orders {
    data class Always(val order: KeyOrder) : Orders

    data object Alternating : Orders
}

/** The generators, with Rust's weights and sizes. */
internal object ModelArbitraries {
    /** A small, overlapping key alphabet (single bytes 'a'..='f'), so writes on one key collide constantly. */
    fun keys(): Arbitrary<String> = Arbitraries.of("a", "b", "c", "d", "e", "f")

    /** 0 to 7 arbitrary bytes. */
    fun values(): Arbitrary<List<Byte>> = Arbitraries.bytes().list().ofMaxSize(7)

    fun batchOps(): Arbitrary<ModelBatchOp> =
        Arbitraries.frequencyOf(
            Tuple.of(4, Combinators.combine(keys(), values()).`as` { k, v -> ModelBatchOp.Put(k, v) }),
            Tuple.of(2, keys().map { ModelBatchOp.Delete(it) }),
            Tuple.of(1, keys().map { ModelBatchOp.CheckAbsent(it) }),
            Tuple.of(1, keys().map { ModelBatchOp.CheckExists(it) }),
            Tuple.of(2, keys().map { ModelBatchOp.CheckCurrentVersion(it) }),
            Tuple.of(1, keys().map { ModelBatchOp.CheckStaleVersion(it) }),
        )

    fun ops(): Arbitrary<ModelOp> =
        Arbitraries.frequencyOf(
            Tuple.of(4, Combinators.combine(keys(), values()).`as` { k, v -> ModelOp.Put(k, v) }),
            Tuple.of(4, keys().map { ModelOp.Get(it) }),
            Tuple.of(2, keys().map { ModelOp.Delete(it) }),
            Tuple.of(1, Arbitraries.just(ModelOp.Reopen)),
            Tuple.of(1, Arbitraries.just(ModelOp.Merge)),
            Tuple.of(3, batchOps().list().ofMaxSize(4).map { ModelOp.Batch(it) }),
        )

    /** Rust's `prop::collection::vec(op_strategy(), 1..40)`. */
    fun runs(): Arbitrary<List<ModelOp>> = ops().list().ofMinSize(1).ofMaxSize(39)
}

private fun bytes(key: String): ByteArray = key.toByteArray(Charsets.ISO_8859_1)

private fun string(bytes: ByteArray): String = String(bytes, Charsets.ISO_8859_1)

/** Small `maxFileSize` so a typical run exercises rotation and gives merge non-trivial input. */
private fun smallFileOptions(order: KeyOrder) = Options(maxFileSize = 200, keyOrder = order)

/** A key/value/version row, comparable with `assertEquals`. */
internal data class Row(val key: String, val value: List<Byte>, val version: Long)

/**
 * Rust's `run_model`: run [ops] against a store in a fresh directory and a
 * reference map, checking every `Get` along the way and everything at the end.
 */
internal fun runModel(ops: List<ModelOp>, orders: Orders) {
    val dir = Files.createTempDirectory("yabiir-model-")
    try {
        runModelIn(dir, ops, orders)
    } finally {
        dir.toFile().deleteRecursively()
    }
}

private fun runModelIn(dir: Path, ops: List<ModelOp>, orders: Orders) {
    // Keys are single bytes 'a'..'f', so String order is the store's unsigned byte order.
    val model = TreeMap<String, List<Byte>>()
    // Each live key's version, as the store reported it.
    val versions = TreeMap<String, Long>()
    var order =
        when (orders) {
            is Orders.Always -> orders.order
            Orders.Alternating -> KeyOrder.HASHED
        }
    var db = Engine.open(dir, smallFileOptions(order))
    try {
        for (op in ops) {
            when (op) {
                is ModelOp.Put -> {
                    db.put(bytes(op.key), op.value.toByteArray(), nowUnix())
                    val version = db.getVersioned(bytes(op.key))!!.version
                    val old = versions[op.key]
                    assertTrue(old == null || version > old, "version $version after $old")
                    versions[op.key] = version
                    model[op.key] = op.value
                }
                is ModelOp.Delete -> {
                    db.delete(bytes(op.key), nowUnix())
                    versions.remove(op.key)
                    model.remove(op.key)
                }
                is ModelOp.Batch -> runBatch(db, op.ops, model, versions)
                is ModelOp.Get -> {
                    val got = db.getVersioned(bytes(op.key))
                    assertEquals(model[op.key], got?.value?.toList())
                    assertEquals(versions[op.key], got?.version)
                }
                ModelOp.Reopen -> {
                    db.close()
                    if (orders == Orders.Alternating) {
                        order = if (order == KeyOrder.HASHED) KeyOrder.SORTED else KeyOrder.HASHED
                    }
                    db = Engine.open(dir, smallFileOptions(order))
                }
                ModelOp.Merge -> db.merge()
            }
        }

        // Final full comparison: catches anything a Get happened not to probe.
        val keys = db.listKeys().map { string(it) }
        val modelKeys = model.keys.toList()
        if (order == KeyOrder.SORTED) {
            // A sorted keydir lists keys in order, and scans them with values.
            assertEquals(modelKeys, keys)
            assertEquals(
                model.map { (k, v) -> k to v },
                db.scan(Bound.Unbounded, Bound.Unbounded).map { (k, v) -> string(k) to v.toList() },
            )
            // Paged, both ways, with versions.
            for (limit in 1..3) {
                for (direction in Direction.entries) {
                    var want = model.map { (k, v) -> Row(k, v, versions.getValue(k)) }
                    if (direction == Direction.REVERSE) want = want.reversed()
                    assertEquals(want, scanAllPages(db, direction, limit), "$direction, $limit at a time")
                }
            }
        }
        assertEquals(modelKeys, keys.sorted())
        for (k in keys) assertEquals(model[k], db.get(bytes(k))?.toList())
    } finally {
        db.close()
    }
}

/** Every live key, value and version, read [limit] at a time. */
private fun scanAllPages(db: Engine, direction: Direction, limit: Int): List<Row> {
    val all = ArrayList<Row>()
    var after: ByteArray? = null
    while (true) {
        val (start, end) =
            when {
                after == null -> Bound.Unbounded to Bound.Unbounded
                direction == Direction.FORWARD -> Bound.Excluded(after) to Bound.Unbounded
                else -> Bound.Unbounded to Bound.Excluded(after)
            }
        val page = db.scanPage(start, end, direction, limit)
        val isLast = page.size < limit
        after = page.lastOrNull()?.first
        page.mapTo(all) { (k, found) -> Row(string(k), found.value.toList(), found.version) }
        if (isLast) return all
    }
}

/** Run one batch against the store and the model: the checks against the state before the batch, then the writes in order, all or nothing. */
private fun runBatch(
    db: Engine,
    batch: List<ModelBatchOp>,
    model: TreeMap<String, List<Byte>>,
    versions: TreeMap<String, Long>,
) {
    val ops = ArrayList<Op>()
    var firstFailure: Int? = null
    for ((index, op) in batch.withIndex()) {
        val current = versions[op.key]
        val key = bytes(op.key)
        val (made, holds) =
            when (op) {
                is ModelBatchOp.Put -> Op.Put(key, op.value.toByteArray()) to true
                is ModelBatchOp.Delete -> Op.Delete(key) to true
                is ModelBatchOp.CheckAbsent -> Op.Check(key, Cond.Absent) to (current == null)
                is ModelBatchOp.CheckExists -> Op.Check(key, Cond.Exists) to (current != null)
                // Absent keys have no version: check one that can't match.
                is ModelBatchOp.CheckCurrentVersion ->
                    if (current != null) Op.Check(key, Cond.Version(current)) to true else Op.Check(key, Cond.Version(1)) to false
                is ModelBatchOp.CheckStaleVersion -> Op.Check(key, Cond.Version(current?.minus(1) ?: 1)) to false
            }
        if (!holds && firstFailure == null) firstFailure = index
        ops.add(made)
    }

    val commit = db.writeBatch(ops)
    if (firstFailure != null) {
        val e = assertThrows<YabiirException.ConditionFailed> { commit.join() }
        assertEquals(firstFailure, e.index, "expected check $firstFailure to fail")
        return
    }
    val written = commit.join()
    var wrote = false
    for (op in batch) {
        when (op) {
            is ModelBatchOp.Put -> {
                wrote = true
                versions[op.key] = checkNotNull(written) { "a put writes" }
                model[op.key] = op.value
            }
            is ModelBatchOp.Delete -> {
                wrote = wrote or (model.remove(op.key) != null)
                versions.remove(op.key)
            }
            else -> {}
        }
    }
    assertEquals(wrote, written != null, "$written")
}
