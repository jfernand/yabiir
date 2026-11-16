package io.github.jfernand.yabiir.difftest

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import java.io.RandomAccessFile
import java.nio.file.Files
import java.nio.file.Path
import java.util.Arrays

/**
 * Runs a trace of commands against two drivers, each on its own copy of a
 * store, and checks after every command that they replied the same and
 * left the same bytes on disk.
 *
 * Besides the `yabiir-diffd` commands, a trace has commands the harness
 * carries out itself, on both directories alike:
 *
 * - `{"cmd":"cut","file":i,"at":n}`: drop both stores (no fsync, like a
 *   crash), cut the `i`th data file (mod the count) to `n` bytes (mod its
 *   length), and reopen both;
 * - `{"cmd":"flip","file":i,"at":n}`: the same, flipping a bit of byte `n`
 *   instead;
 * - `{"cmd":"swap"}`: close both, give each driver the other's directory,
 *   and reopen both: each implementation reads what the other wrote.
 *
 * `{DIR}` in a command stands for the driver's directory.
 */
class Harness(private val a: Driver, private val b: Driver, root: Path) {
    private var dirA: Path = root.resolve("a").also { Files.createDirectories(it) }
    private var dirB: Path = root.resolve("b").also { Files.createDirectories(it) }

    /** Every command run so far, as given: a replayable trace. */
    val trace = ArrayList<JsonObject>()

    /** The last `open`, to reopen with after `cut`, `flip` and `swap`. */
    private var lastOpen: JsonObject? = null
    private var isOpen = false

    /** Whether the open store keeps its keys sorted, so that listings must come back in order. */
    private val isSorted: Boolean get() = lastOpen?.get("sorted")?.jsonPrimitive?.boolean ?: false

    /** The reply both gave, once they agree. */
    fun run(command: JsonObject): JsonObject {
        trace.add(command)
        return when (command.getValue("cmd").str) {
            "cut", "flip" -> {
                both(DROP)
                isOpen = false
                damage(dirA, command)
                damage(dirB, command)
                checkSameFiles(command)
                reopen()
            }
            "swap" -> {
                if (isOpen) both(CLOSE)
                isOpen = false
                dirA = dirB.also { dirB = dirA }
                reopen()
            }
            else -> {
                val reply = both(command)
                when (command.getValue("cmd").str) {
                    "open" -> if ("ok" in reply) {
                        lastOpen = command
                        isOpen = true
                    }
                    "close", "drop" -> isOpen = false
                }
                reply
            }
        }
    }

    private fun reopen(): JsonObject = lastOpen?.let { both(it).also { r -> isOpen = "ok" in r } } ?: OK

    private fun both(command: JsonObject): JsonObject {
        val ra = a.send(command.forDir(dirA))
        val rb = b.send(command.forDir(dirB))
        check(normalize(command, ra) == normalize(command, rb)) {
            "replies differ for $command\n  ${a.name}: $ra\n  ${b.name}: $rb"
        }
        check("protocol" !in ra) { "$command: ${a.name} didn't understand it: $ra" }
        if (isSorted) {
            check(isInOrder(command, ra) && isInOrder(command, rb)) { "listing out of order for $command: $ra / $rb" }
        }
        checkSameFiles(command)
        return ra
    }

    /** The two directories hold the same files, byte for byte (the lock file aside). */
    private fun checkSameFiles(command: JsonObject) {
        val fa = contents(dirA)
        val fb = contents(dirB)
        check(fa.keys == fb.keys) { "after $command the files differ:\n  ${a.name}: ${fa.keys}\n  ${b.name}: ${fb.keys}" }
        for ((name, bytes) in fa) {
            val other = fb.getValue(name)
            val at = Arrays.mismatch(bytes, other)
            check(at < 0) {
                "after $command, $name differs at byte $at (${a.name}: ${bytes.size} bytes, ${b.name}: ${other.size})"
            }
        }
    }

    private fun damage(dir: Path, command: JsonObject) {
        val files = contents(dir).keys.filter { it.endsWith(".bitcask.data") }
        if (files.isEmpty()) return
        val path = dir.resolve(files[command.getValue("file").jsonPrimitive.int.mod(files.size)])
        RandomAccessFile(path.toFile(), "rw").use { f ->
            if (f.length() == 0L) return
            val at = command.getValue("at").jsonPrimitive.long.mod(f.length())
            if (command.getValue("cmd").str == "cut") {
                f.setLength(at)
            } else {
                f.seek(at)
                val byte = f.read()
                f.seek(at)
                f.write(byte xor 0x10)
            }
        }
    }

    companion object {
        val DROP = buildJsonObject { put("cmd", JsonPrimitive("drop")) }
        val CLOSE = buildJsonObject { put("cmd", JsonPrimitive("close")) }
        val OK = buildJsonObject { put("ok", JsonNull) }

        fun contents(dir: Path): Map<String, ByteArray> =
            Files.list(dir).use { s ->
                s.toList()
                    .filter { it.fileName.toString() != ".bitcask.lock" }
                    .associate { it.fileName.toString() to Files.readAllBytes(it) }
                    .toSortedMap()
            }

        private fun JsonObject.forDir(dir: Path): JsonObject =
            if ("dir" in this) JsonObject(this + ("dir" to JsonPrimitive(dir.toString()))) else this

        /** Listings of a hashed store come in no order: compare them sorted. */
        private fun normalize(command: JsonObject, reply: JsonObject): JsonObject {
            val ok = reply["ok"]
            return if (command.getValue("cmd").str in setOf("list_keys", "fold") && ok is JsonArray) {
                JsonObject(mapOf("ok" to JsonArray(ok.sortedBy { it.toString() })))
            } else {
                reply
            }
        }

        /** Whether a listing's keys are strictly ascending (descending for a reverse page), as unsigned bytes. */
        private fun isInOrder(command: JsonObject, reply: JsonObject): Boolean {
            val ok = reply["ok"] as? JsonArray ?: return true
            val keys =
                when (command.getValue("cmd").str) {
                    "list_keys" -> ok.map { unhex(it.str) }
                    "fold", "scan", "scan_page" -> ok.map { unhex(it.jsonArray[0].str) }
                    else -> return true
                }
            val reverse = command["reverse"]?.jsonPrimitive?.boolean ?: false
            return keys.zipWithNext().all { (x, y) ->
                val c = Arrays.compareUnsigned(x, y)
                if (reverse) c > 0 else c < 0
            }
        }
    }
}

/** A JSON command from `"key": value` pairs. */
fun cmd(name: String, vararg fields: Pair<String, Any?>): JsonObject =
    JsonObject(mapOf("cmd" to JsonPrimitive(name)) + fields.associate { (k, v) -> k to json(v) })

fun json(v: Any?): JsonElement =
    when (v) {
        null -> JsonNull
        is JsonElement -> v
        is String -> JsonPrimitive(v)
        is Number -> JsonPrimitive(v)
        is Boolean -> JsonPrimitive(v)
        is ByteArray -> JsonPrimitive(hex(v))
        is List<*> -> JsonArray(v.map(::json))
        is Map<*, *> -> JsonObject(v.entries.associate { (k, x) -> k as String to json(x) })
        else -> error("can't encode $v")
    }
