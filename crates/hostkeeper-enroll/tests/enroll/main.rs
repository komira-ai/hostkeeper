//! The enroller against an in-memory coordinator (in `coordinator.rs`, the coordinator's own
//! enrollment and CA), a fake wall clock, a fake monotonic clock and a pki directory in a
//! scratch directory. No test sleeps, reads the real time or opens a socket.
//!
//! - `acceptance.rs`: the behaviours of design section 5.7, steps 1 to 4;
//! - `returns.rs`: repeated returns after expiry, approvals given while the daemon was down,
//!   lost replies, changed claims, and the bytes a poll signs;
//! - `coordinator.rs`: the requests the enroller makes, held to the CA's profile and posted to
//!   the coordinator's own enrollment and CA, through approval, the first certificate and its
//!   renewal;
//! - `faults.rs`: a failed fsync or a crash at each point of the store's writes;
//! - `refusals.rs`: each check an issued certificate must pass, and each way the coordinator
//!   can break the protocol;
//! - `store.rs`: the pki directory: lock, schema, ownership, crash recovery and pruning;
//! - `host.rs`: the key, the entropy and the Linux claims over temporary trees;
//! - `bmc.rs`: the BMC MAC claim of a Linux host, read through the IPMI port;
//! - `mac.rs`: the claims of a Mac, read from `ioreg` output, and a request refused without them;
//! - `pacing.rs`: the loop's log and sleeps, and the enrollment it reports after each step;
//! - `enrollment.rs`: the host's enrollment as the enroller reads it from the pki directory.
//! - `renewal_state.rs`: what the loop notes after each step for the heartbeat: the end of the
//!   current certificate and the step's error.

mod acceptance;
mod bmc;
mod coordinator;
mod enrollment;
mod faults;
mod host;
mod mac;
mod pacing;
mod refusals;
mod renewal_state;
mod returns;
mod store;
mod support;
