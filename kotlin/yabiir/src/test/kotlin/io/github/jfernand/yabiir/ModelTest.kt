package io.github.jfernand.yabiir

import net.jqwik.api.Arbitrary
import net.jqwik.api.ForAll
import net.jqwik.api.Property
import net.jqwik.api.Provide

/**
 * A port of `crates/yabiir/tests/model.rs`: random sequences of
 * put/delete/get/batch/reopen/merge, checked against a `TreeMap` reference
 * model — with a hashed keydir, a sorted one, and one switching order at
 * every reopen; for a sorted keydir, key order and paged scans too.
 *
 * The number of tries is jqwik's default, set to Rust's 200 cases in
 * `src/test/resources/junit-platform.properties`; override it like
 * `PROPTEST_CASES` with the `jqwik.tries.default` system property.
 */
internal class ModelTest {
    @Provide
    fun runs(): Arbitrary<List<ModelOp>> = ModelArbitraries.runs()

    @Property
    fun `model matches reference`(@ForAll("runs") ops: List<ModelOp>) {
        runModel(ops, Orders.Always(KeyOrder.HASHED))
    }

    @Property
    fun `model matches reference with a sorted keydir`(@ForAll("runs") ops: List<ModelOp>) {
        runModel(ops, Orders.Always(KeyOrder.SORTED))
    }

    @Property
    fun `model matches reference switching order at every reopen`(@ForAll("runs") ops: List<ModelOp>) {
        runModel(ops, Orders.Alternating)
    }
}
