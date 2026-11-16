// The Kotlin port of yabiir (`yabiir/`) and the differential tests that
// check it against the Rust engine (`difftest/`). See README.md.
plugins {
    alias(libs.plugins.kotlin.jvm) apply false
    alias(libs.plugins.kotlin.serialization) apply false
}
