//! How a hostkeeper daemon gets its host certificate and keeps it (docs/design/hostkeeper-design.md,
//! section 5.7, steps 1 to 4).
//!
//! A daemon with no certificate makes a P-256 key, writes it durably to the pki directory and only
//! then posts an enrollment request to the coordinator, whose CA it first checks against the
//! `ca_sha256` of its daemon config. It polls with a nonce signed by the key until an operator
//! approves the request, checks the certificate it receives and stores it as a numbered generation
//! under `/etc/updater/pki/`. It renews at half life, and a host that was offline past expiry
//! returns through a new request marked `renewal_of` with the same key.
//!
//! Everything outside the crate is a port, so tests touch nothing of the host:
//!
//! - [`CoordinatorEnroll`]: the coordinator's enrollment API (`wire.rs`), over gRPC with TLS in
//!   [`GrpcEnroll`] (`grpc.rs`);
//! - [`Store`]: the pki directory ([`PkiDir`] on disk, `store.rs`);
//! - [`ClaimsSource`]: what the host says about itself ([`LinuxClaims`], with the BMC's MAC over
//!   the [`Ipmi`] port; [`MacClaims`] over the [`Ioreg`] port; `claims.rs`);
//! - [`Entropy`]: random bytes for the key ([`OsEntropy`], `key.rs`);
//! - [`WallClock`]: the wall clock, read only for certificate validity ([`SystemWallClock`],
//!   `validity.rs`, the one file the wall-clock lint allows here);
//! - `hostkeeper_steps::Clock`: the monotonic clock the retries sleep on.
//!
//! [`Enroller::run_once`] is one step; [`Enroller::run_loop`] paces the steps.

mod claims;
mod enroller;
mod grpc;
mod key;
mod store;
mod validity;
mod verify;
mod wire;

pub use claims::{
    ClaimsError, ClaimsSource, Ioreg, Ipmi, LAN_CHANNELS, LAN_MAC_PARAMETER, LinuxClaims,
    MacClaims, MacIds, parse_ioreg, parse_lan_mac,
};
pub use enroller::{
    Enroller, Enrollment, Error, Identity, NOT_ENROLLED, Outcome, Ports, ProtocolError, RETRY_SECS,
    Settings,
};
pub use grpc::{GrpcEnroll, GrpcError};
pub use key::{Entropy, EntropyError, KEY_DRAWS, KeyError, OsEntropy};
pub use store::{
    CurrentPaths, Fault, FaultGate, KeyFrom, PkiDir, SCHEMA, Store, StoreError, StoreLock, Stored,
};
pub use validity::{Site, SystemWallClock, WallClock};
pub use verify::Refusal;
pub use wire::{
    Claims, CoordinatorEnroll, EnrollAnswer, EnrollRequest, EnrollState, HostId, Issued,
    MAX_HOST_ID, MAX_REASON, NONCE_MAX, NONCE_MIN, POLL_CONTEXT, Poll, PollAnswer, Reason,
    RenewAnswer, RenewRequest, RequestId, TransportError, poll_message,
};
