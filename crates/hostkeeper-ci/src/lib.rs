//! `hostkeeper ci set-desired`: the part of a service's CI that runs after its release job has
//! verified the hostkeeper release, in the revisions of `set-desired.yml` before it called
//! `hkctl`; a caller pinned to one of them keeps running it (docs/deploy/ci-client.md).
//!
//! The order is the point. The release is verified first (by the workflow, before this runs),
//! so that no OIDC token is ever asked for on behalf of a binary that was not checked. Then
//! this crate, for each call it makes, requests a NEW token (the coordinator accepts each token
//! once), masks it in the runner's log, keeps it only in a file created fresh with mode 0600
//! under the runner's temp directory, runs `hostkeeper ctl` in-process on that file, and removes
//! the file whatever the call returned: the token is never an argument or an environment
//! variable of any process. The calls are `releases set-desired` (naming the release by the one
//! package digest, or by the release index file, read whole before any token, and its digest, both
//! checked by the coordinator and not by this client), and, unless told not to wait,
//! `releases converge` read at a fixed pause up to a bound, until the fleet has converged on the
//! version, is halted, or has been given another version.
//!
//! The job's outputs (`desired_version`, `converged`, `reason`) are appended to the file
//! `--github-output` names. The exit status is 0 when the fleet converged or was not waited for,
//! 1 when the flow failed, and 2 for bad flags, in which case no token was requested and no
//! output written.
//!
//! The flow works through a small set of ports (a token source, the ctl transports, a pause and
//! stdout lines), so that the tests drive all of it with fakes through the real `hostkeeper
//! ctl`; the real ports are the OIDC endpoint, the network transports, the thread's sleep and
//! stdout.

mod call;
mod flags;
mod flow;
mod observe;
mod ports;
mod token;

#[cfg(test)]
mod flags_tests;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod lib_tests;
#[cfg(test)]
mod observe_tests;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod token_tests;

use std::process::ExitCode;

pub use flags::SetDesiredFlags;

/// What `hostkeeper ci` can run.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum CiCommand {
    /// Set a service's desired version on the coordinator and wait for the fleet to converge on
    /// it, authenticated by a GitHub Actions OIDC token.
    SetDesired(SetDesiredFlags),
}

/// Runs a CI command to its exit status: 0 done (converged, or not waited for), 1 failed, 2 a
/// usage error (bad flags), before any token is requested.
pub fn run(command: CiCommand) -> ExitCode {
    ExitCode::from(status(command))
}

/// The exit status of [`run`] as a number.
fn status(command: CiCommand) -> u8 {
    match command {
        CiCommand::SetDesired(flags) => {
            flow::set_desired(&flags, &mut std::io::stderr(), ports::NetworkPorts::new)
        }
    }
}
