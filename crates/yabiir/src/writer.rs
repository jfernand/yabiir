//! The single writer: one thread, the only one that appends to the log.
//!
//! Every write is a batch ([`crate::batch`]) sent to the writer thread over
//! a channel. The thread owns the active file, the counters that name new
//! files and number new writes, and nothing else touches them. It takes
//! whatever batches are queued as one group and, for the whole group:
//!
//! 1. checks each batch's conditions, against the keydir and the batches
//!    before it in the group;
//! 2. appends each batch that passes as one record;
//! 3. hands the records to the OS in one `write`, and, with `sync_on_put`,
//!    makes them durable with one fsync;
//! 4. publishes them to the keydir, in log order, under one lock;
//! 5. completes each batch's [`Commit`](crate::Commit).
//!
//! So a write is visible to readers only once it is in the log (and durable,
//! with `sync_on_put`), the keydir changes in log order, and concurrent
//! writers share one `write` and one fsync without a group commit protocol:
//! a group is simply everything that queued while the previous one was being
//! written.
//!
//! A merge starts with a [`seal`](Writer::seal_for_merge) and `sync` is a
//! command too, so both are ordered with the writes around them.
//!
//! ## When something fails
//!
//! The kernel reports a failed write-back once and then forgets it: the
//! pages are marked clean, and the next fsync of the same file can succeed
//! although the data never reached the disk. So after a failed `write` or
//! fsync the writer stops appending to that file, without syncing it again,
//! and fails every batch of the group. Their records may or may not survive
//! a crash; they are not published, so this process doesn't see them. A
//! write whose commit returned `Ok` survives a crash with `sync_on_put`, and
//! a crash of the process without it.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::batch::{Completer, Op};
use crate::datafile::{ActiveFile, DataFileSet};
use crate::error::Error;
use crate::format::{self, OpRef};
use crate::keydir::{Keydir, KeydirEntry, SharedKeydir};

/// The handle on the writer thread. Dropping it stops the thread once every
/// command sent before has been carried out.
pub(crate) struct Writer {
    commands: Option<Sender<Command>>,
    thread: Option<JoinHandle<()>>,
    max_file_size: u64,
    /// How many fsyncs of the active file the thread has run, for tests.
    #[cfg(test)]
    n_fsyncs: Arc<AtomicUsize>,
}

/// A batch on its way to the writer: validated operations, the timestamp
/// for its record, and who to tell how it ended.
pub(crate) struct Batch {
    pub(crate) ops: Vec<Op>,
    pub(crate) timestamp: u32,
    pub(crate) done: Completer,
    /// Run on the writer thread after the batch's record is written and
    /// before it is published. Tests use it to try to slip a merge or
    /// another write into that window.
    #[cfg(test)]
    pub(crate) between_append_and_publish: Option<Box<dyn FnOnce() + Send>>,
}

enum Command {
    Write(Batch),
    Sync(Sender<io::Result<()>>),
    Seal(Sender<io::Result<Option<MergeScope>>>),
}

/// What a merge may read and write, fixed at the instant it starts. See
/// [`Writer::seal_for_merge`].
pub(crate) struct MergeScope {
    /// The data files to compact, ascending: every data file there was when
    /// the merge started. None of them is appended to again.
    pub(crate) input_ids: Vec<u32>,
    /// The ids reserved for the merge's output files: above every input,
    /// below every file written from now on.
    pub(crate) output_ids: Range<u32>,
    /// The sequence number the next write will get. Every record in the
    /// input has a lower one, including the tombstones merge drops, so the
    /// output files carry it as their floor (see `recovery::recover`).
    pub(crate) seq_floor: u64,
}

impl Writer {
    /// Start the writer thread for the store in `files`' directory. Its
    /// first new data file will be `next_file_id`, and its first write will
    /// get sequence number `next_seq`.
    pub(crate) fn start(
        files: Arc<DataFileSet>,
        keydir: Arc<SharedKeydir>,
        next_file_id: u32,
        next_seq: u64,
        max_file_size: u64,
        should_sync_on_write: bool,
    ) -> io::Result<Self> {
        let (commands, received) = mpsc::channel();
        #[cfg(test)]
        let n_fsyncs = Arc::new(AtomicUsize::new(0));
        let worker = Worker {
            files,
            keydir,
            received,
            postponed: VecDeque::new(),
            active: None,
            next_file_id,
            next_seq,
            has_unreported_sync_failure: false,
            max_file_size,
            should_sync_on_write,
            #[cfg(test)]
            n_fsyncs: Arc::clone(&n_fsyncs),
        };
        let thread = thread::Builder::new()
            .name("yabiir-writer".into())
            .spawn(move || worker.run())?;
        Ok(Self {
            commands: Some(commands),
            thread: Some(thread),
            max_file_size,
            #[cfg(test)]
            n_fsyncs,
        })
    }

    pub(crate) fn max_file_size(&self) -> u64 {
        self.max_file_size
    }

    /// Queue `batch`. Its `done` is completed when the writer has decided
    /// it, or with an error if the writer is gone.
    pub(crate) fn submit(&self, batch: Batch) {
        let Some(commands) = &self.commands else {
            return; // Dropping the batch fails its commit.
        };
        // If the thread is gone the batch comes back in the error, and
        // dropping it fails its commit.
        let _ = commands.send(Command::Write(batch));
    }

    /// Make every write so far durable: flush and fsync the active file, if
    /// there is one. Fails if that fsync fails, or if the fsync of a file
    /// closed since the last `sync` did.
    pub(crate) fn sync(&self) -> io::Result<()> {
        self.ask(Command::Sync)?
    }

    /// Fix what a merge starting now will work on, in one step on the
    /// writer thread: close the active file, and reserve ids for the
    /// merge's output below the id the next active file will get. Returns
    /// `None` if the store has no data files, so there is nothing to merge.
    ///
    /// Recovery replays data files in id order and lets a later entry
    /// replace an earlier one. A merge copies old entries into new files, so
    /// those files must sort *below* every file that receives a write after
    /// the merge started. Otherwise a reopen would replay the merge's copy
    /// of a key after a newer write to it, and bring the old value back.
    pub(crate) fn seal_for_merge(&self) -> io::Result<Option<MergeScope>> {
        self.ask(Command::Seal)?
    }

    /// [`sync`](Self::sync), then stop the thread.
    pub(crate) fn close(mut self) -> io::Result<()> {
        let result = self.sync();
        self.stop();
        result
    }

    /// Send the command `make` builds around a reply channel, and wait for
    /// the reply.
    fn ask<T>(&self, make: impl FnOnce(Sender<T>) -> Command) -> io::Result<T> {
        let (reply, answer) = mpsc::channel();
        self.commands
            .as_ref()
            .ok_or_else(writer_gone)?
            .send(make(reply))
            .map_err(|_| writer_gone())?;
        answer
            .recv()
            .map_err(|_| writer_gone())
    }

    /// Close the channel and wait for the thread to finish what it was sent.
    fn stop(&mut self) {
        self.commands = None;
        if let Some(thread) = self
            .thread
            .take()
        {
            // A panic on the writer thread has already failed every batch
            // it held; there is nothing more to report here.
            let _ = thread.join();
        }
    }

    #[cfg(test)]
    pub(crate) fn fsync_call_count(&self) -> usize {
        self.n_fsyncs
            .load(Ordering::SeqCst)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn writer_gone() -> io::Error {
    io::Error::other("the writer thread has stopped")
}

/// The writer thread's state.
struct Worker {
    files: Arc<DataFileSet>,
    keydir: Arc<SharedKeydir>,
    received: Receiver<Command>,
    /// Commands taken off the channel but not carried out yet: the next
    /// group starts with them.
    postponed: VecDeque<Command>,
    /// The file being appended to. `None` until the first write after open,
    /// after a rotation, after a merge starts, and after a write or an fsync
    /// of the file failed: the next write creates the next file, so a store
    /// that isn't written to gains no files.
    active: Option<ActiveFile>,
    /// The id the next new data file gets. Above every id ever handed out,
    /// whether or not its file was created.
    next_file_id: u32,
    /// The sequence number the next write gets. Above every one ever handed
    /// out, whether or not its record reached the log.
    next_seq: u64,
    /// Set when the fsync that closes a file failed and nobody was told:
    /// without `sync_on_put` no one was waiting on it. The next `sync` or
    /// `close` reports it, since it can't truthfully say everything is
    /// durable.
    has_unreported_sync_failure: bool,
    max_file_size: u64,
    should_sync_on_write: bool,
    #[cfg(test)]
    n_fsyncs: Arc<AtomicUsize>,
}

/// A batch of the group being written, after its conditions held.
struct Written {
    batch: Batch,
    /// Its sequence number, and where its record went: `None` if it had
    /// nothing to write.
    record: Option<RecordAt>,
}

struct RecordAt {
    seq: u64,
    file_id: u32,
    /// Where each operation's value is in the file; `None` for a tombstone.
    value_positions: Vec<Option<u64>>,
}

impl Worker {
    fn run(mut self) {
        while let Some(command) = self.next_command() {
            match command {
                Command::Write(batch) => self.write_group(batch),
                Command::Sync(reply) => {
                    let _ = reply.send(self.sync());
                }
                Command::Seal(reply) => {
                    let _ = reply.send(self.seal_for_merge());
                }
            }
        }
        // The channel is closed: the store is being closed or dropped.
        // Every write so far was handed to the OS when its group ended.
    }

    /// The next command, waiting for one if none is queued. `None` once the
    /// channel is closed and everything sent has been carried out.
    fn next_command(&mut self) -> Option<Command> {
        self.postponed
            .pop_front()
            .or_else(|| {
                self.received
                    .recv()
                    .ok()
            })
    }

    /// The next command if one is already queued, without waiting.
    fn try_next_command(&mut self) -> Option<Command> {
        self.postponed
            .pop_front()
            .or_else(|| {
                // Empty or disconnected alike: nothing more to take now.
                self.received
                    .try_recv()
                    .ok()
            })
    }

    /// Write `first` and every write queued behind it as one group: see the
    /// module docs. A group ends at the first command that isn't a write,
    /// when the queue is empty, or when the active file fills up; whatever
    /// is left waits for the next group.
    fn write_group(&mut self, first: Batch) {
        let mut overlay = Overlay::default();
        let mut written: Vec<Written> = Vec::new();
        let mut next = Some(first);

        while let Some(batch) = next.take() {
            if let Err(index) = overlay.check(&self.keydir, &batch.ops) {
                batch
                    .done
                    .complete(Err(Error::ConditionFailed { index }));
            } else {
                let ops = overlay.effective_ops(&self.keydir, &batch.ops);
                if ops.is_empty() {
                    written.push(Written {
                        batch,
                        record: None,
                    });
                } else {
                    match self.append(&ops, batch.timestamp) {
                        Ok(record) => {
                            overlay.apply(&ops, record.seq);
                            written.push(Written {
                                batch,
                                record: Some(record),
                            });
                        }
                        Err(e) => {
                            // The file may now end in part of a record.
                            // Nothing more goes into it, and no batch
                            // appended to it with this one is published.
                            self.active = None;
                            batch
                                .done
                                .complete(Err(e.into()));
                            fail_all(&mut written, "a write before this one in its group failed");
                            break;
                        }
                    }
                }
            }
            if self.is_active_full() {
                break;
            }
            match self.try_next_command() {
                Some(Command::Write(batch)) => next = Some(batch),
                Some(other) => self
                    .postponed
                    .push_front(other),
                None => {}
            }
        }

        if let Err(e) = self.make_group_durable(&written) {
            for w in written.drain(..) {
                w.batch
                    .done
                    .complete(Err(Error::Io(io::Error::new(e.kind(), e.to_string()))));
            }
            return;
        }
        self.publish(&mut written);
        if self.is_active_full() {
            let _ = self.retire_active();
        }
    }

    /// Append one record of `ops` to the active file, without flushing it.
    fn append(&mut self, ops: &[OpRef<'_>], timestamp: u32) -> io::Result<RecordAt> {
        let seq = self.next_seq;
        // Spent whether or not the append succeeds: the record may reach the
        // file in part.
        self.next_seq += 1;
        let encoded = format::encode_record(seq, timestamp, ops);
        let active = self.active_file()?;
        let record_pos = active.append_buffered(&encoded)?;
        let value_positions = ops
            .iter()
            .enumerate()
            .map(|(i, op)| (!op.tombstone).then(|| record_pos + encoded.value_offset(i)))
            .collect();
        Ok(RecordAt {
            seq,
            file_id: active.file_id(),
            value_positions,
        })
    }

    /// Hand the group's records to the OS and, with `sync_on_put`, fsync
    /// them. On failure the active file is dropped unsynced.
    fn make_group_durable(&mut self, written: &[Written]) -> io::Result<()> {
        let has_records = written
            .iter()
            .any(|w| {
                w.record
                    .is_some()
            });
        if !has_records {
            return Ok(());
        }
        let Some(active) = &mut self.active else {
            return Ok(());
        };
        let result = if self.should_sync_on_write {
            #[cfg(test)]
            self.n_fsyncs
                .fetch_add(1, Ordering::SeqCst);
            active.sync()
        } else {
            active.flush()
        };
        if result.is_err() {
            self.active = None;
        }
        result
    }

    /// Point the keydir at every record of the group, in log order, under
    /// one lock, then complete each batch.
    fn publish(&mut self, written: &mut Vec<Written>) {
        #[cfg(test)]
        for w in written.iter_mut() {
            if let Some(hook) = w
                .batch
                .between_append_and_publish
                .take()
            {
                hook();
            }
        }
        self.keydir
            .with_write(|keydir| {
                for w in written.iter() {
                    if let Some(record) = &w.record {
                        publish_batch(keydir, &w.batch, record);
                    }
                }
            });
        for w in written.drain(..) {
            let version = w
                .record
                .as_ref()
                .map(|r| r.seq);
            w.batch
                .done
                .complete(Ok(version));
        }
    }

    /// See [`Writer::sync`].
    fn sync(&mut self) -> io::Result<()> {
        let result = match &mut self.active {
            Some(active) => active.sync(),
            None => Ok(()),
        };
        if result.is_err() {
            self.active = None;
        }
        let had_earlier_failure = std::mem::take(&mut self.has_unreported_sync_failure);
        result?;
        if had_earlier_failure {
            return Err(io::Error::other(
                "the fsync of a data file closed earlier failed; its writes may not be durable",
            ));
        }
        Ok(())
    }

    /// See [`Writer::seal_for_merge`].
    ///
    /// The number of output files isn't known yet, but it has a bound: the
    /// output holds at most the input's bytes, and every output file but the
    /// last is filled to `max_file_size`.
    fn seal_for_merge(&mut self) -> io::Result<Option<MergeScope>> {
        self.retire_active()?;
        let input_ids = DataFileSet::discover(
            self.files
                .dir(),
        )?;
        let Some(&newest_input_id) = input_ids.last() else {
            return Ok(None);
        };

        let mut input_bytes = 0u64;
        for &id in &input_ids {
            input_bytes += self
                .files
                .len_of(id)?;
        }
        let max_output_files = input_bytes
            / self
                .max_file_size
                .max(1)
            + 1;
        // `next_file_id` is above every input already. The `max` keeps the
        // merge safe even if it weren't: an output file that shared an id
        // with an input would be removed with it.
        let first_output_id = self
            .next_file_id
            .max(newest_input_id.saturating_add(1));
        let end_of_output_ids = u32::try_from(max_output_files)
            .ok()
            .and_then(|n| first_output_id.checked_add(n))
            .ok_or_else(out_of_file_ids)?;
        self.next_file_id = end_of_output_ids;
        Ok(Some(MergeScope {
            input_ids,
            output_ids: first_output_id..end_of_output_ids,
            seq_floor: self.next_seq,
        }))
    }

    fn is_active_full(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.len() >= self.max_file_size)
    }

    /// The active file, created first if there is none.
    fn active_file(&mut self) -> io::Result<&mut ActiveFile> {
        if self
            .active
            .is_none()
        {
            let file_id = self.next_file_id;
            // The id is spent whether or not what follows succeeds: a
            // failure can leave the file behind, and its id must never be
            // handed out again.
            self.next_file_id = file_id
                .checked_add(1)
                .ok_or_else(out_of_file_ids)?;
            let active = self
                .files
                .create_active(file_id, self.next_seq)?;
            // The file's name has to be durable before any record in it is.
            self.files
                .sync_dir()?;
            self.active = Some(active);
        }
        Ok(self
            .active
            .as_mut()
            .expect("just created if it was missing"))
    }

    /// Stop appending to the active file: flush and fsync it, and close it.
    /// Whatever the fsync's result, the file is closed. This is the whole of
    /// a rotation; the next write creates the next file.
    fn retire_active(&mut self) -> io::Result<()> {
        let Some(mut active) = self
            .active
            .take()
        else {
            return Ok(());
        };
        let result = active.sync();
        if result.is_err() {
            self.has_unreported_sync_failure = true;
        }
        result
    }
}

/// Point the keydir at the operations of `batch`, whose record is `record`.
fn publish_batch(keydir: &mut Keydir, batch: &Batch, record: &RecordAt) {
    let mut positions = record
        .value_positions
        .iter()
        .copied();
    for op in &batch.ops {
        match op {
            Op::Check { .. } => {}
            Op::Delete { key } => {
                // A delete of an absent key wrote nothing, and has no
                // position; one that wrote a tombstone has `None`.
                if keydir
                    .get(key)
                    .is_some()
                {
                    keydir.remove(key);
                }
            }
            Op::Put { key, value } => {
                let value_pos = positions
                    .by_ref()
                    .flatten()
                    .next()
                    .expect("a position for every put");
                keydir.insert(
                    key,
                    KeydirEntry {
                        file_id: record.file_id,
                        value_size: value.len() as u32,
                        value_pos,
                        timestamp: batch.timestamp,
                        version: record.seq,
                    },
                );
            }
        }
    }
}

/// Fail every batch in `written` with `reason`.
fn fail_all(written: &mut Vec<Written>, reason: &str) {
    for w in written.drain(..) {
        w.batch
            .done
            .complete(Err(Error::Io(io::Error::other(reason.to_owned()))));
    }
}

fn out_of_file_ids() -> io::Error {
    io::Error::other("the store has used every data file id")
}

/// The versions keys will have once the group being written is published:
/// what the keydir says, as changed by the group's batches so far. Each
/// batch's conditions are checked against this, so a batch sees every
/// batch queued before it.
#[derive(Default)]
struct Overlay(HashMap<Vec<u8>, Option<u64>>);

impl Overlay {
    /// The version `key` has as of the batches so far: `None` if it has no
    /// value.
    fn version(&self, keydir: &SharedKeydir, key: &[u8]) -> Option<u64> {
        match self
            .0
            .get(key)
        {
            Some(version) => *version,
            None => keydir
                .get(key)
                .map(|entry| entry.version),
        }
    }

    /// `Ok` if every check of `ops` holds before the batch, or the index of
    /// the first one that doesn't.
    fn check(&self, keydir: &SharedKeydir, ops: &[Op]) -> Result<(), usize> {
        for (index, op) in ops
            .iter()
            .enumerate()
        {
            if let Op::Check { key, cond } = op
                && !cond.holds(self.version(keydir, key))
            {
                return Err(index);
            }
        }
        Ok(())
    }

    /// The operations of `ops` that write something, in order: every put,
    /// and every delete of a key that has a value at that point of the
    /// batch.
    fn effective_ops<'b>(&self, keydir: &SharedKeydir, ops: &'b [Op]) -> Vec<OpRef<'b>> {
        let mut within: HashMap<&[u8], bool> = HashMap::new();
        let mut effective = Vec::new();
        for op in ops {
            match op {
                Op::Check { .. } => {}
                Op::Put { key, value } => {
                    within.insert(key, true);
                    effective.push(OpRef {
                        key,
                        value,
                        tombstone: false,
                    });
                }
                Op::Delete { key } => {
                    let is_present = within
                        .get(key.as_slice())
                        .copied()
                        .unwrap_or_else(|| {
                            self.version(keydir, key)
                                .is_some()
                        });
                    if is_present {
                        within.insert(key, false);
                        effective.push(OpRef {
                            key,
                            value: &[],
                            tombstone: true,
                        });
                    }
                }
            }
        }
        effective
    }

    /// Record the writes of `ops`, appended with sequence number `seq`.
    fn apply(&mut self, ops: &[OpRef<'_>], seq: u64) {
        for op in ops {
            let version = (!op.tombstone).then_some(seq);
            self.0
                .insert(
                    op.key
                        .to_vec(),
                    version,
                );
        }
    }
}
