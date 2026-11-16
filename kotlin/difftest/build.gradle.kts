// Differential tests: the Kotlin engine and the Rust one (`yabiir-diffd`)
// run the same commands; replies and directory contents must match.
import java.io.ByteArrayOutputStream

plugins {
    alias(libs.plugins.kotlin.jvm)
}

kotlin { jvmToolchain(21) }

dependencies {
    testImplementation(project(":yabiir"))
    testImplementation(libs.serialization.json)
    testImplementation(platform(libs.junit.bom))
    testImplementation(libs.junit.jupiter)
    testImplementation(libs.jqwik)
    testRuntimeOnly(libs.junit.launcher)
}

val repoRoot = rootDir.parentFile

/** Build `yabiir-diffd` in release mode; skipped when YABIIR_DIFFD names a binary already. */
val buildDiffd by tasks.registering(Exec::class) {
    onlyIf { System.getenv("YABIIR_DIFFD") == null }
    workingDir = repoRoot
    commandLine("cargo", "build", "--quiet", "--release", "-p", "yabiir-difftest")
}

/** Where cargo puts its build output: `CARGO_TARGET_DIR` or a config can move it from `target/`. */
fun cargoTargetDir(): String {
    val out = ByteArrayOutputStream()
    val process = ProcessBuilder("cargo", "metadata", "--format-version", "1", "--no-deps")
        .directory(repoRoot).redirectErrorStream(false).start()
    out.write(process.inputStream.readBytes())
    process.waitFor()
    return Regex("\"target_directory\":\"([^\"]+)\"").find(out.toString())!!.groupValues[1]
}

fun configureDiff(task: Test, tries: String) {
    task.useJUnitPlatform { includeEngines("junit-jupiter", "jqwik") }
    task.dependsOn(buildDiffd)
    task.systemProperty("jqwik.tries.default", (findProperty("difftest.tries") as String?) ?: tries)
    task.systemProperty("difftest.failures", layout.buildDirectory.dir("failures").get().asFile.path)
    task.doFirst {
        task.systemProperty("yabiir.diffd", System.getenv("YABIIR_DIFFD") ?: "${cargoTargetDir()}/release/yabiir-diffd")
    }
    task.testLogging { events("failed"); exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL }
}

tasks.test { configureDiff(this, "200") }

/** A long differential run: `./gradlew :difftest:soak -Pdifftest.tries=20000`. */
tasks.register<Test>("soak") {
    testClassesDirs = sourceSets.test.get().output.classesDirs
    classpath = sourceSets.test.get().runtimeClasspath
    configureDiff(this, "5000")
    outputs.upToDateWhen { false }
}
