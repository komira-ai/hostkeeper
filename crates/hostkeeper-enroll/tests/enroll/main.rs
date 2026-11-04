//! The enroller against an in-memory coordinator, a fake wall clock, a fake monotonic clock and a
//! pki directory in a scratch directory. No test sleeps, reads the real time or opens a socket.
//!
//! - `acceptance.rs`: the behaviours of design section 5.7, steps 1 to 4;
//! - `refusals.rs`: each check an issued certificate must pass, and each way the coordinator
//!   can break the protocol;
//! - `store.rs`: the pki directory: lock, schema, crash recovery and pruning;
//! - `host.rs`: the key, the entropy and the Linux claims over temporary trees;
//! - `pacing.rs`: the loop's log and sleeps.

mod acceptance;
mod host;
mod pacing;
mod refusals;
mod store;
mod support;
