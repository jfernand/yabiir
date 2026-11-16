//! `yabiir-diffd`: a yabiir store driven one command at a time, for
//! differential tests that run the same commands against another
//! implementation (the Kotlin port in `kotlin/`) and compare what each
//! returns and what each leaves on disk.
//!
//! The protocol is JSON lines: one command per line on stdin, one reply per
//! line on stdout. Bytes travel as lowercase hex strings. See
//! [`protocol`] for the commands and replies.
//!
//! One store is open at a time. Every write takes its timestamp from the
//! command, so two implementations given the same commands can write the
//! same bytes.

mod protocol;

use std::fs;
use std::io::{self, BufRead, Write};
use std::ops::Bound;
use std::path::PathBuf;

use protocol::{BatchOp, Command, KeyBound, Reply, Value};
use yabiir::{Bitcask, Cond, Engine, Error, KeyOrder, Op, Options};

fn main() -> io::Result<()> {
    let stdin = io::stdin().lock();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut driver = Driver::default();
    for line in stdin.lines() {
        let line = line?;
        if line
            .trim()
            .is_empty()
        {
            continue;
        }
        let reply = match serde_json::from_str::<Command>(&line) {
            Ok(command) => driver.run(command),
            Err(e) => Reply::protocol(format!("bad command: {e}")),
        };
        serde_json::to_writer(&mut stdout, &reply)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

#[derive(Default)]
struct Driver {
    store: Option<Engine>,
}

impl Driver {
    fn run(&mut self, command: Command) -> Reply {
        match self.try_run(command) {
            Ok(value) => Reply::Ok { ok: value },
            Err(Failure::Store(e)) => Reply::from_error(&e),
            Err(Failure::Protocol(message)) => Reply::protocol(message),
        }
    }

    fn store(&self) -> Result<&Engine, Failure> {
        self.store
            .as_ref()
            .ok_or_else(|| Failure::Protocol("no store is open".into()))
    }

    fn try_run(&mut self, command: Command) -> Result<Value, Failure> {
        Ok(match command {
            Command::Open {
                dir,
                max_file_size,
                sync_on_put,
                sorted,
                read_only,
            } => {
                if self
                    .store
                    .is_some()
                {
                    return Err(Failure::Protocol("a store is already open".into()));
                }
                let opts = Options {
                    is_read_write: !read_only,
                    should_sync_on_put: sync_on_put,
                    max_file_size,
                    key_order: if sorted {
                        KeyOrder::Sorted
                    } else {
                        KeyOrder::Hashed
                    },
                    ..Options::default()
                };
                self.store = Some(Engine::open(dir, opts)?);
                Value::Null
            }
            Command::Close => {
                let store = self
                    .store
                    .take()
                    .ok_or_else(|| Failure::Protocol("no store is open".into()))?;
                store.close()?;
                Value::Null
            }
            Command::Drop => {
                drop(
                    self.store
                        .take(),
                );
                Value::Null
            }
            Command::Put { k, v, ts } => {
                self.store()?
                    .put(&k.0, &v.0, ts)?;
                Value::Null
            }
            Command::Delete { k, ts } => {
                self.store()?
                    .delete(&k.0, ts)?;
                Value::Null
            }
            Command::Batch { ops, ts } => {
                let ops = ops
                    .into_iter()
                    .map(BatchOp::into_op)
                    .collect();
                match self
                    .store()?
                    .write_batch_at(ops, ts)
                    .wait()?
                {
                    Some(version) => Value::Version(version),
                    None => Value::Null,
                }
            }
            Command::Get { k } => match self
                .store()?
                .get(&k.0)?
            {
                Some(value) => Value::Bytes(value.into()),
                None => Value::Null,
            },
            Command::GetVersioned { k } => match self
                .store()?
                .get_versioned(&k.0)?
            {
                Some(found) => Value::Versioned {
                    value: found
                        .value
                        .into(),
                    version: found.version,
                },
                None => Value::Null,
            },
            Command::ListKeys => Value::Keys(
                self.store()?
                    .list_keys()?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            ),
            Command::Fold => Value::Pairs(
                self.store()?
                    .fold(
                        |k, v, mut acc: Vec<_>| {
                            acc.push((
                                k.to_vec()
                                    .into(),
                                v.to_vec()
                                    .into(),
                            ));
                            acc
                        },
                        Vec::new(),
                    )?,
            ),
            Command::Scan { start, end } => Value::Pairs(
                self.store()?
                    .scan(bound(&start), bound(&end))?
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            ),
            Command::ScanPage {
                start,
                end,
                reverse,
                limit,
            } => {
                let direction = if reverse {
                    yabiir::Direction::Reverse
                } else {
                    yabiir::Direction::Forward
                };
                Value::Page(
                    self.store()?
                        .scan_page(bound(&start), bound(&end), direction, limit)?
                        .into_iter()
                        .map(|(k, found)| {
                            (
                                k.into(),
                                found
                                    .value
                                    .into(),
                                found.version,
                            )
                        })
                        .collect(),
                )
            }
            Command::Merge => {
                self.store()?
                    .merge()?;
                Value::Null
            }
            Command::Sync => {
                self.store()?
                    .sync()?;
                Value::Null
            }
            Command::SeedV1 {
                dir,
                file_id,
                entries,
            } => {
                seed_v1(dir, file_id, &entries).map_err(Error::Io)?;
                Value::Null
            }
        })
    }
}

/// Write data file `file_id` of `dir` in format version 1, which yabiir no
/// longer writes but still reads.
fn seed_v1(dir: PathBuf, file_id: u32, entries: &[protocol::V1Entry]) -> io::Result<()> {
    fs::create_dir_all(&dir)?;
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend_from_slice(
            yabiir::format::encode_entry(
                &entry
                    .k
                    .0,
                &entry
                    .v
                    .0,
                entry.tombstone,
                entry.ts,
            )
            .as_bytes(),
        );
    }
    fs::write(dir.join(format!("{file_id:020}.bitcask.data")), bytes)
}

fn bound(b: &Option<KeyBound>) -> Bound<&[u8]> {
    match b {
        None => Bound::Unbounded,
        Some(KeyBound::Incl(k)) => Bound::Included(&k.0),
        Some(KeyBound::Excl(k)) => Bound::Excluded(&k.0),
    }
}

impl BatchOp {
    fn into_op(self) -> Op {
        match self {
            BatchOp::Put { k, v } => Op::Put {
                key: k.0,
                value: v.0,
            },
            BatchOp::Delete { k } => Op::Delete { key: k.0 },
            BatchOp::Absent { k } => Op::Check {
                key: k.0,
                cond: Cond::Absent,
            },
            BatchOp::Exists { k } => Op::Check {
                key: k.0,
                cond: Cond::Exists,
            },
            BatchOp::Version { k, version } => Op::Check {
                key: k.0,
                cond: Cond::Version(version),
            },
        }
    }
}

enum Failure {
    Store(Error),
    Protocol(String),
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Failure::Store(e)
    }
}
