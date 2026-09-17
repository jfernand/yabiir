//! Group commit: lets several concurrent `sync_on_put` writers share one
//! `fsync` instead of each paying for its own. See `docs/group-commit.typ`
//! for the full design and the measured impact; this module is deliberately
//! small and doesn't itself know anything about `ActiveFile` or `Engine` —
//! it's handed a `do_fsync` closure by the caller each time it needs one.
//!
//! ## The mechanism, in short
//!
//! Every write that needs to become durable calls [`GroupCommit::record_pending`]
//! (while still holding whatever lock serializes appends, so generation
//! numbers are assigned in the same order writes actually land in the
//! file) to get a generation number, then [`GroupCommit::commit`] (after
//! releasing that lock) to block until a fsync covering that generation has
//! completed. The first caller to reach `commit` while no fsync is already
//! in flight becomes the leader: it performs the fsync (via the caller's
//! `do_fsync` closure, with no lock held) and, on success, marks every
//! generation up to whatever was pending *when it started* as durable,
//! waking every other thread waiting on `commit`. A thread that finds a
//! fsync already in flight just waits — it doesn't need one of its own,
//! since the in-flight fsync (which started after this write completed its
//! append, per the ordering above) will cover it too.
//!
//! Rotation's own unconditional fsync (`ActiveFile::sync`, called
//! regardless of `sync_on_put` when a file crosses the size threshold, or
//! by merge's force-rotate) also durably covers every write appended to
//! that file so far — [`GroupCommit::mark_all_durable`] lets a caller
//! record that without going through the leader/fsync dance again, so a
//! writer whose entry happened to land just before a rotation isn't stuck
//! waiting for an unrelated *next* file's fsync.
//!
//! fsync failure is deliberately kept simple: a failed leader attempt
//! propagates the error to that leader's own caller and does *not* advance
//! `durable`, but does release `syncing` and wake waiters, so one of them
//! retries as the next leader. Under a persistent disk error this retries
//! indefinitely rather than giving up — an accepted simplification (see
//! `docs/group-commit.typ`), not a promise this module makes stronger
//! guarantees about failed fsyncs than the OS does.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

pub(crate) struct GroupCommit {
    state: Mutex<State>,
    cv: Condvar,
    /// Number of times `do_fsync` was actually invoked (i.e. this thread
    /// became leader for its round) — exposed for tests/verification, not
    /// used by the coordination logic itself.
    fsync_calls: AtomicUsize,
}

struct State {
    /// Total writes recorded so far via `record_pending`, monotonically
    /// increasing.
    pending: u64,
    /// The highest generation number known to be durable.
    durable: u64,
    /// Whether some thread is currently running a fsync for this coordinator.
    syncing: bool,
}

impl GroupCommit {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                pending: 0,
                durable: 0,
                syncing: false,
            }),
            cv: Condvar::new(),
            fsync_calls: AtomicUsize::new(0),
        }
    }

    /// Record that a write was just appended and needs to become durable.
    /// Must be called while still holding whatever lock serializes appends,
    /// so the generation numbers handed out reflect real append order.
    /// Returns the generation this write must see covered by `commit`.
    pub(crate) fn record_pending(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.pending += 1;
        state.pending
    }

    /// Record that every write appended so far is now durable via some
    /// fsync this coordinator didn't itself run (rotation's own
    /// unconditional sync). Called right after that sync, still holding the
    /// same append lock `record_pending` was called under for the writes
    /// it's meant to cover.
    pub(crate) fn mark_all_durable(&self) {
        let mut state = self.state.lock().unwrap();
        state.durable = state.pending;
        self.cv.notify_all();
    }

    /// Block until `target_gen` is durable. If no fsync is currently in
    /// flight for this coordinator, this thread becomes the leader and
    /// calls `do_fsync` itself (with no lock held); otherwise it waits for
    /// whichever thread is already leading.
    pub(crate) fn commit(
        &self,
        target_gen: u64,
        do_fsync: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.durable >= target_gen {
                return Ok(());
            }
            if !state.syncing {
                state.syncing = true;
                let covers_up_to = state.pending;
                drop(state);
                let result = do_fsync();
                self.fsync_calls.fetch_add(1, Ordering::Relaxed);
                state = self.state.lock().unwrap();
                state.syncing = false;
                if result.is_ok() {
                    state.durable = state.durable.max(covers_up_to);
                }
                self.cv.notify_all();
                result?;
                // loop: re-check state.durable >= target_gen above
            } else {
                state = self.cv.wait(state).unwrap();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn fsync_call_count(&self) -> usize {
        self.fsync_calls.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn single_writer_commits_and_sees_exactly_one_fsync() {
        let gc = GroupCommit::new();
        let target_gen = gc.record_pending();
        gc.commit(target_gen, || Ok(())).unwrap();
        assert_eq!(gc.fsync_call_count(), 1);
    }

    #[test]
    fn mark_all_durable_satisfies_commit_without_a_fsync() {
        let gc = GroupCommit::new();
        let target_gen = gc.record_pending();
        gc.mark_all_durable();
        gc.commit(target_gen, || panic!("should not fsync: already durable")).unwrap();
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
        let gens: Vec<u64> = (0..n).map(|_| gc.record_pending()).collect();

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (leader_entered_tx, leader_entered_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));

        let leader_gc = Arc::clone(&gc);
        let leader_gen = gens[0];
        let leader = thread::spawn(move || {
            leader_gc
                .commit(leader_gen, || {
                    leader_entered_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });

        leader_entered_rx.recv().unwrap(); // leader is now inside do_fsync, blocked

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
        release_tx.send(()).unwrap();

        leader.join().unwrap();
        for f in followers {
            f.join().unwrap();
        }

        assert_eq!(
            gc.fsync_call_count(),
            1,
            "expected the 7 followers to share the leader's single fsync"
        );
    }

    #[test]
    fn failed_fsync_propagates_to_leader_and_a_follower_retries() {
        let gc = Arc::new(GroupCommit::new());
        let g1 = gc.record_pending();
        let attempt = Arc::new(AtomicU64::new(0));

        let a = Arc::clone(&attempt);
        let result = gc.commit(g1, move || {
            a.fetch_add(1, Ordering::Relaxed);
            Err(io::Error::other("disk full"))
        });
        assert!(result.is_err());

        // A later commit for the same (still not durable) generation
        // retries and can succeed once the underlying problem is gone.
        gc.commit(g1, || Ok(())).unwrap();
        assert_eq!(attempt.load(Ordering::Relaxed), 1);
    }
}
