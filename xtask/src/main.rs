//! Developer task runner (`cargo xtask <task>`), following the common Rust
//! "xtask" convention — a plain workspace member, not a published crate —
//! instead of external shell scripts, so release automation lives in the
//! same language and gets the same tooling (rustfmt, clippy, `cargo test`)
//! as the rest of the project.
//!
//! `cargo xtask release` replaces the two manual commands previously
//! documented in README.md's Releasing section: tag the current commit
//! `vX.Y.Z` (from `yabiir`'s own `Cargo.toml` version) and push that tag,
//! which triggers `.github/workflows/publish.yml`.

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match std::env::args()
        .nth(1)
        .as_deref()
    {
        Some("release") => run_release(),
        Some(other) => {
            eprintln!("unknown task {other:?}\n{USAGE}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage: cargo xtask <task>\n\navailable tasks:\n  \
    release   tag the current commit vX.Y.Z (from Cargo.toml) and push \
    it, triggering the crates.io publish workflow";

fn run_release() -> ExitCode {
    match release() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn release() -> Result<(), String> {
    if !working_tree_is_clean()? {
        return Err(
            "working tree has uncommitted changes — commit or stash them before releasing"
                .to_string(),
        );
    }

    let version = yabiir_version()?;
    let tag = format!("v{version}");

    if tag_exists(&tag)? {
        return Err(format!("tag {tag} already exists"));
    }

    println!("tagging {tag} (yabiir v{version})");
    run("git", &["tag", &tag])?;

    println!("pushing {tag} to origin");
    run("git", &["push", "origin", &tag])?;

    println!("done — .github/workflows/publish.yml will publish {tag} to crates.io");
    Ok(())
}

/// Read `yabiir`'s own version from `cargo metadata`, not xtask's — both
/// are workspace members, so `--no-deps` alone isn't enough to disambiguate.
fn yabiir_version() -> Result<String, String> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|e| format!("failed to run cargo metadata: {e}"))?;
    if !output
        .status
        .success()
    {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("failed to parse cargo metadata output: {e}"))?;
    metadata["packages"]
        .as_array()
        .and_then(|packages| {
            packages
                .iter()
                .find(|p| p["name"] == "yabiir")
        })
        .and_then(|p| p["version"].as_str())
        .map(str::to_string)
        .ok_or_else(|| "could not find yabiir's version in cargo metadata output".to_string())
}

fn working_tree_is_clean() -> Result<bool, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| format!("failed to run git status: {e}"))?;
    Ok(output
        .stdout
        .is_empty())
}

fn tag_exists(tag: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .args(["tag", "--list", tag])
        .output()
        .map_err(|e| format!("failed to run git tag --list: {e}"))?;
    Ok(!output
        .stdout
        .is_empty())
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| format!("failed to run {cmd}: {e}"))?;
    if !status.success() {
        return Err(format!("`{cmd} {}` failed", args.join(" ")));
    }
    Ok(())
}
