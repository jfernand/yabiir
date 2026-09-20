//! Pluggable observability hook for [`crate::Engine`]'s core operations —
//! `docs/ROADMAP.md` §4's metrics item. The crate stays backend-agnostic
//! (no forced dependency on any particular metrics library): a caller
//! wires in whatever collector it wants via [`crate::Options::metrics`],
//! and if it doesn't, nothing is recorded at all.

use std::time::Duration;

/// Implement this to observe [`crate::Engine`]'s `put`/`get`/`delete`/
/// `merge`/`sync` calls — durations of successful calls only (an error
/// part-way through doesn't have a meaningful "how long did the operation
/// take" to report). Every method has a default no-op body, so an
/// implementation only needs to override the operations it actually cares
/// about.
pub trait Metrics: Send + Sync {
    fn record_put(&self, _duration: Duration) {}
    fn record_get(&self, _duration: Duration) {}
    fn record_delete(&self, _duration: Duration) {}
    fn record_merge(&self, _duration: Duration) {}
    fn record_sync(&self, _duration: Duration) {}
}

/// The default when [`crate::Options::metrics`] is `None` — every method
/// keeps [`Metrics`]'s empty default body, so this costs nothing beyond an
/// (easily inlined) vtable call at each call site.
pub(crate) struct NoopMetrics;

impl Metrics for NoopMetrics {}
