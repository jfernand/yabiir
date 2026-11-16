package io.github.jfernand.yabiir.difftest

import io.github.jfernand.yabiir.Bound
import io.github.jfernand.yabiir.Cond
import io.github.jfernand.yabiir.Direction
import io.github.jfernand.yabiir.Engine
import io.github.jfernand.yabiir.KeyOrder
import io.github.jfernand.yabiir.Op
import io.github.jfernand.yabiir.Options
import io.github.jfernand.yabiir.YabiirException
import io.github.jfernand.yabiir.datafile.dataPath
import io.github.jfernand.yabiir.format.encodeEntry
import kotlinx.serialization.json.Json
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
import kotlinx.serialization.json.put
import java.io.BufferedReader
import java.io.BufferedWriter
import java.nio.file.Files
import java.nio.file.Path

/** Runs one command of the `yabiir-diffd` protocol (see `crates/yabiir-difftest/src/protocol.rs`). */
interface Driver : AutoCloseable {
    val name: String

    fun send(command: JsonObject): JsonObject
}

fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

fun unhex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

val JsonElement.str: String get() = jsonPrimitive.content

/** The Rust engine, in a `yabiir-diffd` process. */
class RustDriver(binary: String) : Driver {
    override val name = "rust"
    private val process = ProcessBuilder(binary).redirectError(ProcessBuilder.Redirect.INHERIT).start()
    private val input: BufferedWriter = process.outputStream.bufferedWriter()
    private val output: BufferedReader = process.inputStream.bufferedReader()

    override fun send(command: JsonObject): JsonObject {
        input.write(command.toString())
        input.newLine()
        input.flush()
        val line = output.readLine() ?: error("yabiir-diffd exited (status ${process.waitFor()})")
        return Json.parseToJsonElement(line).jsonObject
    }

    override fun close() {
        input.close()
        process.waitFor()
    }
}

/** The Kotlin engine, in this process, behind the same protocol. */
class KotlinDriver : Driver {
    override val name = "kotlin"
    private var store: Engine? = null

    override fun close() {
        store?.closeWithoutSync()
        store = null
    }

    override fun send(command: JsonObject): JsonObject =
        try {
            buildJsonObject { put("ok", run(command)) }
        } catch (e: YabiirException) {
            buildJsonObject {
                put("err", e.variant)
                if (e is YabiirException.ConditionFailed) put("index", e.index)
                if (e is YabiirException.Io) put("kind", e.kind)
            }
        } catch (e: ProtocolError) {
            buildJsonObject { put("protocol", e.message) }
        }

    private class ProtocolError(message: String) : Exception(message)

    private fun store(): Engine = store ?: throw ProtocolError("no store is open")

    private fun run(c: JsonObject): JsonElement {
        fun bytes(field: String) = unhex(c.getValue(field).str)
        fun flag(field: String) = c[field]?.jsonPrimitive?.boolean ?: false
        val ts = { c.getValue("ts").jsonPrimitive.long }
        return when (val cmd = c.getValue("cmd").str) {
            "open" -> {
                if (store != null) throw ProtocolError("a store is already open")
                val opts =
                    Options(
                        isReadWrite = !flag("read_only"),
                        shouldSyncOnPut = flag("sync_on_put"),
                        maxFileSize = c.getValue("max_file_size").jsonPrimitive.long,
                        keyOrder = if (flag("sorted")) KeyOrder.SORTED else KeyOrder.HASHED,
                    )
                store = Engine.open(Path.of(c.getValue("dir").str), opts)
                JsonNull
            }
            "close" -> {
                val s = store ?: throw ProtocolError("no store is open")
                store = null
                s.close()
                JsonNull
            }
            "drop" -> {
                store?.closeWithoutSync()
                store = null
                JsonNull
            }
            "put" -> {
                store().put(bytes("k"), bytes("v"), ts())
                JsonNull
            }
            "delete" -> {
                store().delete(bytes("k"), ts())
                JsonNull
            }
            "batch" -> {
                val ops = c.getValue("ops").jsonArray.map { op(it.jsonObject) }
                store().writeBatchAt(ops, ts()).join()?.let { JsonPrimitive(it) } ?: JsonNull
            }
            "get" -> store().get(bytes("k"))?.let { JsonPrimitive(hex(it)) } ?: JsonNull
            "get_versioned" ->
                store().getVersioned(bytes("k"))?.let {
                    buildJsonObject {
                        put("value", hex(it.value))
                        put("version", it.version)
                    }
                } ?: JsonNull
            "list_keys" -> JsonArray(store().listKeys().map { JsonPrimitive(hex(it)) })
            "fold" ->
                JsonArray(
                    store().fold(ArrayList<JsonElement>()) { k, v, acc ->
                        acc.add(JsonArray(listOf(JsonPrimitive(hex(k)), JsonPrimitive(hex(v)))))
                        acc
                    },
                )
            "scan" ->
                JsonArray(
                    store().scan(bound(c["start"]), bound(c["end"])).map { (k, v) ->
                        JsonArray(listOf(JsonPrimitive(hex(k)), JsonPrimitive(hex(v))))
                    },
                )
            "scan_page" -> {
                val direction = if (flag("reverse")) Direction.REVERSE else Direction.FORWARD
                JsonArray(
                    store().scanPage(bound(c["start"]), bound(c["end"]), direction, c.getValue("limit").jsonPrimitive.int).map {
                        (k, found) ->
                        JsonArray(listOf(JsonPrimitive(hex(k)), JsonPrimitive(hex(found.value)), JsonPrimitive(found.version)))
                    },
                )
            }
            "merge" -> {
                store().merge()
                JsonNull
            }
            "sync" -> {
                store().sync()
                JsonNull
            }
            "seed_v1" -> {
                val dir = Path.of(c.getValue("dir").str)
                Files.createDirectories(dir)
                val out = java.io.ByteArrayOutputStream()
                for (e in c.getValue("entries").jsonArray) {
                    val o = e.jsonObject
                    out.write(
                        encodeEntry(
                            unhex(o.getValue("k").str),
                            o["v"]?.let { unhex(it.str) } ?: ByteArray(0),
                            o["tombstone"]?.jsonPrimitive?.boolean ?: false,
                            o.getValue("ts").jsonPrimitive.long.toInt(),
                        ),
                    )
                }
                Files.write(dataPath(dir, c.getValue("file_id").jsonPrimitive.long), out.toByteArray())
                JsonNull
            }
            else -> throw ProtocolError("bad command: unknown cmd $cmd")
        }
    }

    private fun op(o: JsonObject): Op {
        val k = unhex(o.getValue("k").str)
        return when (o.getValue("op").str) {
            "put" -> Op.Put(k, unhex(o.getValue("v").str))
            "delete" -> Op.Delete(k)
            "absent" -> Op.Check(k, Cond.Absent)
            "exists" -> Op.Check(k, Cond.Exists)
            "version" -> Op.Check(k, Cond.Version(o.getValue("version").jsonPrimitive.long))
            else -> throw ProtocolError("unknown op")
        }
    }

    private fun bound(b: JsonElement?): Bound =
        when {
            b == null || b is JsonNull -> Bound.Unbounded
            "incl" in b.jsonObject -> Bound.Included(unhex(b.jsonObject.getValue("incl").str))
            else -> Bound.Excluded(unhex(b.jsonObject.getValue("excl").str))
        }
}
