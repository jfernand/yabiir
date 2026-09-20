//! Consumes the AT Protocol ("Bluesky") firehose — the same
//! `com.atproto.sync.subscribeRepos` WebSocket every relay and PDS speaks —
//! and stores every `app.bsky.feed.post` create/update/delete it sees in a
//! `yabiir` datastore, with a `ratatui` dashboard of live throughput
//! metrics (one running sparkline per counter) instead of a scrolling log.
//!
//! Unlike BGP's BMP (the other live-feed protocol considered for this
//! example), the AT Proto firehose is genuinely subscriber-initiated: any
//! client can open this WebSocket and start receiving events, no
//! registration required. Each message on the socket is two concatenated
//! DAG-CBOR values back to back — a small frame header (`{op, t}`) naming
//! the message type, then a body matching that type — and a commit's body
//! carries its *changed* repository data as a CAR (Content-Addressable
//! aRchive) byte blob rather than as already-decoded records, so a `create`
//! needs an extra step: find the block matching the op's CID inside that
//! CAR blob, then DAG-CBOR-decode *that* block as the actual post record.
//!
//! Run with: `cargo run --example bsky_firehose [dir]` (`dir` defaults to a
//! directory under the OS temp dir, printed on startup, kept across runs).
//! `q`/`Esc` to quit.

use std::collections::VecDeque;
use std::io::Cursor as IoCursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use atrium_api::app::bsky::feed::post::RecordData as PostRecordData;
use atrium_api::com::atproto::sync::subscribe_repos::CommitData;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures_util::io::Cursor as AsyncCursor;
use futures_util::{StreamExt, TryStreamExt};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Sparkline};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use yabiir::{Bitcask, Engine, Metrics, Options, now_unix};

const FIREHOSE_URL: &str = "wss://bsky.network/xrpc/com.atproto.sync.subscribeRepos";
const POST_COLLECTION: &str = "app.bsky.feed.post";
/// How many one-second samples each sparkline keeps — 120 = 2 minutes of
/// history scrolling by.
const HISTORY_LEN: usize = 120;
const LOG_LEN: usize = 200;
/// How often (in ticks, i.e. seconds) to run a background `merge` + `sync`
/// — nothing in this example otherwise ever calls either, and the whole
/// point of wiring up `yabiir::Metrics` here is to have something real to
/// show for `record_merge`/`record_sync`. Merge on a modest local dataset
/// finishes well under a second (see docs/merge-batched-flushing.typ), so
/// blocking the event loop briefly every 30s is an acceptable demo
/// tradeoff, not something a production consumer should copy as-is — a
/// real one would run this via `tokio::task::spawn_blocking`.
const MERGE_INTERVAL_TICKS: u32 = 30;

/// The two-value framing every firehose message uses: a header naming the
/// message type (only meaningful when `op == 1`; `op == -1` is an error
/// frame with no body at all), then — for the types we care about — a body
/// of the matching shape. Everything else (`#identity`, `#account`,
/// `#info`, `#sync`, errors) is intentionally ignored; this example only
/// follows post records.
#[derive(serde::Deserialize)]
struct FrameHeader {
    op: i8,
    #[serde(default)]
    t: Option<String>,
}

/// `{repo_did}/{rkey}` — unique per post, and stable across a post's
/// create/update/delete lifecycle, so later events correctly overwrite or
/// remove the same key.
fn post_key(repo_did: &str, rkey: &str) -> Vec<u8> {
    format!("{repo_did}/{rkey}").into_bytes()
}

/// `[created_at: u32 LE][text: rest of the buffer, UTF-8]` — the same
/// hand-rolled, no-serde style `examples/todo_tui.rs` uses.
fn encode_post(text: &str, created_at_unix: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + text.len());
    buf.extend_from_slice(&created_at_unix.to_le_bytes());
    buf.extend_from_slice(text.as_bytes());
    buf
}

fn truncate_for_display(s: &str, max_chars: usize) -> String {
    let mut out: String = s
        .chars()
        .take(max_chars)
        .collect();
    if s.chars()
        .count()
        > max_chars
    {
        out.push('\u{2026}'); // "…"
    }
    out.replace('\n', " ")
}

/// One observable counter: a running total, the count accumulated in the
/// current (not-yet-elapsed) second, and a bounded history of completed
/// per-second counts — exactly what a `Sparkline` needs to draw a running
/// chart, and cheap to maintain (`record` is just an add, `tick` runs once
/// a second).
struct Metric {
    label: &'static str,
    total: u64,
    this_tick: u64,
    history: VecDeque<u64>,
}

impl Metric {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            total: 0,
            this_tick: 0,
            history: VecDeque::with_capacity(HISTORY_LEN),
        }
    }

    fn record(&mut self, n: u64) {
        self.total += n;
        self.this_tick += n;
    }

    /// Close out the current second: push it onto history (dropping the
    /// oldest sample once full) and start a fresh one.
    fn tick(&mut self) {
        if self
            .history
            .len()
            >= HISTORY_LEN
        {
            self.history
                .pop_front();
        }
        self.history
            .push_back(self.this_tick);
        self.this_tick = 0;
    }

    fn last_rate(&self) -> u64 {
        self.history
            .back()
            .copied()
            .unwrap_or(0)
    }
}

/// A `yabiir::Metrics` implementation: just running totals (count + summed
/// nanoseconds) per operation kind, cheap enough to update on every call —
/// `docs/ROADMAP.md` §4's metrics hook, with somewhere real to plug it in.
#[derive(Default)]
struct EngineMetricsCollector {
    put_count: AtomicU64,
    put_nanos: AtomicU64,
    get_count: AtomicU64,
    get_nanos: AtomicU64,
    delete_count: AtomicU64,
    delete_nanos: AtomicU64,
    merge_count: AtomicU64,
    merge_nanos: AtomicU64,
    sync_count: AtomicU64,
    sync_nanos: AtomicU64,
}

impl EngineMetricsCollector {
    fn record(count: &AtomicU64, nanos: &AtomicU64, duration: Duration) {
        count.fetch_add(1, Ordering::Relaxed);
        nanos.fetch_add(duration.as_nanos() as u64, Ordering::Relaxed);
    }

    /// `(count, average nanoseconds per call)` — an all-time average, not a
    /// per-tick one; fine for `merge`/`sync`, which fire too rarely for a
    /// per-second rate to mean much anyway.
    fn snapshot(count: &AtomicU64, nanos: &AtomicU64) -> (u64, u64) {
        let count = count.load(Ordering::Relaxed);
        let nanos = nanos.load(Ordering::Relaxed);
        (
            count,
            nanos
                .checked_div(count)
                .unwrap_or(0),
        )
    }
}

impl Metrics for EngineMetricsCollector {
    fn record_put(&self, duration: Duration) {
        Self::record(&self.put_count, &self.put_nanos, duration);
    }
    fn record_get(&self, duration: Duration) {
        Self::record(&self.get_count, &self.get_nanos, duration);
    }
    fn record_delete(&self, duration: Duration) {
        Self::record(&self.delete_count, &self.delete_nanos, duration);
    }
    fn record_merge(&self, duration: Duration) {
        Self::record(&self.merge_count, &self.merge_nanos, duration);
    }
    fn record_sync(&self, duration: Duration) {
        Self::record(&self.sync_count, &self.sync_nanos, duration);
    }
}

/// A running sparkline of `EngineMetricsCollector`'s per-tick *average
/// latency* (not throughput) for one operation kind — reads the
/// collector's cumulative (count, nanos) each tick and charts the delta's
/// average, the same per-tick-history shape `Metric` uses for throughput.
struct LatencyMetric {
    label: &'static str,
    last_count: u64,
    last_nanos: u64,
    history: VecDeque<u64>, // average nanoseconds per call, per tick
}

impl LatencyMetric {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            last_count: 0,
            last_nanos: 0,
            history: VecDeque::with_capacity(HISTORY_LEN),
        }
    }

    fn tick(&mut self, count: u64, nanos: u64) {
        let delta_count = count.saturating_sub(self.last_count);
        let delta_nanos = nanos.saturating_sub(self.last_nanos);
        let avg_ns = delta_nanos
            .checked_div(delta_count)
            .unwrap_or(0);
        self.last_count = count;
        self.last_nanos = nanos;
        if self
            .history
            .len()
            >= HISTORY_LEN
        {
            self.history
                .pop_front();
        }
        self.history
            .push_back(avg_ns);
    }

    fn last_rate_us(&self) -> u64 {
        self.history
            .back()
            .copied()
            .unwrap_or(0)
            / 1000
    }
}

struct App {
    db: Engine,
    engine_metrics: Arc<EngineMetricsCollector>,
    started: Instant,
    frames: Metric,
    stored: Metric,
    deleted: Metric,
    /// Frames that weren't a `#commit`, commits with no `app.bsky.feed.post`
    /// ops, and post ops that failed to decode — everything this example
    /// deliberately doesn't act on, bucketed into one counter so it's still
    /// visible that the firehose carries far more than just posts.
    skipped: Metric,
    put_latency: LatencyMetric,
    delete_latency: LatencyMetric,
    ticks_since_merge: u32,
    log: VecDeque<String>,
}

impl App {
    fn new(db: Engine, engine_metrics: Arc<EngineMetricsCollector>) -> Self {
        Self {
            db,
            engine_metrics,
            started: Instant::now(),
            frames: Metric::new("Frames/s"),
            stored: Metric::new("Posts stored/s"),
            deleted: Metric::new("Posts deleted/s"),
            skipped: Metric::new("Skipped/s"),
            put_latency: LatencyMetric::new("Put latency"),
            delete_latency: LatencyMetric::new("Delete latency"),
            ticks_since_merge: 0,
            log: VecDeque::with_capacity(LOG_LEN),
        }
    }

    fn tick(&mut self) {
        self.frames
            .tick();
        self.stored
            .tick();
        self.deleted
            .tick();
        self.skipped
            .tick();

        self.put_latency
            .tick(
                self.engine_metrics
                    .put_count
                    .load(Ordering::Relaxed),
                self.engine_metrics
                    .put_nanos
                    .load(Ordering::Relaxed),
            );
        self.delete_latency
            .tick(
                self.engine_metrics
                    .delete_count
                    .load(Ordering::Relaxed),
                self.engine_metrics
                    .delete_nanos
                    .load(Ordering::Relaxed),
            );

        self.ticks_since_merge += 1;
        if self.ticks_since_merge >= MERGE_INTERVAL_TICKS {
            self.ticks_since_merge = 0;
            match self
                .db
                .merge()
                .and_then(|()| {
                    self.db
                        .sync()
                }) {
                Ok(()) => self.log("(background merge + sync completed)".to_string()),
                Err(err) => self.log(format!("warning: background merge/sync failed: {err}")),
            }
        }
    }

    fn log(&mut self, line: String) {
        if self
            .log
            .len()
            >= LOG_LEN
        {
            self.log
                .pop_front();
        }
        self.log
            .push_back(line);
    }

    async fn handle_frame(&mut self, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        self.frames
            .record(1);

        let mut cursor = IoCursor::new(bytes);
        let header: FrameHeader = serde_ipld_dagcbor::de::from_reader_once(&mut cursor)?;
        if header.op != 1
            || header
                .t
                .as_deref()
                != Some("#commit")
        {
            self.skipped
                .record(1);
            return Ok(());
        }
        let commit: CommitData = serde_ipld_dagcbor::de::from_reader_once(&mut cursor)?;

        let post_ops: Vec<_> = commit
            .ops
            .iter()
            .filter(|op| {
                op.path
                    .starts_with(&format!("{POST_COLLECTION}/"))
            })
            .collect();
        if post_ops.is_empty() {
            self.skipped
                .record(1);
            return Ok(());
        }

        // The commit's `blocks` is a CAR file covering only what changed in
        // this one commit — decoded lazily, and only once per frame, since
        // a frame with nothing but deletes never needs it.
        let mut car_blocks: Option<Vec<(String, Vec<u8>)>> = None;

        for op in post_ops {
            let Some(rkey) = op
                .path
                .rsplit('/')
                .next()
            else {
                self.skipped
                    .record(1);
                continue;
            };
            let key = post_key(
                commit
                    .repo
                    .as_str(),
                rkey,
            );

            match op
                .action
                .as_str()
            {
                "delete" => {
                    self.db
                        .delete(&key, now_unix())?;
                    self.deleted
                        .record(1);
                    self.log(format!(
                        "[{}] {rkey}: (deleted)",
                        commit
                            .repo
                            .as_str()
                    ));
                }
                "create" | "update" => {
                    let Some(cid_link) = &op.cid else {
                        self.skipped
                            .record(1);
                        continue;
                    };
                    if car_blocks.is_none() {
                        let mut reader = AsyncCursor::new(
                            commit
                                .blocks
                                .as_slice(),
                        );
                        let (blocks, _header) = rs_car::car_read_all(&mut reader, false).await?;
                        car_blocks = Some(
                            blocks
                                .into_iter()
                                .map(|(cid, bytes)| (cid.to_string(), bytes))
                                .collect(),
                        );
                    }
                    let target = cid_link
                        .0
                        .to_string();
                    let Some((_, block_bytes)) = car_blocks
                        .as_ref()
                        .unwrap()
                        .iter()
                        .find(|(cid, _)| *cid == target)
                    else {
                        self.skipped
                            .record(1); // referenced block not in this commit's diff
                        continue;
                    };
                    let Ok(record) = serde_ipld_dagcbor::from_slice::<PostRecordData>(block_bytes)
                    else {
                        self.skipped
                            .record(1); // not decodable as a post record
                        continue;
                    };
                    let created_at = record
                        .created_at
                        .as_str()
                        .parse::<chrono::DateTime<chrono::FixedOffset>>()
                        .map(|dt| {
                            dt.timestamp()
                                .max(0) as u32
                        })
                        .unwrap_or_else(|_| now_unix());
                    self.db
                        .put(&key, &encode_post(&record.text, created_at), now_unix())?;
                    self.stored
                        .record(1);
                    self.log(format!(
                        "[{}] {rkey}: {}",
                        commit
                            .repo
                            .as_str(),
                        truncate_for_display(&record.text, 140)
                    ));
                }
                _ => self
                    .skipped
                    .record(1),
            }
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let [
            header,
            totals,
            charts,
            engine_totals,
            engine_charts,
            log,
            footer,
        ] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(16),
            Constraint::Length(1),
            Constraint::Length(10),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let uptime = self
            .started
            .elapsed()
            .as_secs();
        frame.render_widget(
            Line::from(format!(
                "yabiir bsky firehose — connected — uptime {uptime}s"
            )),
            header,
        );
        frame.render_widget(
            Line::from(format!(
                "stored: {}   deleted: {}   frames: {}   skipped: {}",
                self.stored
                    .total,
                self.deleted
                    .total,
                self.frames
                    .total,
                self.skipped
                    .total
            )),
            totals,
        );

        let chart_areas: [_; 4] = Layout::horizontal([
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
            Constraint::Ratio(1, 4),
        ])
        .areas(charts);
        for (area, metric) in chart_areas
            .iter()
            .zip([&self.frames, &self.stored, &self.deleted, &self.skipped])
        {
            let data: Vec<u64> = metric
                .history
                .iter()
                .copied()
                .collect();
            let sparkline = Sparkline::default()
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!("{} ({}/s)", metric.label, metric.last_rate())),
                )
                .data(&data)
                .style(Style::default().fg(Color::Cyan));
            frame.render_widget(sparkline, *area);
        }

        // The yabiir engine's own observability hooks (docs/ROADMAP.md §4),
        // on top of this app's bsky-specific stats above: Options::metrics
        // (EngineMetricsCollector, this file) feeds these directly.
        let (merge_count, merge_avg_ns) = EngineMetricsCollector::snapshot(
            &self
                .engine_metrics
                .merge_count,
            &self
                .engine_metrics
                .merge_nanos,
        );
        let (sync_count, sync_avg_ns) = EngineMetricsCollector::snapshot(
            &self
                .engine_metrics
                .sync_count,
            &self
                .engine_metrics
                .sync_nanos,
        );
        let (get_count, get_avg_ns) = EngineMetricsCollector::snapshot(
            &self
                .engine_metrics
                .get_count,
            &self
                .engine_metrics
                .get_nanos,
        );
        frame.render_widget(
            Line::from(format!(
                "engine: puts avg {}µs   deletes avg {}µs   gets: {get_count} (avg {}µs)   \
                 merges: {merge_count} (avg {}ms)   syncs: {sync_count} (avg {}ms)",
                self.put_latency
                    .last_rate_us(),
                self.delete_latency
                    .last_rate_us(),
                get_avg_ns / 1000,
                merge_avg_ns / 1_000_000,
                sync_avg_ns / 1_000_000,
            )),
            engine_totals,
        );

        let engine_chart_areas: [_; 2] =
            Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                .areas(engine_charts);
        for (area, latency) in engine_chart_areas
            .iter()
            .zip([&self.put_latency, &self.delete_latency])
        {
            let data: Vec<u64> = latency
                .history
                .iter()
                .map(|ns| ns / 1000) // ns -> µs for display
                .collect();
            let sparkline = Sparkline::default()
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(
                            "{} ({}µs avg)",
                            latency.label,
                            latency.last_rate_us()
                        )),
                )
                .data(&data)
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(sparkline, *area);
        }

        let items: Vec<ListItem> = self
            .log
            .iter()
            .rev()
            .take(log.height as usize)
            .map(|line| ListItem::new(line.as_str()))
            .collect();
        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Activity"),
            ),
            log,
        );

        frame.render_widget(
            Paragraph::new("q/Esc to quit").style(Style::default().fg(Color::DarkGray)),
            footer,
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // tokio-tungstenite's rustls backend needs an explicit process-wide
    // crypto provider installed before the first TLS (wss://) connection.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls's ring crypto provider (should only be called once)");

    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("yabiir-bsky-firehose"));
    let engine_metrics = Arc::new(EngineMetricsCollector::default());
    let db = Engine::open(
        &dir,
        Options {
            metrics: Some(engine_metrics.clone() as Arc<dyn Metrics>),
            ..Options::default()
        },
    )?;

    let (ws, _response) = tokio_tungstenite::connect_async(FIREHOSE_URL).await?;
    let (_write, mut read) = ws.split();

    let mut terminal = ratatui::init();
    let mut app = App::new(db, engine_metrics);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let result = 'outer: loop {
        tokio::select! {
            _ = tick.tick() => {
                app.tick();
                if let Err(err) = terminal.draw(|f| app.draw(f)) {
                    break 'outer Err(err.into());
                }
            }
            event = events.try_next() => {
                match event {
                    Ok(Some(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                            break 'outer Ok(());
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break 'outer Ok(()), // terminal input closed
                    Err(err) => break 'outer Err(err.into()),
                }
            }
            frame = read.next() => {
                match frame {
                    Some(Ok(WsMessage::Binary(bytes))) => {
                        if let Err(err) = app.handle_frame(&bytes).await {
                            app.log(format!("warning: skipping one frame: {err}"));
                        }
                    }
                    Some(Ok(_)) => {} // ping/pong/text/close — nothing to decode
                    Some(Err(err)) => break 'outer Err(err.into()),
                    None => break 'outer Ok(()), // firehose closed the connection
                }
            }
        }
    };

    ratatui::restore();
    app.db
        .close()?;
    result
}
