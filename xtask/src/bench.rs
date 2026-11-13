//! `cargo xtask bench` and `cargo xtask bench-table`: see the crate docs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value, json};

/// Where the recorded medians live, one JSON file per measured version.
const HISTORY_DIR: &str = "bench-history";

pub(crate) fn bench(args: &[String]) -> Result<(), String> {
    let root = repo_root()?;
    let mut cpu = None;
    let mut refs = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--cpu" {
            cpu = Some(
                args.next()
                    .ok_or("--cpu needs a CPU number")?
                    .clone(),
            );
        } else {
            refs.push(arg.clone());
        }
    }
    let cpu = cpu.as_deref();
    if refs.is_empty() {
        let label = working_tree_label()?;
        let commit = git_out(&root, &["rev-parse", "HEAD"])?;
        return measure(&root, &root, &label, &commit, cpu);
    }
    for git_ref in &refs {
        let commit = git_out(&root, &["rev-parse", &format!("{git_ref}^{{commit}}")])?;
        // Outside the repo: cargo looks upward for a workspace, and an old
        // single-package tree inside this one would find ours.
        let worktree = std::env::temp_dir()
            .join("yabiir-xtask-bench")
            .join(git_ref.replace('/', "_"));
        if worktree.exists() {
            let _ = git(
                &root,
                &["worktree", "remove", "--force", path_str(&worktree)?],
            );
        }
        git(
            &root,
            &["worktree", "add", "--detach", path_str(&worktree)?, &commit],
        )?;
        let result = measure(&root, &worktree, git_ref, &commit, cpu);
        git(
            &root,
            &["worktree", "remove", "--force", path_str(&worktree)?],
        )?;
        result?;
    }
    Ok(())
}

/// Run the benches of the tree checked out at `tree`, save them as the
/// Criterion baseline `label`, and record the medians in the history.
fn measure(
    root: &Path,
    tree: &Path,
    label: &str,
    commit: &str,
    cpu: Option<&str>,
) -> Result<(), String> {
    let criterion_home = root.join("target/criterion");
    println!("== {label} ({commit:.10}) in {}", tree.display());
    let target_dir = (tree != root).then(|| root.join("target/xtask-bench/target"));
    // `--benches` would also run the lib's libtest harness, which rejects
    // Criterion's flags: name each bench target of that version instead.
    let bench_targets: Vec<String> = bench_names(tree)?
        .into_iter()
        .flat_map(|name| ["--bench".to_string(), name])
        .collect();
    // Build on every core, then measure on `cpu` alone if one is given.
    for (pin, run_args) in [
        (None, &["--no-run"][..]),
        (cpu, &["--", "--save-baseline", label, "--noplot"][..]),
    ] {
        let mut cmd = match pin {
            Some(cpu) => {
                let mut cmd = Command::new("taskset");
                cmd.args(["-c", cpu, "cargo"]);
                cmd
            }
            None => Command::new("cargo"),
        };
        cmd.current_dir(tree)
            .args(["bench", "-p", "yabiir", "--locked"])
            .args(&bench_targets)
            .args(run_args)
            .env("CRITERION_HOME", &criterion_home);
        if let Some(dir) = &target_dir {
            // One build cache for every rewound version: the dependencies
            // don't change between them, only yabiir itself is rebuilt.
            cmd.env("CARGO_TARGET_DIR", dir);
        }
        let status = cmd
            .status()
            .map_err(|e| format!("failed to run cargo bench: {e}"))?;
        if !status.success() {
            return Err(format!("cargo bench failed for {label}"));
        }
    }

    let mut results = Map::new();
    collect(&criterion_home, label, &mut results)?;
    if results.is_empty() {
        return Err(format!("no Criterion results for baseline {label:?}"));
    }
    let record = json!({
        "label": label,
        "commit": commit,
        "commit_date": git_out(root, &["show", "-s", "--format=%cI", commit])?,
        "measured_at": command_out(Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]))?,
        "rustc": command_out(Command::new("rustc").arg("-V"))?,
        "cpu": cpu_model(),
        "pinned_cpu": cpu,
        "unit": "ns",
        "results": results,
    });
    let dir = root.join(HISTORY_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let file = dir.join(format!("{label}.json"));
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())? + "\n";
    fs::write(&file, text).map_err(|e| format!("writing {}: {e}", file.display()))?;
    println!(
        "recorded {} benchmarks in {}",
        record["results"]
            .as_object()
            .map_or(0, Map::len),
        file.display()
    );
    Ok(())
}

/// Find every `<bench dir>/<label>/` under Criterion's home and take the
/// median (with its 95% interval) and mean from its `estimates.json`.
fn collect(dir: &Path, label: &str, out: &mut Map<String, Value>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let estimates = path.join("estimates.json");
        if entry.file_name() == label && estimates.exists() {
            let meta = read_json(&path.join("benchmark.json"))?;
            let est = read_json(&estimates)?;
            let id = meta["full_id"]
                .as_str()
                .ok_or_else(|| format!("no full_id in {}", path.display()))?;
            let median = &est["median"];
            out.insert(
                id.to_string(),
                json!({
                    "median": median["point_estimate"],
                    "median_lo": median["confidence_interval"]["lower_bound"],
                    "median_hi": median["confidence_interval"]["upper_bound"],
                    "mean": est["mean"]["point_estimate"],
                }),
            );
        } else {
            collect(&path, label, out)?;
        }
    }
    Ok(())
}

pub(crate) fn table() -> Result<(), String> {
    let dir = repo_root()?.join(HISTORY_DIR);
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let path = entry
            .map_err(|e| e.to_string())?
            .path();
        if path
            .extension()
            .is_some_and(|e| e == "json")
        {
            records.push(read_json(&path)?);
        }
    }
    if records.is_empty() {
        return Err(format!("nothing recorded in {}", dir.display()));
    }
    let key = |r: &Value| {
        (
            r["commit_date"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            r["label"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    };
    records.sort_by_key(key);

    let mut rows: BTreeMap<&str, Vec<Option<f64>>> = BTreeMap::new();
    for (col, record) in records
        .iter()
        .enumerate()
    {
        for (id, est) in record["results"]
            .as_object()
            .into_iter()
            .flatten()
        {
            let row = rows
                .entry(id)
                .or_insert_with(|| vec![None; records.len()]);
            row[col] = est["median"].as_f64();
        }
    }
    let id_width = rows
        .keys()
        .map(|k| k.len())
        .max()
        .unwrap_or(0);
    print!("{:id_width$}", "median");
    for record in &records {
        print!(
            "  {:>10}",
            record["label"]
                .as_str()
                .unwrap_or("?")
        );
    }
    println!();
    for (id, cells) in rows {
        print!("{id:id_width$}");
        for cell in cells {
            print!("  {:>10}", cell.map_or("-".to_string(), format_ns));
        }
        println!();
    }
    Ok(())
}

fn format_ns(ns: f64) -> String {
    match ns {
        n if n < 1e3 => format!("{n:.1} ns"),
        n if n < 1e6 => format!("{:.2} µs", n / 1e3),
        n if n < 1e9 => format!("{:.2} ms", n / 1e6),
        n => format!("{:.2} s", n / 1e9),
    }
}

/// `vX.Y.Z` when the clean working tree is exactly that release's tag,
/// otherwise `vX.Y.Z+<short sha>`, with `-dirty` for uncommitted changes.
fn working_tree_label() -> Result<String, String> {
    let root = repo_root()?;
    let clean = crate::working_tree_is_clean()?;
    let tag = git_out(&root, &["tag", "--points-at", "HEAD", "--list", "v*"])?;
    if clean
        && let Some(tag) = tag
            .lines()
            .next()
    {
        return Ok(tag.to_string());
    }
    let sha = git_out(&root, &["rev-parse", "--short", "HEAD"])?;
    let version = crate::yabiir_version()?;
    Ok(format!(
        "v{version}+{sha}{}",
        if clean { "" } else { "-dirty" }
    ))
}

/// The `[[bench]]` targets of yabiir as checked out at `tree`.
fn bench_names(tree: &Path) -> Result<Vec<String>, String> {
    let out = command_out(
        Command::new("cargo")
            .current_dir(tree)
            .args(["metadata", "--no-deps", "--format-version", "1"]),
    )?;
    let meta: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let names: Vec<String> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"] == "yabiir")
        .flat_map(|p| {
            p["targets"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|t| {
            t["kind"]
                .as_array()
                .is_some_and(|k| {
                    k.iter()
                        .any(|k| k == "bench")
                })
        })
        .filter_map(|t| {
            t["name"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    if names.is_empty() {
        return Err(format!("no bench targets in {}", tree.display()));
    }
    Ok(names)
}

fn cpu_model() -> Option<String> {
    fs::read_to_string("/proc/cpuinfo")
        .ok()?
        .lines()
        .find(|l| l.starts_with("model name"))?
        .split_once(':')
        .map(|(_, m)| {
            m.trim()
                .to_string()
        })
}

fn repo_root() -> Result<PathBuf, String> {
    git_out(Path::new("."), &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    git_out(dir, args).map(drop)
}

fn git_out(dir: &Path, args: &[&str]) -> Result<String, String> {
    command_out(
        Command::new("git")
            .current_dir(dir)
            .args(args),
    )
}

fn command_out(cmd: &mut Command) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run {cmd:?}: {e}"))?;
    if !output
        .status
        .success()
    {
        return Err(format!(
            "{cmd:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string())
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("non-UTF-8 path {}", path.display()))
}
