//! `cargo xtask campaign`: benchmark several versions a slice of time at a
//! time, interleaved, until every benchmark has been measured on every one.
//!
//! `cargo xtask bench REF...` measures one version after another, so whatever
//! drifts over the run (the clock as the machine heats up, the room, other
//! load) lands on whole versions: the last one measured reads slow. A
//! campaign measures one benchmark on every version, in a shuffled order,
//! before it moves to the next benchmark, so drift reaches every version
//! alike. That also makes it safe to stop and carry on later: a session on a
//! warmer day shifts every version of the benchmarks it measures together.
//!
//! - `campaign new [--cpu N] [--rounds R] REF...` builds each version's bench
//!   binaries once, lists their benchmarks and plans the work: one unit per
//!   benchmark group per round.
//! - `campaign run DURATION` (`30m`, `1h`, `1h30m`, `90s`) measures units until
//!   the next one would not finish in time. It stops at the deadline even in
//!   the middle of a unit, which is then measured again by a later session.
//! - `campaign status` shows what is done and what is left.
//!
//! When the last unit is done, `bench-history/<ref>.json` is written for every
//! version, in the format `cargo xtask bench` writes, with the median of the
//! rounds' medians. The campaign lives in `target/xtask-bench/campaign/`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::bench::{
    HISTORY_DIR, bench_names, collect, command_out, cpu_model, git, git_out, path_str, repo_root,
};
use crate::machine::Machine;

/// The planned work and everything measured so far.
#[derive(Serialize, Deserialize)]
struct Campaign {
    created_at: String,
    cpu: Option<String>,
    rounds: u32,
    versions: Vec<Version>,
    units: Vec<Unit>,
    sessions: Vec<Session>,
}

/// One version being measured.
#[derive(Serialize, Deserialize)]
struct Version {
    label: String,
    commit: String,
    commit_date: String,
    rustc: String,
    /// Each benchmark this version has, and the binary that runs it.
    benches: BTreeMap<String, PathBuf>,
}

/// One benchmark group, in one round, on every version that has it.
#[derive(Serialize, Deserialize)]
struct Unit {
    round: u32,
    group: String,
    ids: Vec<String>,
    done: Option<Done>,
}

#[derive(Serialize, Deserialize)]
struct Done {
    session: usize,
    finished_at: String,
    seconds: f64,
    results: Results,
}

/// Version label → benchmark id → estimate.
type Results = BTreeMap<String, BTreeMap<String, Estimate>>;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct Estimate {
    median: f64,
    median_lo: f64,
    median_hi: f64,
    mean: f64,
}

#[derive(Serialize, Deserialize)]
struct Session {
    started_at: String,
    budget_seconds: u64,
    machine: Value,
}

pub(crate) fn campaign(args: &[String]) -> Result<(), String> {
    match args
        .first()
        .map(String::as_str)
    {
        Some("new") => new(&args[1..]),
        Some("run") => run(&args[1..]),
        Some("status") => status(),
        _ => Err(
            "usage: cargo xtask campaign { new [--cpu N] [--rounds R] REF... \
                  | run DURATION | status }"
                .to_string(),
        ),
    }
}

fn dir() -> Result<PathBuf, String> {
    Ok(repo_root()?.join("target/xtask-bench/campaign"))
}

fn state_path() -> Result<PathBuf, String> {
    Ok(dir()?.join("state.json"))
}

fn load() -> Result<Campaign, String> {
    let path = state_path()?;
    if !path.exists() {
        return Err("no campaign: start one with `cargo xtask campaign new REF...`".to_string());
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Write the state through a temporary file, so an interrupted save never
/// leaves half of one.
fn save(campaign: &Campaign) -> Result<(), String> {
    let path = state_path()?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(campaign).map_err(|e| e.to_string())? + "\n";
    fs::write(&tmp, text).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| format!("renaming {}: {e}", tmp.display()))
}

// ---------------------------------------------------------------- new

fn new(args: &[String]) -> Result<(), String> {
    let mut cpu = None;
    let mut rounds = 1;
    let mut refs = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cpu" => {
                cpu = Some(
                    args.next()
                        .ok_or("--cpu needs a CPU number")?
                        .clone(),
                )
            }
            "--rounds" => {
                rounds = args
                    .next()
                    .and_then(|r| {
                        r.parse()
                            .ok()
                    })
                    .filter(|&r| r > 0)
                    .ok_or("--rounds needs a positive number")?;
            }
            _ => refs.push(arg.clone()),
        }
    }
    if refs.is_empty() {
        return Err("name the versions to measure, e.g. v0.7.0 v0.8.0".to_string());
    }
    if let Ok(existing) = load()
        && existing
            .units
            .iter()
            .any(|u| {
                u.done
                    .is_none()
            })
    {
        return Err(format!(
            "a campaign is still unfinished ({}); remove {} to abandon it",
            progress(&existing),
            state_path()?.display()
        ));
    }

    let root = repo_root()?;
    let dir = dir()?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("removing {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;

    let started = Instant::now();
    let mut versions = Vec::new();
    for git_ref in &refs {
        versions.push(build(&root, &dir, git_ref)?);
    }
    let units = plan(&versions, rounds);
    let campaign = Campaign {
        created_at: now_utc()?,
        cpu,
        rounds,
        versions,
        units,
        sessions: Vec::new(),
    };
    save(&campaign)?;
    println!(
        "built {} versions in {}; planned {} units ({} rounds of {} groups)",
        refs.len(),
        format_duration(started.elapsed()),
        campaign
            .units
            .len(),
        rounds,
        campaign
            .units
            .len()
            / rounds as usize,
    );
    println!("run with: cargo xtask campaign run 30m");
    Ok(())
}

/// Check `git_ref` out, build its bench binaries, keep a copy of each, and
/// list the benchmarks it has.
fn build(root: &Path, dir: &Path, git_ref: &str) -> Result<Version, String> {
    let commit = git_out(root, &["rev-parse", &format!("{git_ref}^{{commit}}")])?;
    // Outside the repo: cargo looks upward for a workspace, and an old
    // single-package tree inside this one would find ours.
    let worktree = std::env::temp_dir()
        .join("yabiir-xtask-campaign")
        .join(git_ref.replace('/', "_"));
    if worktree.exists() {
        let _ = git(
            root,
            &["worktree", "remove", "--force", path_str(&worktree)?],
        );
    }
    git(
        root,
        &["worktree", "add", "--detach", path_str(&worktree)?, &commit],
    )?;
    let result = build_in(root, dir, &worktree, git_ref, &commit);
    git(
        root,
        &["worktree", "remove", "--force", path_str(&worktree)?],
    )?;
    result
}

fn build_in(
    root: &Path,
    dir: &Path,
    tree: &Path,
    label: &str,
    commit: &str,
) -> Result<Version, String> {
    println!("== building {label} ({commit:.10})");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(tree)
        .args([
            "bench",
            "-p",
            "yabiir",
            "--locked",
            "--no-run",
            "--message-format=json",
        ])
        .env("CARGO_TARGET_DIR", root.join("target/xtask-bench/target"))
        .stderr(Stdio::inherit());
    for name in bench_names(tree)? {
        cmd.args(["--bench", &name]);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run cargo bench: {e}"))?;
    if !out
        .status
        .success()
    {
        return Err(format!("building the benches of {label} failed"));
    }

    // Copy each binary out: versions share the build directory, so a later
    // build could replace it.
    let bin_dir = dir
        .join("bin")
        .join(label.replace('/', "_"));
    fs::create_dir_all(&bin_dir).map_err(|e| format!("creating {}: {e}", bin_dir.display()))?;
    let mut benches = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let is_bench = msg["reason"] == "compiler-artifact"
            && msg["target"]["kind"]
                .as_array()
                .is_some_and(|k| {
                    k.iter()
                        .any(|k| k == "bench")
                });
        let (true, Some(exe), Some(name)) = (
            is_bench,
            msg["executable"].as_str(),
            msg["target"]["name"].as_str(),
        ) else {
            continue;
        };
        let copy = bin_dir.join(name);
        fs::copy(exe, &copy).map_err(|e| format!("copying {exe}: {e}"))?;
        for id in list(&copy)? {
            benches.insert(id, copy.clone());
        }
    }
    if benches.is_empty() {
        return Err(format!("{label} has no benchmarks"));
    }
    println!("   {} benchmarks", benches.len());
    Ok(Version {
        label: label.to_string(),
        commit: commit.to_string(),
        commit_date: git_out(root, &["show", "-s", "--format=%cI", commit])?,
        rustc: command_out(Command::new("rustc").arg("-V"))?,
        benches,
    })
}

/// The benchmark ids a Criterion binary has.
fn list(binary: &Path) -> Result<Vec<String>, String> {
    let out = command_out(
        Command::new(binary)
            .args(["--bench", "--list"])
            .stderr(Stdio::null()),
    )?;
    Ok(out
        .lines()
        .filter_map(|l| l.strip_suffix(": benchmark"))
        .map(str::to_string)
        .collect())
}

/// The group a benchmark belongs to: its id without the last parameter,
/// `read_entry` for `read_entry/64`, or the whole id when it has none.
fn group_of(id: &str) -> &str {
    id.rsplit_once('/')
        .map_or(id, |(group, _)| group)
}

/// One unit per group per round, round by round, so that a campaign that
/// stops early has measured everything once before anything twice.
fn plan(versions: &[Version], rounds: u32) -> Vec<Unit> {
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for version in versions {
        for id in version
            .benches
            .keys()
        {
            let ids = groups
                .entry(group_of(id))
                .or_default();
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    (1..=rounds)
        .flat_map(|round| {
            groups
                .iter()
                .map(move |(group, ids)| Unit {
                    round,
                    group: group.to_string(),
                    ids: ids.clone(),
                    done: None,
                })
        })
        .collect()
}

// ---------------------------------------------------------------- run

fn run(args: &[String]) -> Result<(), String> {
    let budget = parse_duration(
        args.first()
            .ok_or("how long? e.g. cargo xtask campaign run 30m")?,
    )?;
    let deadline = Instant::now() + budget;
    let mut campaign = load()?;
    let cpu = campaign
        .cpu
        .clone();
    let machine = Machine::inspect(cpu.as_deref());
    for warning in machine.warnings(cpu.as_deref()) {
        eprintln!("warning: {warning}");
    }
    campaign
        .sessions
        .push(Session {
            started_at: now_utc()?,
            budget_seconds: budget.as_secs(),
            machine: machine.to_json(),
        });
    let session = campaign
        .sessions
        .len()
        - 1;
    save(&campaign)?;
    println!(
        "session {}: {} for {}",
        session + 1,
        format_duration(budget),
        progress(&campaign)
    );

    let mut rng = Rng::seeded();
    let mut n_done = 0;
    while let Some(next) = campaign
        .units
        .iter()
        .position(|u| {
            u.done
                .is_none()
        })
    {
        let estimate = estimate(&campaign, next);
        let left = deadline.saturating_duration_since(Instant::now());
        if estimate > left {
            println!(
                "stopping: {}{} needs about {}, {} left",
                campaign.units[next].group,
                round_label(&campaign, next),
                format_duration(estimate),
                format_duration(left)
            );
            break;
        }
        let started = Instant::now();
        let unit = &campaign.units[next];
        print!(
            "{}{} ({} benchmarks, about {}) ... ",
            unit.group,
            round_label(&campaign, next),
            unit.ids
                .len(),
            format_duration(estimate)
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let Some(results) = measure_unit(&campaign, next, deadline, &mut rng)? else {
            println!("out of time; it will be measured again");
            break;
        };
        let seconds = started
            .elapsed()
            .as_secs_f64();
        println!("{}", format_duration(Duration::from_secs_f64(seconds)));
        campaign.units[next].done = Some(Done {
            session,
            finished_at: now_utc()?,
            seconds,
            results,
        });
        save(&campaign)?;
        n_done += 1;
    }

    println!(
        "measured {n_done} units this session; {}",
        progress(&campaign)
    );
    if campaign
        .units
        .iter()
        .all(|u| {
            u.done
                .is_some()
        })
    {
        finish(&campaign)?;
    } else {
        println!(
            "about {} of measuring left",
            format_duration(remaining(&campaign))
        );
    }
    Ok(())
}

/// Measure every benchmark of unit `index` on every version that has it,
/// one benchmark at a time, the versions in a fresh random order for each.
/// `None` if the deadline passed first.
fn measure_unit(
    campaign: &Campaign,
    index: usize,
    deadline: Instant,
    rng: &mut Rng,
) -> Result<Option<Results>, String> {
    let home = dir()?.join("criterion");
    let mut results: Results = BTreeMap::new();
    for id in &campaign.units[index].ids {
        let mut order: Vec<&Version> = campaign
            .versions
            .iter()
            .filter(|v| {
                v.benches
                    .contains_key(id)
            })
            .collect();
        rng.shuffle(&mut order);
        for version in order {
            let version_home = home.join(
                version
                    .label
                    .replace('/', "_"),
            );
            let mut cmd = match &campaign.cpu {
                Some(cpu) => {
                    let mut cmd = Command::new("taskset");
                    cmd.args(["-c", cpu]);
                    cmd.arg(&version.benches[id]);
                    cmd
                }
                None => Command::new(&version.benches[id]),
            };
            cmd.args(["--bench", "--noplot", "--exact", id])
                .env("CRITERION_HOME", &version_home)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let child = cmd
                .spawn()
                .map_err(|e| format!("failed to run {cmd:?}: {e}"))?;
            if !wait_until(child, deadline)? {
                return Ok(None);
            }
            let mut found = Map::new();
            collect(&version_home, "new", &mut found)?;
            let estimate = found
                .get(id)
                .and_then(estimate_of)
                .ok_or_else(|| format!("no result for {id} of {}", version.label))?;
            results
                .entry(
                    version
                        .label
                        .clone(),
                )
                .or_default()
                .insert(id.clone(), estimate);
        }
    }
    Ok(Some(results))
}

/// Wait for `child`; kill it at the deadline. Whether it finished in time.
fn wait_until(mut child: Child, deadline: Instant) -> Result<bool, String> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| e.to_string())?
        {
            return if status.success() {
                Ok(true)
            } else {
                Err(format!("a benchmark binary failed: {status}"))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn estimate_of(value: &Value) -> Option<Estimate> {
    Some(Estimate {
        median: value["median"].as_f64()?,
        median_lo: value["median_lo"].as_f64()?,
        median_hi: value["median_hi"].as_f64()?,
        mean: value["mean"].as_f64()?,
    })
}

/// How long unit `index` should take: what the same group took in an
/// earlier round, or else about ten seconds a measurement (Criterion's three
/// of warm-up and five of measuring, plus its analysis).
fn estimate(campaign: &Campaign, index: usize) -> Duration {
    let unit = &campaign.units[index];
    let measured = campaign
        .units
        .iter()
        .filter(|u| u.group == unit.group)
        .find_map(|u| {
            u.done
                .as_ref()
                .map(|d| d.seconds)
        });
    let seconds = measured.unwrap_or_else(|| {
        let measurements = unit
            .ids
            .iter()
            .map(|id| {
                campaign
                    .versions
                    .iter()
                    .filter(|v| {
                        v.benches
                            .contains_key(id)
                    })
                    .count()
            })
            .sum::<usize>();
        measurements as f64 * 10.0
    });
    Duration::from_secs_f64(seconds)
}

fn remaining(campaign: &Campaign) -> Duration {
    (0..campaign
        .units
        .len())
        .filter(|&i| {
            campaign.units[i]
                .done
                .is_none()
        })
        .map(|i| estimate(campaign, i))
        .sum()
}

fn round_label(campaign: &Campaign, index: usize) -> String {
    if campaign.rounds == 1 {
        String::new()
    } else {
        format!(
            " (round {}/{})",
            campaign.units[index].round, campaign.rounds
        )
    }
}

// ---------------------------------------------------------------- finish

/// Write `bench-history/<label>.json` for every version: per benchmark, the
/// median of the rounds' medians, the widest of their intervals, and each
/// round's median.
fn finish(campaign: &Campaign) -> Result<(), String> {
    let root = repo_root()?;
    let history = root.join(HISTORY_DIR);
    fs::create_dir_all(&history).map_err(|e| format!("creating {}: {e}", history.display()))?;
    let finished_at = campaign
        .units
        .iter()
        .filter_map(|u| {
            u.done
                .as_ref()
                .map(|d| {
                    d.finished_at
                        .as_str()
                })
        })
        .max()
        .unwrap_or_default()
        .to_string();
    for version in &campaign.versions {
        let mut rounds: BTreeMap<&str, Vec<Estimate>> = BTreeMap::new();
        for done in campaign
            .units
            .iter()
            .filter_map(|u| {
                u.done
                    .as_ref()
            })
        {
            for (id, estimate) in done
                .results
                .get(&version.label)
                .into_iter()
                .flatten()
            {
                rounds
                    .entry(id)
                    .or_default()
                    .push(*estimate);
            }
        }
        let results: Map<String, Value> = rounds
            .iter()
            .map(|(id, estimates)| (id.to_string(), combine(estimates)))
            .collect();
        let record = json!({
            "label": version.label,
            "commit": version.commit,
            "commit_date": version.commit_date,
            "measured_at": finished_at,
            "rustc": version.rustc,
            "cpu": cpu_model(),
            "pinned_cpu": campaign.cpu,
            "machine": campaign.sessions.first().map(|s| s.machine.clone()),
            "campaign": {
                "created_at": campaign.created_at,
                "rounds": campaign.rounds,
                "versions": campaign.versions.iter().map(|v| v.label.as_str()).collect::<Vec<_>>(),
                "sessions": campaign.sessions.iter().map(|s| json!({
                    "started_at": s.started_at,
                    "budget_seconds": s.budget_seconds,
                    "machine": s.machine,
                })).collect::<Vec<_>>(),
            },
            "unit": "ns",
            "results": results,
        });
        let file = history.join(format!(
            "{}.json",
            version
                .label
                .replace('/', "_")
        ));
        let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())? + "\n";
        fs::write(&file, text).map_err(|e| format!("writing {}: {e}", file.display()))?;
        println!(
            "recorded {} benchmarks in {}",
            results.len(),
            file.display()
        );
    }
    println!("campaign finished");
    Ok(())
}

fn combine(estimates: &[Estimate]) -> Value {
    let medians: Vec<f64> = estimates
        .iter()
        .map(|e| e.median)
        .collect();
    let lo = estimates
        .iter()
        .map(|e| e.median_lo)
        .fold(f64::INFINITY, f64::min);
    let hi = estimates
        .iter()
        .map(|e| e.median_hi)
        .fold(f64::NEG_INFINITY, f64::max);
    let mean = estimates
        .iter()
        .map(|e| e.mean)
        .sum::<f64>()
        / estimates.len() as f64;
    json!({
        "median": median(&medians),
        "median_lo": lo,
        "median_hi": hi,
        "mean": mean,
        "rounds": medians,
    })
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

// ---------------------------------------------------------------- status

fn status() -> Result<(), String> {
    let campaign = load()?;
    println!(
        "versions: {}",
        campaign
            .versions
            .iter()
            .map(|v| v
                .label
                .as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "pinned to CPU {}, {} round(s), started {}",
        campaign
            .cpu
            .as_deref()
            .unwrap_or("none"),
        campaign.rounds,
        campaign.created_at
    );
    println!("{}", progress(&campaign));
    let spent: f64 = campaign
        .units
        .iter()
        .filter_map(|u| {
            u.done
                .as_ref()
                .map(|d| d.seconds)
        })
        .sum();
    println!(
        "{} sessions, {} measured, about {} left",
        campaign
            .sessions
            .len(),
        format_duration(Duration::from_secs_f64(spent)),
        format_duration(remaining(&campaign))
    );
    Ok(())
}

fn progress(campaign: &Campaign) -> String {
    let done = campaign
        .units
        .iter()
        .filter(|u| {
            u.done
                .is_some()
        })
        .count();
    format!(
        "{done} of {} units done",
        campaign
            .units
            .len()
    )
}

// ---------------------------------------------------------------- small helpers

/// `30m`, `1h`, `1h30m`, `90s`; a bare number is minutes.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = || format!("can't read {text:?} as a duration; try 30m, 1h or 1h30m");
    if let Ok(minutes) = text.parse::<u64>() {
        return Ok(Duration::from_secs(minutes * 60));
    }
    let mut seconds = 0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let n: u64 = number
            .parse()
            .map_err(|_| bad())?;
        number.clear();
        seconds += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return Err(bad()),
        };
    }
    if !number.is_empty() || seconds == 0 {
        return Err(bad());
    }
    Ok(Duration::from_secs(seconds))
}

fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

fn now_utc() -> Result<String, String> {
    command_out(Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]))
}

/// A small xorshift generator for shuffling; no need for a crate.
struct Rng(u64);

impl Rng {
    fn seeded() -> Rng {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Rng(nanos | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Fisher–Yates.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(label: &str, ids: &[&str]) -> Version {
        Version {
            label: label.to_string(),
            commit: String::new(),
            commit_date: String::new(),
            rustc: String::new(),
            benches: ids
                .iter()
                .map(|id| (id.to_string(), PathBuf::from("bench")))
                .collect(),
        }
    }

    #[test]
    fn a_group_is_the_id_without_its_last_parameter() {
        assert_eq!(group_of("read_entry/64"), "read_entry");
        assert_eq!(
            group_of("order/get_hit/hashed/1000"),
            "order/get_hit/hashed"
        );
        assert_eq!(group_of("merge_noop"), "merge_noop");
    }

    #[test]
    fn the_plan_measures_everything_once_before_anything_twice() {
        let versions = [
            version("old", &["a/1", "a/2", "b"]),
            version("new", &["a/1", "a/2", "b", "c/1"]),
        ];
        let units = plan(&versions, 2);
        let shape: Vec<(u32, &str, usize)> = units
            .iter()
            .map(|u| {
                (
                    u.round,
                    u.group
                        .as_str(),
                    u.ids
                        .len(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (1, "a", 2),
                (1, "b", 1),
                (1, "c", 1),
                (2, "a", 2),
                (2, "b", 1),
                (2, "c", 1)
            ]
        );
    }

    #[test]
    fn an_unmeasured_unit_is_estimated_from_its_measurements() {
        let campaign = Campaign {
            created_at: String::new(),
            cpu: None,
            rounds: 2,
            versions: vec![
                version("old", &["a/1", "a/2"]),
                version("new", &["a/1", "a/2", "a/3"]),
            ],
            units: plan(
                &[
                    version("old", &["a/1", "a/2"]),
                    version("new", &["a/1", "a/2", "a/3"]),
                ],
                2,
            ),
            sessions: Vec::new(),
        };
        // a/1 and a/2 on both versions, a/3 on one: five measurements.
        assert_eq!(estimate(&campaign, 0), Duration::from_secs(50));
    }

    #[test]
    fn a_measured_group_is_estimated_by_its_last_round() {
        let versions = vec![version("v", &["a/1"])];
        let mut units = plan(&versions, 2);
        units[0].done = Some(Done {
            session: 0,
            finished_at: String::new(),
            seconds: 123.0,
            results: BTreeMap::new(),
        });
        let campaign = Campaign {
            created_at: String::new(),
            cpu: None,
            rounds: 2,
            versions,
            units,
            sessions: Vec::new(),
        };
        assert_eq!(estimate(&campaign, 1), Duration::from_secs(123));
        assert_eq!(remaining(&campaign), Duration::from_secs(123));
    }

    #[test]
    fn rounds_combine_to_the_median_and_the_widest_interval() {
        let e = |median: f64| Estimate {
            median,
            median_lo: median - 1.0,
            median_hi: median + 1.0,
            mean: median,
        };
        let combined = combine(&[e(10.0), e(30.0), e(20.0)]);
        assert_eq!(combined["median"], 20.0);
        assert_eq!(combined["median_lo"], 9.0);
        assert_eq!(combined["median_hi"], 31.0);
        assert_eq!(combined["rounds"], json!([10.0, 30.0, 20.0]));
        assert_eq!(combine(&[e(10.0), e(20.0)])["median"], 15.0);
    }

    #[test]
    fn durations_read_like_people_write_them() {
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("30"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert!(parse_duration("1x").is_err());
        assert!(parse_duration("1h30").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn a_shuffle_keeps_every_item() {
        let mut rng = Rng(42);
        let mut items: Vec<u32> = (0..20).collect();
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>());
        assert_ne!(
            items, sorted,
            "a seeded shuffle of 20 items should move something"
        );
    }
}
