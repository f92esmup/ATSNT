//! Progress of a long computation, reported to whoever watches it (a terminal, a log, nothing).

/// Receives the progress of a computation made of `total` equal steps.
///
/// Steps are reported from worker threads, so a sink is shared between them.
pub trait ProgressSink: Send + Sync {
    /// The computation starts and has `total` steps.
    fn start(&self, total: u64);
    /// One more step is done.
    fn advance(&self);
    /// Every step is done.
    fn finish(&self);
}
