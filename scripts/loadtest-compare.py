#!/usr/bin/env python3
"""Run the Rust engine's load test and the Kotlin port's on the same workloads,
and compare them.

    scripts/loadtest-compare.py [--rounds 3] [--duration 15] [--warmup 5] [--cpus 2-5]
                                [--workloads read-heavy,merge] [--dir DIR] [--out DIR]

Each round runs every workload on both engines, in a shuffled order, so drift
in the machine (clock, temperature, page cache) reaches both alike. The table
reports the median over rounds:
- throughput: the operations completed, gets, puts and deletes, per second;
- latency percentiles per kind of operation, from each run's summary.

Both tools are built first: `cargo build --release -p yabiir-loadtest` and
`kotlin/gradlew -p kotlin :loadtest:installDist`. The Kotlin one runs with a
fixed 2 GiB heap and G1 (kotlin/loadtest/build.gradle.kts; LOADTEST_OPTS adds
JVM options). `--cpus` pins both to the same cores with taskset; the JVM's own
threads (JIT, GC) then compete for them too, which is part of its cost.
"""

import argparse
import json
import random
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

WRITE_HEAVY = ["--read-weight", "1", "--write-weight", "8", "--delete-weight", "1"]
WORKLOADS = {
    # The tool's defaults: 4 threads, 10,000 keys, 128-byte values, gets:puts:deletes 8:2:1.
    "read-heavy": [],
    "write-heavy": WRITE_HEAVY,
    "sorted": ["--sorted-keydir"],
    # Small files, so that every merge has rotated files to compact.
    "merge": ["--merge-interval-secs", "3", "--max-file-size", str(1 << 20)],
    # fsync on every write, from enough threads that group commit has writes to share.
    "sync-on-put": WRITE_HEAVY + ["--sync-on-put", "--threads", "16"],
}
KINDS = ["get", "put", "delete", "merge"]


def build():
    subprocess.run(["cargo", "build", "--quiet", "--release", "-p", "yabiir-loadtest"], cwd=ROOT, check=True)
    subprocess.run([str(ROOT / "kotlin/gradlew"), "-q", "-p", "kotlin", ":loadtest:installDist"], cwd=ROOT, check=True)
    meta = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, check=True, capture_output=True, text=True
    )
    target = Path(json.loads(meta.stdout)["target_directory"])
    return {
        "rust": [str(target / "release/loadtest")],
        "kotlin": [str(ROOT / "kotlin/loadtest/build/install/loadtest/bin/loadtest")],
    }


def run_one(cmd, store, args, workload, summary, cpus):
    shutil.rmtree(store, ignore_errors=True)
    full = cmd + [str(store), "--duration-secs", str(args.duration), "--warmup-secs", str(args.warmup)]
    full += WORKLOADS[workload] + ["--summary-json", str(summary)]
    if cpus:
        full = ["taskset", "-c", cpus] + full
    log = summary.with_suffix(".log")
    with open(log, "w") as out:
        result = subprocess.run(full, stdout=out, stderr=subprocess.STDOUT)
    shutil.rmtree(store, ignore_errors=True)
    if result.returncode != 0:
        sys.exit(f"{' '.join(full)} failed with {result.returncode}; see {log}")
    return json.loads(summary.read_text())


def fmt_ns(ns):
    if ns >= 1e9:
        return f"{ns / 1e9:.2f}s"
    if ns >= 1e6:
        return f"{ns / 1e6:.2f}ms"
    if ns >= 1e3:
        return f"{ns / 1e3:.2f}us"
    return f"{ns:.0f}ns"


def throughput(summary):
    n = sum(summary["ops"].get(k, {}).get("n", 0) for k in ("get", "put", "delete"))
    return n / summary["duration_secs"]


def report(results, workloads, args):
    lines = [
        f"# Load test: Rust vs Kotlin",
        "",
        f"{args.rounds} rounds of {args.duration}s (after {args.warmup}s warmup) per engine and workload; "
        f"medians over rounds" + (f"; pinned to CPUs {args.cpus}" if args.cpus else "") + ".",
        "",
    ]
    for w in workloads:
        lines += [f"## {w}", "", f"Options: `{' '.join(WORKLOADS[w]) or '(defaults)'}`", ""]
        tput = {e: statistics.median(throughput(s) for s in results[w][e]) for e in ("rust", "kotlin")}
        lines += [
            "| engine | ops/s | vs Rust | op | p50 | p90 | p99 | p99.9 | max |",
            "|---|--:|--:|---|--:|--:|--:|--:|--:|",
        ]
        for e in ("rust", "kotlin"):
            first = True
            for kind in KINDS:
                runs = [s["ops"][kind] for s in results[w][e] if kind in s["ops"]]
                if not runs:
                    continue
                med = {f: statistics.median(r[f] for r in runs) for f in ("p50_ns", "p90_ns", "p99_ns", "p999_ns", "max_ns")}
                head = (
                    f"| {e} | {tput[e]:,.0f} | {tput[e] / tput['rust']:.2f}x |"
                    if first
                    else "| | | |"
                )
                lines.append(
                    f"{head} {kind} | {fmt_ns(med['p50_ns'])} | {fmt_ns(med['p90_ns'])} | {fmt_ns(med['p99_ns'])} | "
                    f"{fmt_ns(med['p999_ns'])} | {fmt_ns(med['max_ns'])} |"
                )
                first = False
        lines.append("")
    return "\n".join(lines)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--rounds", type=int, default=3)
    p.add_argument("--duration", type=int, default=15, help="seconds measured per run")
    p.add_argument("--warmup", type=int, default=5, help="seconds before measuring, per run")
    p.add_argument("--cpus", help="pin both engines to these CPUs (taskset -c syntax, e.g. 2-5)")
    p.add_argument("--workloads", default=",".join(WORKLOADS), help="comma-separated: " + ", ".join(WORKLOADS))
    p.add_argument("--dir", type=Path, default=ROOT / "target/loadtest-compare/store",
                   help="where the stores go: a real disk, not tmpfs, for the fsync numbers to mean anything")
    p.add_argument("--out", type=Path, default=ROOT / "target/loadtest-compare",
                   help="where each run's summary and log, and results.md, go")
    p.add_argument("--seed", type=int, default=None, help="for the order of runs")
    args = p.parse_args()

    workloads = args.workloads.split(",")
    for w in workloads:
        if w not in WORKLOADS:
            sys.exit(f"unknown workload {w}; known: {', '.join(WORKLOADS)}")
    commands = build()
    args.out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(args.seed)
    results = {w: {"rust": [], "kotlin": []} for w in workloads}
    plan = [(r, w, e) for r in range(args.rounds) for w in workloads for e in ("rust", "kotlin")]
    total = len(plan)
    done = 0
    for r in range(args.rounds):
        units = [(w, e) for w in workloads for e in ("rust", "kotlin")]
        rng.shuffle(units)
        for w, e in units:
            done += 1
            print(f"[{done}/{total}] round {r + 1}: {w} on {e}", file=sys.stderr, flush=True)
            summary = args.out / f"{w}-{e}-{r + 1}.json"
            results[w][e].append(run_one(commands[e], args.dir, args, w, summary, args.cpus))

    text = report(results, workloads, args)
    (args.out / "results.md").write_text(text + "\n")
    print(text)
    print(f"\nwritten to {args.out / 'results.md'}", file=sys.stderr)


if __name__ == "__main__":
    main()
