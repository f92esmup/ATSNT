//! Composition of the concrete adapters for the binaries.
//!
//! The library names no adapter (see the architecture contract): the binaries take the clock and
//! the recorder from here and pass them down as ports.

use adapters::{NullRunRecorder, SystemClock};

/// The clock of the binaries: the machine's wall clock.
pub fn system_clock() -> SystemClock {
    SystemClock
}

/// The recorder of runs whose analytics are not persisted. No storage is wired in yet, so
/// long-running binaries keep nothing and their memory does not grow with the run.
pub fn disabled_recorder() -> NullRunRecorder {
    NullRunRecorder
}
