//! Write batches: the operations a caller hands [`Engine::write_batch`], and
//! the [`Commit`] that tells it how the batch ended.
//!
//! [`Engine::write_batch`]: crate::Engine::write_batch

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::sync::{lock, wait};

/// One operation of a write batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Store `value` under `key`.
    Put { key: Vec<u8>, value: Vec<u8> },
    /// Remove `key`. Removing a key that isn't there does nothing.
    Delete { key: Vec<u8> },
    /// Write nothing, but fail the whole batch unless `key` meets `cond`.
    Check { key: Vec<u8>, cond: Cond },
}

/// A condition on a key, for [`Op::Check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    /// The key has no value.
    Absent,
    /// The key has a value.
    Exists,
    /// The key has a value, and its version is exactly this one (see
    /// [`Engine::get_versioned`](crate::Engine::get_versioned)).
    Version(u64),
}

impl Cond {
    /// Whether a key whose current version is `version` (`None`: no value)
    /// meets this condition.
    pub(crate) fn holds(self, version: Option<u64>) -> bool {
        match self {
            Cond::Absent => version.is_none(),
            Cond::Exists => version.is_some(),
            Cond::Version(expected) => version == Some(expected),
        }
    }
}

/// How a write batch ended: `Ok(Some(version))` if it wrote, with the
/// version every key it put now has; `Ok(None)` if its conditions held but
/// it had nothing to write (only checks, or deletes of absent keys); or an
/// error, [`Error::ConditionFailed`] among them.
pub type Outcome = Result<Option<u64>>;

/// The result of a write batch, once the writer has decided it.
///
/// Wait for it with [`Commit::wait`], or `.await` it: it is a [`Future`]
/// that needs no particular runtime, completed by yabiir's writer thread.
/// It resolves after the batch is in the log, published to readers, and,
/// with [`Options::should_sync_on_put`](crate::Options::should_sync_on_put),
/// durable. Dropping it doesn't cancel the batch.
#[must_use = "a batch's conditions can fail: wait for its Commit"]
pub struct Commit(Inner);

enum Inner {
    /// Decided before it reached the writer: a batch that failed validation.
    Ready(Option<Outcome>),
    Pending(Arc<Slot>),
}

impl Commit {
    pub(crate) fn ready(outcome: Outcome) -> Self {
        Self(Inner::Ready(Some(outcome)))
    }

    /// A commit, and the handle the writer completes it through.
    pub(crate) fn pending() -> (Self, Completer) {
        let slot = Arc::new(Slot {
            state: Mutex::new(State {
                outcome: None,
                waker: None,
            }),
            decided: Condvar::new(),
            is_decided: AtomicBool::new(false),
        });
        (
            Self(Inner::Pending(Arc::clone(&slot))),
            Completer(Some(slot)),
        )
    }

    /// Block this thread until the batch is decided, and return how it
    /// ended.
    pub fn wait(self) -> Outcome {
        match self.0 {
            Inner::Ready(outcome) => outcome.expect("a Commit is waited on once"),
            Inner::Pending(slot) => {
                // A write without an fsync is decided within microseconds,
                // sooner than this thread could be parked and woken again:
                // check for a while before sleeping.
                let spin_until = Instant::now() + SPIN_BEFORE_PARKING;
                while !slot
                    .is_decided
                    .load(Ordering::Acquire)
                    && Instant::now() < spin_until
                {
                    std::hint::spin_loop();
                }
                let mut state = lock(&slot.state);
                loop {
                    if let Some(outcome) = state
                        .outcome
                        .take()
                    {
                        return outcome;
                    }
                    state = wait(&slot.decided, state);
                }
            }
        }
    }
}

impl Future for Commit {
    type Output = Outcome;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Outcome> {
        match &mut self
            .get_mut()
            .0
        {
            Inner::Ready(outcome) => Poll::Ready(
                outcome
                    .take()
                    .expect("a Commit is not polled after it is ready"),
            ),
            Inner::Pending(slot) => {
                let mut state = lock(&slot.state);
                match state
                    .outcome
                    .take()
                {
                    Some(outcome) => Poll::Ready(outcome),
                    None => {
                        state.waker = Some(
                            cx.waker()
                                .clone(),
                        );
                        Poll::Pending
                    }
                }
            }
        }
    }
}

struct Slot {
    state: Mutex<State>,
    decided: Condvar,
    /// Set once `state.outcome` is, for a waiter spinning without the lock.
    is_decided: AtomicBool,
}

/// How long [`Commit::wait`] spins before it sleeps, and the writer thread
/// before it waits for the next batch.
pub(crate) const SPIN_BEFORE_PARKING: Duration = Duration::from_micros(50);

struct State {
    outcome: Option<Outcome>,
    waker: Option<Waker>,
}

/// The writer's end of a [`Commit`]. Dropped without
/// [`complete`](Self::complete), as when the writer thread panics, it
/// completes the commit with an error, so nobody waits forever.
pub(crate) struct Completer(Option<Arc<Slot>>);

impl Completer {
    pub(crate) fn complete(mut self, outcome: Outcome) {
        if let Some(slot) = self
            .0
            .take()
        {
            deliver(&slot, outcome);
        }
    }
}

impl Drop for Completer {
    fn drop(&mut self) {
        if let Some(slot) = self
            .0
            .take()
        {
            deliver(
                &slot,
                Err(Error::Io(io::Error::other(
                    "the writer stopped before deciding this batch; it may or may not have been \
                     written",
                ))),
            );
        }
    }
}

fn deliver(slot: &Slot, outcome: Outcome) {
    // Not through `crate::sync::lock`: this can run while a panic unwinds,
    // and a second panic would abort.
    let waker = match slot
        .state
        .lock()
    {
        Ok(mut state) => {
            state.outcome = Some(outcome);
            slot.is_decided
                .store(true, Ordering::Release);
            state
                .waker
                .take()
        }
        Err(_) => None,
    };
    slot.decided
        .notify_all();
    if let Some(waker) = waker {
        waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Wake;

    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0
                .store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn wait_returns_what_another_thread_completes() {
        let (commit, completer) = Commit::pending();
        let t = std::thread::spawn(move || completer.complete(Ok(Some(7))));
        assert_eq!(
            commit
                .wait()
                .unwrap(),
            Some(7)
        );
        t.join()
            .unwrap();
    }

    #[test]
    fn polling_registers_a_waker_that_completion_wakes() {
        let (mut commit, completer) = Commit::pending();
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);
        assert!(
            Pin::new(&mut commit)
                .poll(&mut cx)
                .is_pending()
        );
        completer.complete(Ok(None));
        assert!(
            flag.0
                .load(Ordering::SeqCst)
        );
        match Pin::new(&mut commit).poll(&mut cx) {
            Poll::Ready(Ok(None)) => {}
            other => panic!("expected Ready(Ok(None)), got {other:?}"),
        }
    }

    #[test]
    fn a_dropped_completer_fails_the_commit() {
        let (commit, completer) = Commit::pending();
        drop(completer);
        assert!(matches!(commit.wait(), Err(Error::Io(_))));
    }

    #[test]
    fn a_ready_commit_needs_no_writer() {
        assert!(matches!(
            Commit::ready(Err(Error::EmptyKey)).wait(),
            Err(Error::EmptyKey)
        ));
    }

    #[test]
    fn conditions_compare_against_the_current_version() {
        assert!(Cond::Absent.holds(None));
        assert!(!Cond::Absent.holds(Some(1)));
        assert!(Cond::Exists.holds(Some(1)));
        assert!(!Cond::Exists.holds(None));
        assert!(Cond::Version(3).holds(Some(3)));
        assert!(!Cond::Version(3).holds(Some(4)));
        assert!(!Cond::Version(3).holds(None));
    }
}
