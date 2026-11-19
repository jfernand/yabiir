package io.github.jfernand.yabiir

import java.nio.file.Path

// Helpers shared by the ports of `crates/yabiir/src/engine/tests/`.

/** The UTF-8 bytes of [s]. */
internal fun bytesOf(s: String): ByteArray = s.toByteArray()

/** The bytes as a UTF-8 string, for assertions that compare keys and values. */
internal fun ByteArray.utf8(): String = String(this)

/** `open(&dir)` of the Rust tests: a read-write store, default options unless given. */
internal fun openEngine(dir: Path, opts: Options = Options()): Engine = Engine.open(dir, opts)

/** The version of [key]'s value, or `null`: `version_of` in the Rust tests. */
internal fun Engine.versionOf(key: ByteArray): Long? = getVersioned(key)?.version
