//! The single writer: the one place entries are appended to the log.
//!
//! A [`Writer`] owns the active file, the counter that names new files, and
//! the group-commit coordinator, behind one mutex. Everything that changes
//! the log's shape happens under that mutex: an append, the keydir update
//! that publishes it, a rotation, and the moment a merge decides which files
//! are its input. That is what gives the store one total order of writes.

use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crate::commit::GroupCommit;
use crate::datafile::{ActiveFile, DataFileSet};
use crate::format::EncodedEntry;
use crate::sync::lock;

pub(crate) struct Writer {
    files: Arc<DataFileSet>,
    state: Mutex<State>,
    group_commit: GroupCommit,
    /// The active file is closed once it reaches this many bytes.
    max_file_size: u64,
    /// Whether an append waits for an fsync before it returns.
    should_sync_on_append: bool,
}

struct State {
    /// The file being appended to. `None` until the first append after open,
    /// after a rotation, after a merge starts, and after a write or an fsync
    /// of the file failed: the next append creates the next file, so a store
    /// that isn't written to gains no files.
    active: Option<ActiveFile>,
    /// The id the next new data file gets. Above every id ever handed out,
    /// whether or not its file was created.
    next_file_id: u32,
    /// Set when the fsync that closes a file failed and nobody was told:
    /// the write that filled the file had succeeded, and without
    /// `sync_on_put` no one was waiting on the fsync. The next `sync` or
    /// `close` reports it, since it can't truthfully say everything is
    /// durable.
    has_unreported_sync_failure: bool,
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
}

impl Writer {
    /// A writer for the store in `files`' directory, whose first new data
    /// file will be `next_file_id`.
    pub(crate) fn new(
        files: Arc<DataFileSet>,
        next_file_id: u32,
        max_file_size: u64,
        should_sync_on_append: bool,
    ) -> Self {
        Self {
            files,
            state: Mutex::new(State {
                active: None,
                next_file_id,
                has_unreported_sync_failure: false,
            }),
            group_commit: GroupCommit::new(),
            max_file_size,
            should_sync_on_append,
        }
    }

    /// Append `encoded` to the active file and run `publish` with the file
    /// id and the position its value landed at, both under the writer's
    /// lock. Then close the file if it is full and, with `sync_on_put`, wait
    /// for an fsync that covers the entry.
    ///
    /// `publish` is where the caller updates the keydir. Running it under
    /// the lock makes the keydir change in the same order as the log: two
    /// writes to one key can't publish out of log order, and a merge, which
    /// picks its input under this lock, never sees an appended entry that
    /// isn't published yet.
    ///
    /// The fsync happens after the lock is released, so that concurrent
    /// writers share one (see `crate::commit`). With `sync_on_put` a write
    /// is therefore visible to readers shortly before it is durable and
    /// before its `put` returns.
    ///
    /// On an error from the append itself, nothing was published. On an
    /// error from the fsync, the entry is in the log and published, but
    /// isn't known to be durable.
    pub(crate) fn append(
        &self,
        encoded: &EncodedEntry,
        publish: impl FnOnce(u32, u64),
    ) -> io::Result<()> {
        let generation = {
            let mut state = lock(&self.state);
            let active = self.active_file(&mut state)?;
            let value_pos = match active.append(encoded) {
                Ok(value_pos) => value_pos,
                Err(e) => {
                    // The file may now end in part of an entry. Nothing more
                    // goes into it: recovery reads it like the torn tail a
                    // crash leaves.
                    let _ = self.retire_active(&mut state);
                    return Err(e);
                }
            };
            publish(active.file_id(), value_pos);
            let is_full = active.len() >= self.max_file_size;
            // Without `sync_on_put` nobody waits for the entry to be durable.
            let generation = self
                .should_sync_on_append
                .then(|| {
                    self.group_commit
                        .record_pending()
                });
            if is_full {
                // If closing the file fails, the entry is still appended and
                // published. Whether it is durable is group commit's to say,
                // below, and `retire_active` has told it.
                let _ = self.retire_active(&mut state);
            }
            generation
        };

        match generation {
            Some(generation) => self.wait_until_durable(generation),
            None => Ok(()),
        }
    }

    pub(crate) fn max_file_size(&self) -> u64 {
        self.max_file_size
    }

    /// Make every write so far durable: flush and fsync the active file, if
    /// there is one. Fails if that fsync fails, or if the fsync of a file
    /// closed since the last `sync` did.
    pub(crate) fn sync(&self) -> io::Result<()> {
        let mut state = lock(&self.state);
        let result = match &mut state.active {
            Some(active) => active.sync(),
            None => Ok(()),
        };
        match result {
            Ok(()) => self
                .group_commit
                .mark_all_durable(),
            Err(_) => {
                self.group_commit
                    .mark_all_failed();
                state.active = None;
            }
        }
        let had_earlier_failure = std::mem::take(&mut state.has_unreported_sync_failure);
        result?;
        if had_earlier_failure {
            return Err(unreported_sync_failure());
        }
        Ok(())
    }

    /// [`sync`](Self::sync), consuming the writer.
    pub(crate) fn close(self) -> io::Result<()> {
        self.sync()
    }

    /// Fix what a merge starting now will work on, in one step under the
    /// writer's lock: close the active file, and reserve ids for the merge's
    /// output below the id the next active file will get. Returns `None` if
    /// the store has no data files, so there is nothing to merge.
    ///
    /// Recovery replays data files in id order and lets a later entry
    /// replace an earlier one. A merge copies old entries into new files, so
    /// those files must sort *below* every file that receives a write after
    /// the merge started. Otherwise a reopen would replay the merge's copy
    /// of a key after a newer write to it, and bring the old value back.
    ///
    /// The number of output files isn't known yet, but it has a bound: the
    /// output holds at most the input's bytes, and every output file but the
    /// last is filled to `max_file_size`.
    pub(crate) fn seal_for_merge(&self) -> io::Result<Option<MergeScope>> {
        let mut state = lock(&self.state);
        self.retire_active(&mut state)?;
        let input_ids = DataFileSet::discover(
            self.files
                .dir(),
        )?;
        if input_ids.is_empty() {
            return Ok(None);
        }

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
        let newest_input_id = *input_ids
            .last()
            .expect("checked non-empty above");
        let first_output_id = state
            .next_file_id
            .max(newest_input_id.saturating_add(1));
        let end_of_output_ids = u32::try_from(max_output_files)
            .ok()
            .and_then(|n| first_output_id.checked_add(n))
            .ok_or_else(out_of_file_ids)?;
        state.next_file_id = end_of_output_ids;
        Ok(Some(MergeScope {
            input_ids,
            output_ids: first_output_id..end_of_output_ids,
        }))
    }

    /// The active file, created first if there is none.
    fn active_file<'s>(&self, state: &'s mut State) -> io::Result<&'s mut ActiveFile> {
        if state
            .active
            .is_none()
        {
            let file_id = state.next_file_id;
            // The id is spent whether or not what follows succeeds: a
            // failure can leave the file behind, and its id must never be
            // handed out again.
            state.next_file_id = file_id
                .checked_add(1)
                .ok_or_else(out_of_file_ids)?;
            let active = self
                .files
                .create_active(file_id)?;
            // The file's name has to be durable before any entry in it is.
            self.files
                .sync_dir()?;
            state.active = Some(active);
        }
        Ok(state
            .active
            .as_mut()
            .expect("just created if it was missing"))
    }

    /// Stop appending to the active file: flush and fsync it, and close it.
    /// Whatever the fsync's result, the file is closed, and group commit
    /// learns that result for every write still waiting on it. This is the
    /// whole of a rotation; the next append creates the next file.
    fn retire_active(&self, state: &mut State) -> io::Result<()> {
        let Some(mut active) = state
            .active
            .take()
        else {
            return Ok(());
        };
        let result = active.sync();
        match &result {
            Ok(()) => self
                .group_commit
                .mark_all_durable(),
            Err(_) => {
                self.group_commit
                    .mark_all_failed();
                state.has_unreported_sync_failure = true;
            }
        }
        result
    }

    /// Wait for an fsync that covers `generation`, running it if nobody else
    /// is. The fsync runs outside the writer's lock, on a second handle onto
    /// the active file, so appends go on meanwhile.
    ///
    /// If the fsync fails, the file may have lost bytes the OS had accepted,
    /// and a later fsync of it could succeed all the same (see
    /// `crate::commit`). So the file is dropped, unsynced, before the error
    /// reaches group commit: no other writer can append behind the lost
    /// bytes or lead another fsync of the same file, and group commit fails
    /// every write the fsync covered.
    fn wait_until_durable(&self, generation: u64) -> io::Result<()> {
        self.group_commit
            .commit(generation, || {
                let (file_id, handle) = {
                    let mut state = lock(&self.state);
                    let Some(active) = &mut state.active else {
                        // Closed since the append, which settled every
                        // write in it: there is nothing left to sync.
                        return Ok(());
                    };
                    let file_id = active.file_id();
                    match active.sync_handle() {
                        Ok(handle) => (file_id, handle),
                        Err(e) => {
                            state.active = None;
                            return Err(e);
                        }
                    }
                };
                handle
                    .sync_data()
                    .inspect_err(|_| self.drop_active_if(file_id))
            })
    }

    /// Stop appending to data file `file_id`, without syncing it, if it is
    /// still the active file.
    fn drop_active_if(&self, file_id: u32) {
        let mut state = lock(&self.state);
        let is_still_active = state
            .active
            .as_ref()
            .is_some_and(|active| active.file_id() == file_id);
        if is_still_active {
            state.active = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn fsync_call_count(&self) -> usize {
        self.group_commit
            .fsync_call_count()
    }
}

fn unreported_sync_failure() -> io::Error {
    io::Error::other(
        "the fsync of a data file closed earlier failed; its writes may not be durable",
    )
}

fn out_of_file_ids() -> io::Error {
    io::Error::other("the store has used every data file id")
}
