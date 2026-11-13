//! The crate's one policy for poisoned locks.
//!
//! A lock is poisoned when a thread panicked while holding it, so whatever it
//! guards may be half-updated: a keydir that disagrees with the log, an
//! active file whose offset is wrong. Carrying on could return wrong data or
//! write a corrupt entry, so every lock in the crate is taken through these
//! helpers, which panic instead. The store on disk is still consistent;
//! reopening it recovers.

use std::sync::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

const POISONED: &str = "a thread panicked while holding this lock; its state can't be trusted";

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect(POISONED)
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .expect(POISONED)
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .expect(POISONED)
}

/// [`Condvar::wait`], with the same policy.
pub(crate) fn wait<'a, T>(cond_var: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    cond_var
        .wait(guard)
        .expect(POISONED)
}
