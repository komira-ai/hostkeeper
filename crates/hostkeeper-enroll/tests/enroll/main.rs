//! The enroller against an in-memory coordinator, a fake wall clock, a fake monotonic clock and a
//! pki directory in a scratch directory. No test sleeps, reads the real time or opens a socket.
//!
//! - `acceptance.rs`: the behaviours of design section 5.7, steps 1 to 4;
//! - `returns.rs`: repeated returns after expiry, approvals given while the daemon was down,
//!   lost replies, changed claims, and the bytes a poll signs;
//! - `faults.rs`: a failed fsync or a crash at each point of the store's writes;
//! - `refusals.rs`: each check an issued certificate must pass, and each way the coordinator
//!   can break the protocol;
//! - `store.rs`: the pki directory: lock, schema, ownership, crash recovery and pruning;
//! - `host.rs`: the key, the entropy and the Linux claims over temporary trees;
//! - `mac.rs`: the claims of a Mac, read from `ioreg` output, and a request refused without them;
//! - `pacing.rs`: the loop's log and sleeps.

mod acceptance;
mod faults;
mod host;
mod mac;
mod pacing;
mod refusals;
mod returns;
mod store;
mod support;
