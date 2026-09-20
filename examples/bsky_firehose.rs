//! Consumes the AT Protocol ("Bluesky") firehose — the same
//! `com.atproto.sync.subscribeRepos` WebSocket every relay and PDS speaks —
//! and stores every `app.bsky.feed.post` create/update/delete it sees in a
//! `yabiir` datastore, printing each one as it arrives.
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
//! Stop with Ctrl+C.

use std::io::Cursor as IoCursor;
use std::path::PathBuf;

use atrium_api::app::bsky::feed::post::RecordData as PostRecordData;
use atrium_api::com::atproto::sync::subscribe_repos::CommitData;
use futures_util::StreamExt;
use futures_util::io::Cursor as AsyncCursor;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use yabiir::{Bitcask, Engine, Options, now_unix};

const FIREHOSE_URL: &str = "wss://bsky.network/xrpc/com.atproto.sync.subscribeRepos";
const POST_COLLECTION: &str = "app.bsky.feed.post";

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
    println!("using datastore at {}", dir.display());
    let db = Engine::open(&dir, Options::default())?;

    println!("connecting to {FIREHOSE_URL}");
    let (ws, _response) = tokio_tungstenite::connect_async(FIREHOSE_URL).await?;
    let (_write, mut read) = ws.split();
    println!("connected — streaming posts, press Ctrl+C to stop");

    let mut stored = 0u64;
    let mut deleted = 0u64;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nstopping: stored {stored} post(s), deleted {deleted}");
                break;
            }
            frame = read.next() => {
                let Some(frame) = frame else {
                    println!("firehose closed the connection");
                    break;
                };
                let WsMessage::Binary(bytes) = frame? else {
                    continue; // ping/pong/text/close — nothing to decode
                };
                if let Err(err) = handle_frame(&db, &bytes, &mut stored, &mut deleted).await {
                    eprintln!("warning: skipping one frame: {err}");
                }
            }
        }
    }

    db.close()?;
    Ok(())
}

async fn handle_frame(
    db: &Engine,
    bytes: &[u8],
    stored: &mut u64,
    deleted: &mut u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cursor = IoCursor::new(bytes);
    let header: FrameHeader = serde_ipld_dagcbor::de::from_reader_once(&mut cursor)?;
    if header.op != 1
        || header
            .t
            .as_deref()
            != Some("#commit")
    {
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
        return Ok(());
    }

    // The commit's `blocks` is a CAR file covering only what changed in
    // this one commit — decoded lazily, and only once per frame, since a
    // frame with nothing but deletes never needs it.
    let mut car_blocks: Option<Vec<(String, Vec<u8>)>> = None;

    for op in post_ops {
        let Some(rkey) = op
            .path
            .rsplit('/')
            .next()
        else {
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
                db.delete(&key, now_unix())?;
                *deleted += 1;
                println!(
                    "[{}] {rkey}: (deleted)",
                    commit
                        .repo
                        .as_str()
                );
            }
            "create" | "update" => {
                let Some(cid_link) = &op.cid else { continue };
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
                    continue; // referenced block not in this commit's diff
                };
                let Ok(record) = serde_ipld_dagcbor::from_slice::<PostRecordData>(block_bytes)
                else {
                    continue; // not decodable as a post record - skip, don't fail the frame
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
                db.put(&key, &encode_post(&record.text, created_at), now_unix())?;
                *stored += 1;
                println!(
                    "[{}] {rkey}: {}",
                    commit
                        .repo
                        .as_str(),
                    truncate_for_display(&record.text, 140)
                );
            }
            _ => {}
        }
    }
    Ok(())
}
