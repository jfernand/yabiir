//! Group commit: lets concurrent `sync_on_put` writers share one fsync
//! instead of each paying for its own. `docs/group-commit.typ` has the
//! design and the measurements. This module knows nothing about files: the
//! caller hands it a `do_fsync` closure when one is needed.
//!
//! ## The mechanism
//!
//! Every write that has to become durable calls
//! [`GroupCommit::record_pending`] while it still holds the lock that
//! serializes appends, and gets a generation number; generations are handed
//! out in append order. After releasing that lock it calls
//! [`GroupCommit::commit`], which blocks until an fsync covering its
//! generation has completed. The first caller to arrive while no fsync is in
//! flight becomes the leader: it runs the fsync with no lock held, and on
//! success marks durable every generation that was pending *when it
//! started*. A caller that finds an fsync in flight waits, then checks
//! again.
//!
//! A sync that happens for another reason (rotation syncs the file it
//! closes; so does an explicit `sync`) also covers every write appended so
//! far. [`GroupCommit::mark_all_durable`] records that, so those writers
//! don't wait for an fsync of their own.
//!
//! ## When an fsync fails
//!
//! The kernel reports a failed write-back once and then forgets it: the
//! pages are marked clean, and the *next* fsync of the same file can succeed
//! although the data never reached the disk. So a failed fsync can't be
//! retried into a success. Instead [`GroupCommit::commit`] fails every write
//! that wasn't durable when the failed fsync started, the leader's and the
//! waiters' alike, even if another sync of the same file succeeded in the
//! meantime: that sync may be the one the kernel didn't tell. The caller
//! must stop appending to the file before its `do_fsync` returns the error
//! (the writer drops the file there and starts a new one), so that no later
//! write lands behind bytes that may be lost. A write that got an error may
//! or may not survive a crash; a write that got `Ok` does.
use std::io;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use crate::sync::{lock, wait};

pub(crate) struct GroupCommit {
    state: Mutex<State>,
    cond_var: Condvar,
    /// How many times `do_fsync` ran. For tests; the coordination doesn't
    /// use it.
    n_fsync_calls: AtomicUsize,
}

struct State {
    /// The generation of the newest write recorded by `record_pending`.
    newest_generation: u64,
    /// Every generation up to this one is durable, unless it is in `failed`.
    durable_generation: u64,
    /// Generations whose fsync failed. They are never reported durable, even
    /// if a later sync succeeds. Adjacent failures merge, so this stays one
    /// range for as long as the disk keeps failing.
    failed: Vec<RangeInclusive<u64>>,
    /// Whether some thread is running an fsync right now.
    is_syncing: bool,
}

impl State {
    fn has_failed(&self, generation: u64) -> bool {
        self.failed
            .iter()
            .any(|range| range.contains(&generation))
    }

    /// Record that the fsync for every generation not yet durable failed.
    fn fail_everything_pending(&mut self) {
        self.fail_everything_after(self.durable_generation);
    }

    /// Record that the fsync for every generation after `durable` failed,
    /// up to the newest. `durable` can be below `durable_generation`: a
    /// leader whose fsync failed passes what was durable when it started,
    /// because a sync that succeeded meanwhile proves nothing about the
    /// writes the failed one covered (see the module docs).
    fn fail_everything_after(&mut self, durable: u64) {
        let first = durable + 1;
        let last = self.newest_generation;
        if first > last {
            return;
        }
        match self
            .failed
            .last_mut()
        {
            Some(range) if *range.end() + 1 >= first => *range = (*range.start()).min(first)..=last,
            _ => self
                .failed
                .push(first..=last),
        }
        // Nothing waits on these any more: they have their answer.
        self.durable_generation = last;
    }
}

/// Armed while a leader runs its fsync. If the fsync panics, this fails the
/// writes it was covering and lets the next writer lead, instead of leaving
/// everyone waiting on a leader that is gone.
struct FailIfAbandoned<'a>(&'a GroupCommit);

impl Drop for FailIfAbandoned<'_> {
    fn drop(&mut self) {
        // Not through `crate::sync::lock`: this runs during a panic, and a
        // second one would abort the process.
        if let Ok(mut state) = self
            .0
            .state
            .lock()
        {
            state.is_syncing = false;
            state.fail_everything_pending();
        }
        self.0
            .cond_var
            .notify_all();
    }
}

impl GroupCommit {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                newest_generation: 0,
                durable_generation: 0,
                failed: Vec::new(),
                is_syncing: false,
            }),
            cond_var: Condvar::new(),
            n_fsync_calls: AtomicUsize::new(0),
        }
    }

    /// Record that a write was just appended and has to become durable.
    /// Call it while still holding the lock that serializes appends, so that
    /// generations follow append order. Returns the generation to pass to
    /// [`commit`](Self::commit).
    pub(crate) fn record_pending(&self) -> u64 {
        let mut state = lock(&self.state);
        state.newest_generation += 1;
        state.newest_generation
    }

    /// Record that every write appended so far is durable, through a sync
    /// this coordinator didn't run. Call it right after that sync, still
    /// holding the lock that serializes appends.
    pub(crate) fn mark_all_durable(&self) {
        let mut state = lock(&self.state);
        state.durable_generation = state.newest_generation;
        self.cond_var
            .notify_all();
    }

    /// Block until `generation` is durable, and return `Ok`; or return an
    /// error if the fsync that covered it failed. If no fsync is in flight
    /// this thread runs `do_fsync` itself, with no lock held; otherwise it
    /// waits for the thread that is running one.
    pub(crate) fn commit(
        &self,
        generation: u64,
        do_fsync: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let mut do_fsync = Some(do_fsync);
        let mut state = lock(&self.state);
        loop {
            if state.has_failed(generation) {
                return Err(io::Error::other(
                    "an fsync covering this write failed; the write may not be durable",
                ));
            }
            if state.durable_generation >= generation {
                return Ok(());
            }
            if state.is_syncing {
                state = wait(&self.cond_var, state);
                continue;
            }

            // Lead: this fsync covers every write appended before it starts,
            // this thread's included. So after it, the loop finds an answer.
            let do_fsync = do_fsync
                .take()
                .expect("a leader's fsync settles its own generation, so it never leads twice");
            state.is_syncing = true;
            let covered = state.newest_generation;
            let durable_before = state.durable_generation;
            drop(state);
            let abandoned = FailIfAbandoned(self);
            let result = do_fsync();
            std::mem::forget(abandoned);
            self.n_fsync_calls
                .fetch_add(1, Ordering::Relaxed);

            state = lock(&self.state);
            state.is_syncing = false;
            match &result {
                Ok(()) => {
                    state.durable_generation = state
                        .durable_generation
                        .max(covered)
                }
                Err(_) => state.fail_everything_after(durable_before),
            }
            self.cond_var
                .notify_all();
            result?;
        }
    }

    /// Record that a sync this coordinator didn't run failed: every write
    /// not yet durable gets an error from [`commit`](Self::commit). The
    /// counterpart of [`mark_all_durable`](Self::mark_all_durable), called
    /// under the same lock.
    pub(crate) fn mark_all_failed(&self) {
        lock(&self.state).fail_everything_pending();
        self.cond_var
            .notify_all();
    }

    #[cfg(test)]
    pub(crate) fn fsync_call_count(&self) -> usize {
        self.n_fsync_calls
            .load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn single_writer_commits_and_sees_exactly_one_fsync() {
        let gc = GroupCommit::new();
        let target_gen = gc.record_pending();
        gc.commit(target_gen, || Ok(()))
            .unwrap();
        assert_eq!(gc.fsync_call_count(), 1);
    }

    #[test]
    fn mark_all_durable_satisfies_commit_without_a_fsync() {
        let gc = GroupCommit::new();
        let target_gen = gc.record_pending();
        gc.mark_all_durable();
        gc.commit(target_gen, || panic!("should not fsync: already durable"))
            .unwrap();
        assert_eq!(gc.fsync_call_count(), 0);
    }

    /// The core group-commit property: N threads that all arrive at
    /// `commit` while one is already fsyncing share that single fsync
    /// rather than each running their own. Uses a real fsync-call delay
    /// (blocked on a channel) so the other threads are deterministically
    /// guaranteed to arrive while the first is still "in flight", rather
    /// than relying on timing.
    #[test]
    fn concurrent_committers_share_one_fsync() {
        let gc = Arc::new(GroupCommit::new());
        let n = 8u64;
        let gens: Vec<u64> = (0..n)
            .map(|_| gc.record_pending())
            .collect();

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (leader_entered_tx, leader_entered_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let leader_gc = Arc::clone(&gc);
        let leader_gen = gens[0];
        let leader = thread::spawn(move || {
            leader_gc
                .commit(leader_gen, || {
                    leader_entered_tx
                        .send(())
                        .unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv()
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        });

        leader_entered_rx
            .recv()
            .unwrap(); // leader is now inside do_fsync, blocked

        let followers: Vec<_> = gens[1..]
            .iter()
            .map(|&g| {
                let gc = Arc::clone(&gc);
                thread::spawn(move || {
                    gc.commit(g, || panic!("follower should not run its own fsync"))
                        .unwrap();
                })
            })
            .collect();

        // Give followers a chance to actually reach `commit` and start
        // waiting before releasing the leader — otherwise this test would
        // pass trivially even if sharing were broken (followers just
        // wouldn't have started yet).
        thread::sleep(Duration::from_millis(50));
        release_tx
            .send(())
            .unwrap();

        leader
            .join()
            .unwrap();
        for f in followers {
            f.join()
                .unwrap();
        }

        assert_eq!(
            gc.fsync_call_count(),
            1,
            "expected the 7 followers to share the leader's single fsync"
        );
    }

    /// A failed fsync can't be retried into a success (see the module
    /// docs): the write it covered stays failed, whoever asks, and whatever
    /// syncs succeed later.
    #[test]
    fn a_failed_fsync_fails_its_writes_for_good() {
        let gc = GroupCommit::new();
        let g1 = gc.record_pending();
        let g2 = gc.record_pending();

        let result = gc.commit(g1, || Err(io::Error::other("disk full")));
        assert!(result.is_err());
        assert_eq!(gc.fsync_call_count(), 1);

        // g2 was appended before the failed fsync, so it failed too, and
        // neither is retried.
        for generation in [g1, g2] {
            gc.commit(generation, || panic!("must not retry a failed fsync"))
                .unwrap_err();
        }
        gc.mark_all_durable();
        gc.commit(g1, || panic!("must not retry a failed fsync"))
            .unwrap_err();

        // Writes appended after the failure are unaffected.
        let g3 = gc.record_pending();
        gc.commit(g3, || Ok(()))
            .unwrap();
    }

    /// While a leader's fsync is failing, another sync of the same file
    /// succeeds (a rotation, say) and marks everything durable. The kernel
    /// reports a write-back error to one fsync only, so that success proves
    /// nothing: the writes the failed fsync covered must still fail.
    #[test]
    fn a_sync_that_succeeds_during_a_failed_fsync_does_not_rescue_its_writes() {
        let gc = GroupCommit::new();
        let g1 = gc.record_pending();
        let g2 = gc.record_pending();

        gc.commit(g1, || {
            gc.mark_all_durable();
            Err(io::Error::other("disk full"))
        })
        .unwrap_err();
        gc.commit(g2, || panic!("must not retry a failed fsync"))
            .unwrap_err();
    }

    /// A follower waiting on a leader whose fsync fails gets the failure
    /// too, without running an fsync of its own.
    #[test]
    fn a_failed_fsync_fails_the_waiting_followers() {
        let gc = Arc::new(GroupCommit::new());
        let leader_gen = gc.record_pending();
        let follower_gen = gc.record_pending();
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let leader_gc = Arc::clone(&gc);
        let leader = thread::spawn(move || {
            leader_gc.commit(leader_gen, || {
                entered_tx
                    .send(())
                    .unwrap();
                release_rx
                    .recv()
                    .unwrap();
                Err(io::Error::other("disk full"))
            })
        });
        entered_rx
            .recv()
            .unwrap(); // the leader is inside its fsync

        let follower_gc = Arc::clone(&gc);
        let follower = thread::spawn(move || {
            follower_gc.commit(follower_gen, || panic!("a follower must not fsync"))
        });
        thread::sleep(Duration::from_millis(50)); // let the follower start waiting
        release_tx
            .send(())
            .unwrap();

        assert!(
            leader
                .join()
                .unwrap()
                .is_err()
        );
        assert!(
            follower
                .join()
                .unwrap()
                .is_err()
        );
        assert_eq!(gc.fsync_call_count(), 1);
    }
}
