package io.github.jfernand.yabiir.difftest

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import net.jqwik.api.Arbitraries
import net.jqwik.api.Arbitrary
import net.jqwik.api.Combinators
import net.jqwik.api.ForAll
import net.jqwik.api.Property
import net.jqwik.api.Provide
import net.jqwik.api.Tuple
import org.junit.jupiter.api.DynamicTest
import org.junit.jupiter.api.TestFactory
import java.nio.file.Files
import java.nio.file.Path

/** One generated step; [run] turns it into commands, given the store as the model knows it. */
sealed interface Step {
    data class Put(val k: Bytes, val v: Bytes) : Step

    data class Delete(val k: Bytes) : Step

    data class Get(val k: Bytes, val versioned: Boolean) : Step

    data class Batch(val ops: List<BatchOp>) : Step

    data object ListKeys : Step

    data object Fold : Step

    data class Scan(val start: Bnd, val end: Bnd) : Step

    data class ScanPage(val start: Bnd, val end: Bnd, val reverse: Boolean, val limit: Int) : Step

    data object Sync : Step

    data object Merge : Step

    /** Close (or drop, without an fsync) and open again, maybe in another key order or read-only. */
    data class Reopen(val clean: Boolean, val sorted: Boolean, val readOnly: Boolean) : Step

    data class Cut(val file: Int, val at: Long) : Step

    data class Flip(val file: Int, val at: Long) : Step

    data object Swap : Step
}

sealed interface BatchOp {
    data class Put(val k: Bytes, val v: Bytes) : BatchOp

    data class Delete(val k: Bytes) : BatchOp

    data class Absent(val k: Bytes) : BatchOp

    data class Exists(val k: Bytes) : BatchOp

    /** A check against the version the key has now, looked up when the step runs. */
    data class Current(val k: Bytes) : BatchOp

    data class Version(val k: Bytes, val version: Long) : BatchOp
}

/** A bound: `null` key is unbounded. */
data class Bnd(val k: Bytes?, val inclusive: Boolean)

/** Bytes with value equality and a readable form, for jqwik's reports. */
class Bytes(val b: ByteArray) {
    override fun equals(other: Any?) = other is Bytes && b.contentEquals(other.b)

    override fun hashCode() = b.contentHashCode()

    override fun toString() = b.joinToString("", "\"", "\"") { if (it in 0x61..0x7a) it.toInt().toChar().toString() else "\\x%02x".format(it) }
}

data class Run(
    val maxFileSize: Long,
    val sorted: Boolean,
    val syncOnPut: Boolean,
    val seedV1: List<Triple<Bytes, Bytes, Boolean>>,
    val steps: List<Step>,
)

class DifferentialTest {
    @Property(shrinking = net.jqwik.api.ShrinkingMode.FULL)
    fun `the Kotlin and Rust engines agree on every reply and every byte`(@ForAll("runs") run: Run) {
        val root = Files.createTempDirectory("yabiir-diff")
        val kotlin = KotlinDriver()
        rust.send(Harness.DROP)
        val harness = Harness(kotlin, rust, root)
        try {
            Session(harness, run).go()
        } catch (e: Throwable) {
            saveFailure(harness.trace)
            throw e
        } finally {
            kotlin.close()
            rust.send(Harness.DROP)
        }
        root.toFile().deleteRecursively()
    }

    /** Every trace in `src/test/resources/corpus`: fixed regressions, replayed exactly. */
    @TestFactory
    fun corpus(): List<DynamicTest> {
        val dir = Path.of(javaClass.getResource("/corpus")?.toURI() ?: return emptyList())
        return Files.list(dir).use { it.toList() }.filter { it.toString().endsWith(".jsonl") }.sorted().map { file ->
            DynamicTest.dynamicTest(file.fileName.toString()) {
                val root = Files.createTempDirectory("yabiir-corpus")
                KotlinDriver().use { kotlin ->
                    rust.send(Harness.DROP)
                    val harness = Harness(kotlin, rust, root)
                    Files.readAllLines(file).filter { it.isNotBlank() }.forEach {
                        harness.run(Json.parseToJsonElement(it).jsonObject)
                    }
                    rust.send(Harness.DROP)
                }
                root.toFile().deleteRecursively()
            }
        }
    }

    @Provide
    fun runs(): Arbitrary<Run> {
        val seed = Combinators.combine(keys(), values(), Arbitraries.of(false, false, true)).`as` { k, v, t -> Triple(k, v, t) }
            .list().ofMaxSize(6)
        return Combinators.combine(
            Arbitraries.of(1L, 64L, 200L, 200L, 4096L),
            Arbitraries.of(true, false),
            Arbitraries.of(false, false, false, true),
            Arbitraries.frequencyOf(Tuple.of(3, Arbitraries.just(emptyList())), Tuple.of(1, seed)),
            steps().list().ofMaxSize(60),
        ).`as` { size, sorted, sync, v1, steps -> Run(size, sorted, sync, v1, steps) }
    }

    private fun keys(): Arbitrary<Bytes> =
        Arbitraries.frequencyOf(
            Tuple.of(8, Arbitraries.of(*"abcdef".toCharArray().toTypedArray()).map { Bytes(byteArrayOf(it.code.toByte())) }),
            Tuple.of(
                1,
                Arbitraries.of(0x00.toByte(), 0x61.toByte(), 0x62.toByte(), 0xff.toByte())
                    .list().ofMinSize(1).ofMaxSize(3).map { Bytes(it.toByteArray()) },
            ),
        )

    private fun values(): Arbitrary<Bytes> =
        Arbitraries.frequencyOf(
            Tuple.of(6, Arbitraries.bytes().array(ByteArray::class.java).ofMaxSize(8)),
            Tuple.of(1, Arbitraries.bytes().array(ByteArray::class.java).ofMaxSize(300)),
        ).map(::Bytes)

    private fun bounds(): Arbitrary<Bnd> =
        Arbitraries.frequencyOf(
            Tuple.of(1, Arbitraries.just(Bnd(null, false))),
            Tuple.of(3, Combinators.combine(keys(), Arbitraries.of(true, false)).`as` { k, i -> Bnd(k, i) }),
        )

    private fun batchOps(): Arbitrary<BatchOp> =
        Arbitraries.frequencyOf(
            Tuple.of(4, Combinators.combine(keys(), values()).`as` { k, v -> BatchOp.Put(k, v) }),
            Tuple.of(2, keys().map { BatchOp.Delete(it) }),
            Tuple.of(1, keys().map { BatchOp.Absent(it) }),
            Tuple.of(1, keys().map { BatchOp.Exists(it) }),
            Tuple.of(2, keys().map { BatchOp.Current(it) }),
            Tuple.of(1, Combinators.combine(keys(), Arbitraries.longs().between(0, 60)).`as` { k, v -> BatchOp.Version(k, v) }),
        )

    private fun steps(): Arbitrary<Step> =
        Arbitraries.frequencyOf(
            Tuple.of(8, Combinators.combine(keys(), values()).`as` { k, v -> Step.Put(k, v) }),
            Tuple.of(4, keys().map { Step.Delete(it) }),
            Tuple.of(4, Combinators.combine(keys(), Arbitraries.of(true, false)).`as` { k, v -> Step.Get(k, v) }),
            Tuple.of(6, batchOps().list().ofMaxSize(6).map { Step.Batch(it) }),
            Tuple.of(1, Arbitraries.just(Step.ListKeys)),
            Tuple.of(1, Arbitraries.just(Step.Fold)),
            Tuple.of(1, Combinators.combine(bounds(), bounds()).`as` { s, e -> Step.Scan(s, e) }),
            Tuple.of(
                2,
                Combinators.combine(bounds(), bounds(), Arbitraries.of(true, false), Arbitraries.integers().between(0, 5))
                    .`as` { s, e, r, l -> Step.ScanPage(s, e, r, l) },
            ),
            Tuple.of(1, Arbitraries.just(Step.Sync)),
            Tuple.of(2, Arbitraries.just(Step.Merge)),
            Tuple.of(
                2,
                Combinators.combine(Arbitraries.of(true, false), Arbitraries.of(true, false), Arbitraries.of(false, false, false, true))
                    .`as` { c, s, r -> Step.Reopen(c, s, r) },
            ),
            Tuple.of(1, Combinators.combine(Arbitraries.integers().between(0, 9), Arbitraries.longs().between(0, 5000)).`as` { f, a -> Step.Cut(f, a) }),
            Tuple.of(1, Combinators.combine(Arbitraries.integers().between(0, 9), Arbitraries.longs().between(0, 5000)).`as` { f, a -> Step.Flip(f, a) }),
            Tuple.of(1, Arbitraries.just(Step.Swap)),
        )

    companion object {
        private val rust: RustDriver by lazy {
            val binary = System.getProperty("yabiir.diffd") ?: error("set yabiir.diffd to the yabiir-diffd binary")
            RustDriver(binary).also { d -> Runtime.getRuntime().addShutdownHook(Thread { d.close() }) }
        }

        private var shortestSaved = Int.MAX_VALUE

        /** Keep the shortest failing trace (the one jqwik shrinks to), to replay or add to the corpus. */
        private fun saveFailure(trace: List<JsonObject>) {
            if (trace.size > shortestSaved) return
            shortestSaved = trace.size
            val dir = Path.of(System.getProperty("difftest.failures") ?: "build/failures")
            Files.createDirectories(dir)
            Files.write(dir.resolve("shortest.jsonl"), trace.map { it.toString() })
        }
    }
}

/**
 * One run: the steps, sent through the harness, and a model of what the
 * store holds, checked against the replies.
 */
private class Session(private val h: Harness, private val run: Run) {
    /** hex key -> hex value. Unknown after damage until resynced. */
    private val model = HashMap<String, String>()
    private var sorted = run.sorted
    private var readOnly = false

    /** The highest version a write returned since the last damage: every later write's must be higher. */
    private var maxVersion = 0L
    private var ts = 1_000L

    /**
     * Whether the model still describes the store. After damage (a cut or a
     * flipped bit) it doesn't: what recovery and merge make of damaged files
     * is checked by the two engines agreeing, not by the model. (Merge can
     * drop a value that recovery took from a valid hint file when the data
     * file's header is damaged, for example; both engines do.)
     */
    private var isModelTrusted = true

    fun go() {
        if (run.seedV1.isNotEmpty()) {
            val entries = run.seedV1.map { (k, v, tomb) -> mapOf("k" to k.b, "v" to if (tomb) ByteArray(0) else v.b, "tombstone" to tomb, "ts" to 7) }
            h.run(cmd("seed_v1", "dir" to "{DIR}", "file_id" to 0, "entries" to entries))
            for ((k, v, tomb) in run.seedV1) if (tomb) model.remove(hex(k.b)) else model[hex(k.b)] = hex(v.b)
        }
        open(sorted, readOnly = false)
        for (step in run.steps) step(step)
        h.run(cmd("close"))
        // The store reopens to the same contents.
        open(sorted, readOnly = true)
        checkFold()
        h.run(cmd("drop"))
    }

    private fun open(sorted: Boolean, readOnly: Boolean) {
        this.sorted = sorted
        this.readOnly = readOnly
        val reply = h.run(cmd("open", "dir" to "{DIR}", "max_file_size" to run.maxFileSize, "sync_on_put" to run.syncOnPut, "sorted" to sorted, "read_only" to readOnly))
        check("ok" in reply) { "open failed: $reply" }
    }

    private fun write(command: JsonObject): JsonObject {
        val reply = h.run(command)
        if (readOnly) check(reply["err"]?.str == "ReadOnly") { "a write to a read-only store: $reply" }
        return reply
    }

    private fun step(step: Step) {
        ts++
        when (step) {
            is Step.Put -> {
                if ("ok" in write(cmd("put", "k" to step.k.b, "v" to step.v.b, "ts" to ts))) model[hex(step.k.b)] = hex(step.v.b)
                if (!readOnly) checkVersion(step.k)
            }
            is Step.Delete -> if ("ok" in write(cmd("delete", "k" to step.k.b, "ts" to ts))) model.remove(hex(step.k.b))
            is Step.Get -> get(step.k, step.versioned)
            is Step.Batch -> batch(step)
            Step.ListKeys -> {
                val reply = h.run(cmd("list_keys"))
                check(!isModelTrusted || reply.getValue("ok").jsonArray.map { it.str }.toSet() == model.keys) { "list_keys: $reply vs model ${model.keys}" }
            }
            Step.Fold -> checkFold()
            is Step.Scan -> {
                val reply = h.run(cmd("scan", "start" to step.start.json(), "end" to step.end.json()))
                if (!sorted) check(reply["err"]?.str == "Unordered") { "scan of a hashed store: $reply" }
            }
            is Step.ScanPage -> {
                val reply = h.run(cmd("scan_page", "start" to step.start.json(), "end" to step.end.json(), "reverse" to step.reverse, "limit" to step.limit))
                if (!sorted) check(reply["err"]?.str == "Unordered") { "scan_page of a hashed store: $reply" }
                reply["ok"]?.jsonArray?.forEach { check(!isModelTrusted || model[it.jsonArray[0].str] == it.jsonArray[1].str) { "scan_page: $reply" } }
            }
            Step.Sync -> h.run(cmd("sync"))
            Step.Merge -> {
                val reply = h.run(cmd("merge"))
                if (readOnly) check(reply["err"]?.str == "ReadOnly")
            }
            is Step.Reopen -> {
                h.run(cmd(if (step.clean) "close" else "drop"))
                open(step.sorted, step.readOnly)
            }
            is Step.Cut -> damage(cmd("cut", "file" to step.file, "at" to step.at))
            is Step.Flip -> damage(cmd("flip", "file" to step.file, "at" to step.at))
            Step.Swap -> h.run(cmd("swap"))
        }
    }

    private fun get(k: Bytes, versioned: Boolean) {
        val reply = h.run(cmd(if (versioned) "get_versioned" else "get", "k" to k.b))
        val ok = reply["ok"] ?: return check(!isModelTrusted) { "get ${hex(k.b)}: $reply" }
        val value = if (ok is JsonNull) null else if (versioned) ok.jsonObject.getValue("value").str else ok.str
        check(!isModelTrusted || value == model[hex(k.b)]) { "get ${hex(k.b)}: $reply, model ${model[hex(k.b)]}" }
    }

    private fun currentVersion(k: Bytes): Long? =
        (h.run(cmd("get_versioned", "k" to k.b))["ok"] as? JsonObject)?.getValue("version")?.jsonPrimitive?.long

    private fun checkVersion(k: Bytes) {
        val v = currentVersion(k) ?: return
        check(v > maxVersion) { "version $v of ${hex(k.b)} isn't above $maxVersion" }
        maxVersion = v
    }

    private fun batch(step: Step.Batch) {
        val ops =
            step.ops.map { op ->
                when (op) {
                    is BatchOp.Put -> mapOf("op" to "put", "k" to op.k.b, "v" to op.v.b)
                    is BatchOp.Delete -> mapOf("op" to "delete", "k" to op.k.b)
                    is BatchOp.Absent -> mapOf("op" to "absent", "k" to op.k.b)
                    is BatchOp.Exists -> mapOf("op" to "exists", "k" to op.k.b)
                    is BatchOp.Current -> mapOf("op" to "version", "k" to op.k.b, "version" to (currentVersion(op.k) ?: 0L))
                    is BatchOp.Version -> mapOf("op" to "version", "k" to op.k.b, "version" to op.version)
                }
            }
        val reply = write(cmd("batch", "ops" to ops, "ts" to ts))
        val ok = reply["ok"] ?: return
        for (op in step.ops) {
            when (op) {
                is BatchOp.Put -> model[hex(op.k.b)] = hex(op.v.b)
                is BatchOp.Delete -> model.remove(hex(op.k.b))
                else -> {}
            }
        }
        if (ok is JsonPrimitive && ok !is JsonNull) {
            check(ok.long > maxVersion) { "batch version ${ok.long} isn't above $maxVersion" }
            maxVersion = ok.long
        }
    }

    private fun checkFold() {
        val reply = h.run(cmd("fold"))
        if (!isModelTrusted) return
        val pairs = reply.getValue("ok").jsonArray.associate { it.jsonArray[0].str to it.jsonArray[1].str }
        check(!isModelTrusted || pairs == model) { "fold: $pairs\nmodel: $model" }
    }

    /** After damage only the engines' agreement is checked: see [isModelTrusted]. */
    private fun damage(command: JsonObject) {
        h.run(command)
        isModelTrusted = false
        h.run(cmd("fold"))
        maxVersion = 0
    }

    private fun Bnd.json(): Any? = k?.let { mapOf((if (inclusive) "incl" else "excl") to it.b) }
}
