// loadtest: a port of crates/yabiir-loadtest, with the same options and
// output, so that scripts/loadtest-compare.py can run the two side by side.
plugins {
    alias(libs.plugins.kotlin.jvm)
    application
}

kotlin { jvmToolchain(21) }

dependencies {
    implementation(project(":yabiir"))
}

application {
    mainClass = "io.github.jfernand.yabiir.loadtest.MainKt"
    applicationName = "loadtest"
    // A fixed, pre-touched heap, so the comparison doesn't depend on how the
    // JVM sizes it; LOADTEST_OPTS (or JAVA_OPTS) adds to or overrides these.
    applicationDefaultJvmArgs = listOf("-Xms2g", "-Xmx2g", "-XX:+UseG1GC", "-XX:+AlwaysPreTouch")
}
