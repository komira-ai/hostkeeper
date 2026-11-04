//! The errors of the ports. Every one carries what failed and why, as text for the operator;
//! the observer turns each into an `unobserved` round (or, for an unreachable health URL of a
//! running unit, an unhealthy one), never into a state of the service.

/// A port could not make its observation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObserveError {
    /// The service manager could not be asked about `unit`, or its answer could not be read.
    #[error("the service manager cannot tell the state of {unit}: {why}")]
    Manager { unit: String, why: String },
    /// Process `pid` could not be read (its stat, or the file it runs).
    #[error("process {pid} cannot be read: {why}")]
    Process { pid: u32, why: String },
    /// The health URL was asked and did not answer over HTTP: refused, reset, timed out, or no
    /// status line.
    #[error("{url} did not answer: {why}")]
    Unreachable { url: String, why: String },
    /// The health probe could not be made at all.
    #[error("the probe of {url} could not be made: {why}")]
    Probe { url: String, why: String },
}

/// A command could not be run to its end.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// The argv was empty.
    #[error("there is no program to run")]
    Empty,
    /// The program could not be started, read or waited for.
    #[error("cannot run {program}: {why}")]
    Failed { program: String, why: String },
    /// The program did not finish within its deadline, and was killed.
    #[error("{program} did not finish within its deadline and was killed")]
    TimedOut { program: String },
}
