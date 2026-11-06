//! `hostkeeper update-hook <step> [<staged release>] [--params <file>]`: the hook of hostkeeper's
//! own package, the command its manifest names (`bin/hostkeeper update-hook`).
//!
//! A daemon runs the hook of the release it installs or updates to, so this is the hook of the
//! release ordered, run by the daemon that runs now, which may be older: every daemon runs it for
//! an `install` and an `update` of the service `hostkeeper`. hostkeeper replaces itself by the
//! daemon's self-update follow-on (design section 6.6, [`crate::selfupdate`]), not by its hook, so
//! every step does nothing: it exits 0 and prints nothing, which the contract reads as a
//! success that can be rolled back (design section 7). A command line the contract does not
//! allow exits 1 with the reason on standard error, a failure, so that a daemon that calls it
//! otherwise than the contract says stops before it changes anything.

use std::ffi::OsString;
use std::process::ExitCode;

use hostkeeper_hook::{ArgsError, Invocation, Outcome};

/// How the hook ends for the arguments after `update-hook`: [`Outcome::Success`] for a step the
/// contract allows, whatever the step, and the reason the command line is refused otherwise.
pub fn run<I>(args: I) -> Result<Outcome, ArgsError>
where
    I: IntoIterator<Item = OsString>,
{
    Invocation::parse(args).map(|_| Outcome::Success)
}

/// The verb: [`run`] as an exit status, a refused command line said on standard error and ended
/// [`Outcome::Failure`].
pub fn main(args: Vec<OsString>) -> ExitCode {
    let outcome = run(args).unwrap_or_else(|error| {
        eprintln!("hostkeeper update-hook: {error}");
        Outcome::Failure
    });
    outcome.into()
}
