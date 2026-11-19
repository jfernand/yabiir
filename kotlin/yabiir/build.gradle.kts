plugins {
    alias(libs.plugins.kotlin.jvm)
}

group = "io.github.jfernand"
version = "0.9.0"

kotlin {
    jvmToolchain(21)
    explicitApi()
}

dependencies {
    testImplementation(platform(libs.junit.bom))
    testImplementation(libs.junit.jupiter)
    testImplementation(libs.jqwik)
    testRuntimeOnly(libs.junit.launcher)
}

tasks.test {
    useJUnitPlatform { includeEngines("junit-jupiter", "jqwik") }
    // The model properties' cases, as Rust's 200 (PROPTEST_CASES there): -Pmodel.tries=2000.
    systemProperty("jqwik.tries.default", (findProperty("model.tries") as String?) ?: "200")
}
