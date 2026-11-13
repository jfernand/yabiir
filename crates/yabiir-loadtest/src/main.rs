//! Synthetic load generator for measuring latency *predictability* under
//! sustained mixed load — not peak throughput. Prints periodic latency
//! percentiles per operation kind so pauses/spikes (rotation, fsync,
//! merge) show up as visible bumps in a specific report line rather than
//! being averaged away, and can optionally run merge concurrently to see
//! its effect on foreground op latency.
//!
//! Meant to be run either directly (`cargo run --release --bin loadtest --
//! ...`) or under `cargo flamegraph` for profiling:
//!
//! ```text
//! cargo flamegraph --profile profiling --bin loadtest -- \
//!     /tmp/loadtest-data --duration-secs 20 --threads 4 \
//!     --merge-interval-secs 3 --csv /tmp/loadtest.csv
//! ```
//!
//! `--csv` dumps every recorded sample as `elapsed_secs,op,latency_ns`, one
//! per line, for plotting latency-over-time outside this tool.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use yabiir::{Bitcask, Engine, KeyOrder, Options, now_unix};

#[derive(Parser)]
#[command(
    name = "loadtest",
    about = "Synthetic load generator for measuring yabiir's latency predictability under sustained mixed load"
)]
struct Args {
    /// Datastore directory (wiped clean before starting unless --keep).
    dir: PathBuf,

    /// How long to run, after warmup, while recording stats.
    #[arg(long, default_value_t = 20)]
    duration_secs: u64,

    /// Run the load for this long first without recording (lets rotation/
    /// caching etc. reach steady state before numbers count).
    #[arg(long, default_value_t = 0)]
    warmup_secs: u64,

    /// Number of concurrent worker threads issuing get/put/delete.
    #[arg(long, default_value_t = 4)]
    threads: usize,

    /// Keyspace size — workers pick uniformly from `0..keys`.
    #[arg(long, default_value_t = 10_000)]
    keys: u64,

    /// Value size in bytes for puts (and for pre-populating the keyspace).
    #[arg(long, default_value_t = 128)]
    value_size: usize,

    /// Relative weight of `get` in the operation mix.
    #[arg(long, default_value_t = 8)]
    read_weight: u32,
    /// Relative weight of `put` in the operation mix.
    #[arg(long, default_value_t = 2)]
    write_weight: u32,
    /// Relative weight of `delete` in the operation mix.
    #[arg(long, default_value_t = 1)]
    delete_weight: u32,

    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_file_size: u64,

    #[arg(long)]
    sync_on_put: bool,

    /// Keep the keydir sorted (a `BTreeMap`) instead of hashed.
    #[arg(long)]
    sorted_keydir: bool,

    /// Run merge() in a dedicated background thread every N seconds.
    /// Omit to disable merge entirely.
    #[arg(long)]
    merge_interval_secs: Option<u64>,

    /// How often to print a windowed percentile report.
    #[arg(long, default_value_t = 1)]
    report_interval_secs: u64,

    /// Keep any existing data in `dir` instead of wiping it first.
    #[arg(long)]
    keep: bool,

    /// Write every recorded sample to this CSV file (elapsed_secs,op,latency_ns).
    #[arg(long)]
    csv: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum OpKind {
    Get,
    Put,
    Delete,
    Merge,
}

const ALL_KINDS: [OpKind; 4] = [OpKind::Get, OpKind::Put, OpKind::Delete, OpKind::Merge];

impl OpKind {
    fn label(self) -> &'static str {
        match self {
            OpKind::Get => "get",
            OpKind::Put => "put",
            OpKind::Delete => "delete",
            OpKind::Merge => "merge",
        }
    }
}

type Sample = (f64, OpKind, u64); // (elapsed_secs_since_start, kind, latency_ns)

/// xorshift64* — a small, dependency-free PRNG; this tool doesn't need
/// cryptographic quality, just cheap, decent-enough key/op shuffling.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn key_bytes(k: u64) -> Vec<u8> {
    format!("key-{k:012}").into_bytes()
}

fn percentile(sorted_ns: &[u64], p: f64) -> u64 {
    if sorted_ns.is_empty() {
        return 0;
    }
    let idx = (((sorted_ns.len() - 1) as f64) * p).round() as usize;
    sorted_ns[idx]
}

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000_000 {
        format!("{:.2}s", ns as f64 / 1e9)
    } else if ns >= 1_000_000 {
        format!("{:.2}ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.2}us", ns as f64 / 1e3)
    } else {
        format!("{ns}ns")
    }
}

/// Sleep in short increments so a merge/report loop reacts to `stop`
/// promptly instead of blocking for up to a whole interval after the run
/// ends. Returns `true` if `stop` fired before the full duration elapsed.
fn sleep_or_stop(dur: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    stop.load(Ordering::Relaxed)
}

/// Drain every sample currently queued, recording each into `all_samples`
/// (the running, all-time accumulator) and `csv` (if enabled), and return
/// the drained window for the caller to additionally report on (e.g. a
/// periodic print). Called both inside the periodic reporting loop and
/// once more after the worker/merge threads are joined, since anything
/// recorded after the loop's last drain but before those joins complete
/// would otherwise never be recorded anywhere.
fn drain_samples(
    samples: &Mutex<Vec<Sample>>,
    all_samples: &mut HashMap<OpKind, Vec<u64>>,
    csv: &mut Option<BufWriter<File>>,
) -> Vec<Sample> {
    let window: Vec<Sample> = std::mem::take(
        &mut *samples
            .lock()
            .unwrap(),
    );
    for &(t, kind, ns) in &window {
        all_samples
            .entry(kind)
            .or_default()
            .push(ns);
        if let Some(w) = csv.as_mut() {
            writeln!(w, "{t:.6},{},{ns}", kind.label()).unwrap();
        }
    }
    window
}

/// Print one periodic report row per op kind present in `window`, labeled
/// with `elapsed_secs` (seconds since the run started).
fn print_window(window: &[Sample], elapsed_secs: f64) {
    if window.is_empty() {
        return;
    }
    let mut by_kind: HashMap<OpKind, Vec<u64>> = HashMap::new();
    for &(_, kind, ns) in window {
        by_kind
            .entry(kind)
            .or_default()
            .push(ns);
    }
    for kind in ALL_KINDS {
        if let Some(v) = by_kind.get_mut(&kind) {
            v.sort_unstable();
            println!(
                "{:>8.1}s {:>7} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
                elapsed_secs,
                kind.label(),
                v.len(),
                fmt_ns(percentile(v, 0.50)),
                fmt_ns(percentile(v, 0.90)),
                fmt_ns(percentile(v, 0.99)),
                fmt_ns(percentile(v, 0.999)),
                fmt_ns(
                    *v.last()
                        .unwrap()
                )
            );
        }
    }
}

/// What the worker, merge and reporting threads share during a run.
struct Run {
    db: Engine,
    /// Samples recorded since the reporter last drained them.
    samples: Mutex<Vec<Sample>>,
    /// Set when the run is over: every loop checks it and returns.
    stop: AtomicBool,
    /// Whether samples are being kept: false during warmup.
    recording: AtomicBool,
    start: Instant,
}

impl Run {
    fn elapsed_secs(&self) -> f64 {
        self.start
            .elapsed()
            .as_secs_f64()
    }

    /// Queue `batch`, emptying it.
    fn record(&self, batch: &mut Vec<Sample>) {
        self.samples
            .lock()
            .unwrap()
            .append(batch);
    }

    /// A sample for an operation of `kind` that started at `op_start` and
    /// has just finished.
    fn sample(&self, kind: OpKind, op_start: Instant) -> Sample {
        let latency_ns = op_start
            .elapsed()
            .as_nanos() as u64;
        let started_at = op_start
            .duration_since(self.start)
            .as_secs_f64();
        (started_at, kind, latency_ns)
    }
}

fn main() {
    let args = Args::parse();
    assert!(
        args.read_weight + args.write_weight + args.delete_weight > 0,
        "at least one op weight must be nonzero"
    );

    let db = open_store(&args);
    let value = vec![0xABu8; args.value_size];
    prepopulate(&db, args.keys, &value);
    eprintln!(
        "done. running {} threads for {}s (+{}s warmup), merge={}",
        args.threads,
        args.duration_secs,
        args.warmup_secs,
        args.merge_interval_secs
            .map(|s| format!("every {s}s"))
            .unwrap_or_else(|| "disabled".to_string())
    );

    let run = Run {
        db,
        samples: Mutex::new(Vec::new()),
        stop: AtomicBool::new(false),
        recording: AtomicBool::new(args.warmup_secs == 0),
        start: Instant::now(),
    };
    let mut all_samples: HashMap<OpKind, Vec<u64>> = HashMap::new();
    let mut csv = open_csv(&args);

    std::thread::scope(|scope| {
        for worker_id in 0..args.threads {
            let (run, args, value) = (&run, &args, &value);
            scope.spawn(move || work(run, args, value, worker_id as u64));
        }
        if let Some(interval_secs) = args.merge_interval_secs {
            let run = &run;
            scope.spawn(move || merge_periodically(run, Duration::from_secs(interval_secs)));
        }
        report_until_done(&run, &args, &mut all_samples, &mut csv);
        run.stop
            .store(true, Ordering::Relaxed);
    });

    // One more drain, now that every thread has returned: a worker's last
    // partial batch, or a merge that was still running when the reporter
    // took its last look, lands after the reporter's last drain. For a
    // merge that takes seconds, that can be all the data there is.
    let window = drain_samples(&run.samples, &mut all_samples, &mut csv);
    print_window(&window, run.elapsed_secs());
    if let Some(w) = csv.as_mut() {
        w.flush()
            .unwrap();
    }

    print_summary(&mut all_samples);
    let _ = run
        .db
        .close();
}

/// Open the store the run uses, emptying its directory first unless
/// `--keep` was given.
fn open_store(args: &Args) -> Engine {
    if !args.keep {
        let _ = std::fs::remove_dir_all(&args.dir);
    }
    let options = Options {
        max_file_size: args.max_file_size,
        should_sync_on_put: args.sync_on_put,
        key_order: if args.sorted_keydir {
            KeyOrder::Sorted
        } else {
            KeyOrder::Hashed
        },
        ..Options::default()
    };
    Engine::open(&args.dir, options).expect("failed to open datastore")
}

/// Give every key a value, so that reads hit from the first one.
fn prepopulate(db: &Engine, keys: u64, value: &[u8]) {
    eprintln!("pre-populating {keys} keys...");
    for k in 0..keys {
        db.put(&key_bytes(k), value, now_unix())
            .unwrap();
    }
}

/// The `--csv` file with its header written, if one was asked for.
fn open_csv(args: &Args) -> Option<BufWriter<File>> {
    let path = args
        .csv
        .as_ref()?;
    let mut w = BufWriter::new(File::create(path).expect("failed to create --csv file"));
    writeln!(w, "elapsed_secs,op,latency_ns").unwrap();
    Some(w)
}

/// A worker thread: gets, puts and deletes on random keys, in the
/// proportions the weights give, until the run stops.
fn work(run: &Run, args: &Args, value: &[u8], worker_id: u64) {
    const BATCH: usize = 64;
    let (read_w, write_w) = (args.read_weight as u64, args.write_weight as u64);
    let total_weight = read_w + write_w + args.delete_weight as u64;
    let mut rng = Rng::new(0x9E3779B97F4A7C15u64.wrapping_add(worker_id + 1));
    let mut batch: Vec<Sample> = Vec::with_capacity(BATCH);

    while !run
        .stop
        .load(Ordering::Relaxed)
    {
        let key = key_bytes(rng.next_u64() % args.keys);
        let pick = rng.next_u64() % total_weight;

        let op_start = Instant::now();
        let kind = if pick < read_w {
            run.db
                .get(&key)
                .unwrap();
            OpKind::Get
        } else if pick < read_w + write_w {
            run.db
                .put(&key, value, now_unix())
                .unwrap();
            OpKind::Put
        } else {
            run.db
                .delete(&key, now_unix())
                .unwrap();
            OpKind::Delete
        };
        let sample = run.sample(kind, op_start);

        if run
            .recording
            .load(Ordering::Relaxed)
        {
            batch.push(sample);
            if batch.len() >= BATCH {
                run.record(&mut batch);
            }
        }
    }
    run.record(&mut batch);
}

/// The merge thread: one merge every `interval`, until the run stops.
fn merge_periodically(run: &Run, interval: Duration) {
    while !sleep_or_stop(interval, &run.stop) {
        let merge_start = Instant::now();
        run.db
            .merge()
            .unwrap();
        let sample = run.sample(OpKind::Merge, merge_start);
        eprintln!("[merge] took {}", fmt_ns(sample.2));
        if run
            .recording
            .load(Ordering::Relaxed)
        {
            run.record(&mut vec![sample]);
        }
    }
}

/// The reporting loop, on the main thread: every report interval, print
/// the percentiles of the samples recorded since the last report. Returns
/// when warmup and duration have both elapsed.
fn report_until_done(
    run: &Run,
    args: &Args,
    all_samples: &mut HashMap<OpKind, Vec<u64>>,
    csv: &mut Option<BufWriter<File>>,
) {
    let warmup = Duration::from_secs(args.warmup_secs);
    let total_run = warmup + Duration::from_secs(args.duration_secs);
    let report_interval = Duration::from_secs(
        args.report_interval_secs
            .max(1),
    );
    println!(
        "{:>9} {:>7} {:>8} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "elapsed", "op", "count", "p50", "p90", "p99", "p99.9", "max"
    );

    while run
        .start
        .elapsed()
        < total_run
    {
        let remaining = total_run.saturating_sub(
            run.start
                .elapsed(),
        );
        std::thread::sleep(report_interval.min(remaining.max(Duration::from_millis(1))));

        let is_warmup_over = run
            .start
            .elapsed()
            >= warmup;
        if is_warmup_over
            && !run
                .recording
                .swap(true, Ordering::Relaxed)
        {
            eprintln!("--- warmup complete, recording ---");
        }

        let window = drain_samples(&run.samples, all_samples, csv);
        print_window(&window, run.elapsed_secs());
    }
}

/// Print the percentiles of everything recorded, per kind of operation.
fn print_summary(all_samples: &mut HashMap<OpKind, Vec<u64>>) {
    println!("\n=== final summary (recorded, post-warmup samples only) ===");
    for kind in ALL_KINDS {
        if let Some(v) = all_samples.get_mut(&kind) {
            v.sort_unstable();
            println!(
                "{:<7} n={:<9} p50={:>10} p90={:>10} p99={:>10} p99.9={:>10} max={:>10}",
                kind.label(),
                v.len(),
                fmt_ns(percentile(v, 0.50)),
                fmt_ns(percentile(v, 0.90)),
                fmt_ns(percentile(v, 0.99)),
                fmt_ns(percentile(v, 0.999)),
                fmt_ns(
                    *v.last()
                        .unwrap()
                )
            );
        }
    }
}
