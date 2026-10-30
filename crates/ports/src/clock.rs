//! Time as a port: the core reads the time only through this trait, so a run can be replayed
//! on a simulated clock with the same results.

/// A source of the current time.
pub trait Clock {
    /// The current time, in milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}
