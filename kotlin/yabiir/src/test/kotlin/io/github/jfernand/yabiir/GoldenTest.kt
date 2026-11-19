package io.github.jfernand.yabiir

import io.github.jfernand.yabiir.format.FileFormat
import io.github.jfernand.yabiir.format.HINT_MAGIC
import io.github.jfernand.yabiir.format.OpRef
import io.github.jfernand.yabiir.format.RecordRead
import io.github.jfernand.yabiir.format.crc32
import io.github.jfernand.yabiir.format.decodeHintFile
import io.github.jfernand.yabiir.format.encodeEntry
import io.github.jfernand.yabiir.format.encodeFileHeader
import io.github.jfernand.yabiir.format.encodeHintV2
import io.github.jfernand.yabiir.format.encodeRecord
import io.github.jfernand.yabiir.format.identify
import io.github.jfernand.yabiir.format.putU32
import io.github.jfernand.yabiir.format.readEntry
import io.github.jfernand.yabiir.format.readRecord
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.fail
import org.junit.jupiter.api.io.TempDir
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.ValueSource
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import kotlin.io.path.copyTo
import kotlin.io.path.listDirectoryEntries
import kotlin.io.path.name
import kotlin.io.path.readLines

/**
 * Stores the Rust engine wrote (`src/test/resources/golden/`, made by its
 * `regenerate.sh` from the `.jsonl` traces next to them): the Kotlin engine
 * must read what Rust read from them, and its format code must decode every
 * file and encode it back to the same bytes.
 */
class GoldenTest {
    @TempDir
    lateinit var tmp: Path

    private fun golden(name: String): Path = Paths.get(GoldenTest::class.java.getResource("/golden/$name")!!.toURI())

    private fun copyOf(name: String): Path {
        val dir = Files.createDirectory(tmp.resolve(name))
        for (file in golden(name).listDirectoryEntries()) file.copyTo(dir.resolve(file.name))
        return dir
    }

    private fun files(name: String, suffix: String): List<Path> =
        golden(name).listDirectoryEntries("*$suffix").sorted().also { assertTrue(it.isNotEmpty() || suffix == ".hint") }

    private class Expected(val key: String, val value: String, val version: Long)

    private fun expected(name: String): List<Expected> =
        golden("$name.expected").readLines().filter { it.isNotEmpty() }.map {
            val (k, v, version) = it.split(" ")
            Expected(k, v, version.toLong())
        }

    @ParameterizedTest
    @ValueSource(strings = ["v1", "v2", "mixed"])
    fun `a golden store holds what the Rust engine read from it`(name: String) {
        val dir = copyOf(name)
        val want = expected(name)
        Engine.open(dir, Options(keyOrder = KeyOrder.SORTED).readOnly()).use { db ->
            val page = db.scanPage(Bound.Unbounded, Bound.Unbounded, Direction.FORWARD, Int.MAX_VALUE)
            val got = page.map { (k, found) -> Triple(k.hex(), found.value.hex(), found.version) }
            assertEquals(want.map { Triple(it.key, it.value, it.version) }, got)
        }
        Engine.open(dir).use { db ->
            assertEquals(want.map { it.key }.toSet(), db.listKeys().map { it.hex() }.toSet())
            for (e in want) assertEquals(e.value, db.get(e.key.unhex())!!.hex(), "key ${e.key}")
        }
    }

    @ParameterizedTest
    @ValueSource(strings = ["v1", "v2", "mixed"])
    fun `every golden data file decodes and encodes back to the same bytes`(name: String) {
        for (path in files(name, ".data")) {
            val bytes = Files.readAllBytes(path)
            val out = ByteArrayOutputStream()
            when (val format = identify(bytes.copyOf(minOf(bytes.size, 24)))) {
                FileFormat.V1 -> {
                    val input = ByteArrayInputStream(bytes)
                    while (true) {
                        val read = readEntry(input, input.available().toLong()) ?: break
                        val record = (read as? RecordRead.Ok ?: fail("$path: $read")).record
                        val op = record.ops.single()
                        out.write(encodeEntry(op.key, op.value, op.tombstone, record.timestamp))
                    }
                }
                is FileFormat.V2 -> {
                    out.write(encodeFileHeader(format.seqFloor))
                    val input = ByteArrayInputStream(bytes, 24, bytes.size - 24)
                    while (true) {
                        val read = readRecord(input, input.available().toLong()) ?: break
                        val record = (read as? RecordRead.Ok ?: fail("$path: $read")).record
                        val ops = record.ops.map { OpRef(it.key, it.value, it.tombstone) }
                        out.write(encodeRecord(record.seq, record.timestamp, ops).bytes)
                    }
                }
            }
            assertArrayEquals(bytes, out.toByteArray(), "$path")
        }
    }

    @ParameterizedTest
    @ValueSource(strings = ["v2", "mixed"])
    fun `every golden hint file decodes and encodes back to the same bytes`(name: String) {
        val hints = files(name, ".hint")
        assertTrue(hints.isNotEmpty())
        for (path in hints) {
            val bytes = Files.readAllBytes(path)
            val out = ByteArrayOutputStream()
            out.write(HINT_MAGIC)
            for (record in decodeHintFile(bytes)) out.write(encodeHintV2(record))
            val content = out.toByteArray()
            val trailer = ByteArray(4).also { it.putU32(0, crc32(content)) }
            assertArrayEquals(bytes, content + trailer, "$path")
        }
    }
}

private fun ByteArray.hex(): String = joinToString("") { "%02x".format(it) }

private fun String.unhex(): ByteArray = chunked(2).map { it.toInt(16).toByte() }.toByteArray()
