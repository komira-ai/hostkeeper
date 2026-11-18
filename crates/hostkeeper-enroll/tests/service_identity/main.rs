//! The keeper of service identities against a coordinator that signs from a test CA (or answers as
//! a test says), the real identity directory in a scratch directory owned by the test's own user,
//! an injected wall clock and an injected monotonic clock. No test sleeps, reads the real time or
//! opens a socket.
//!
//! - `install.rs`: what a service with an identity gets and when it is asked again;
//! - `refusals.rs`: each check a certificate must pass, and the coordinator's refusals;
//! - `pacing.rs`: the retry after a failure and the loop's pace;
//! - `directory.rs`: the identity directory's ownership, modes, links and removal;
//! - `accounts.rs`: a service whose account changed;
//! - `sweep.rs`: the directories of services that are not installed;
//! - `trust.rs`: the host's id and CA bundle read from the pki directory.

mod accounts;
mod directory;
mod install;
mod pacing;
mod refusals;
mod support;
mod sweep;
mod trust;
