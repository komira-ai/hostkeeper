//! Enrollment, as far as it is a certificate or a file (docs/design/hostkeeper-design.md,
//! sections 4.4, 5.7, 6.1 and 8.8).
//!
//! - [`ca`]: the certificate authority, a module of the coordinator. [`ca::check_request`]
//!   holds a CSR to the profile (ECDSA P-256, subject `host:<id>`); [`ca::Ca`] signs a host's
//!   client certificate (7 days, `clientAuth`) and the coordinator's server certificate
//!   (`serverAuth`, capped), loads or creates its owner-only key file, and gives the CA
//!   certificate with its SHA-256 ([`ca::CaBundle`]). [`ca::Signer`] is the seam a separate
//!   signer process would implement.
//! - [`enroll`]: the durable enrollment store: pending requests, approval, one certificate per
//!   request id and round, duplicate claims, live-daemon duplicates and deny rows.
//! - [`validity`]: the [`validity::UnixClock`] port, the only place the crate reads the wall
//!   clock.
//! - [`fs`]: the [`fs::Fs`] port the store and the key file write through, with a real
//!   implementation and [`fs::FaultFs`], which fails a chosen operation.
//! - [`names`]: host ids, roles, service names and key hashes.
//!
//! Errors name the refusal token the coordinator answers with (`token()`), where there is one.
//! No RPC is served and no wire message is used here; the store's own messages
//! (`proto/hostkeeper/enrollment/store/v1/store.proto`) are private to the crate. The crate
//! depends on no crate of this workspace.

pub mod ca;
pub mod enroll;
pub mod fs;
pub mod names;
pub mod validity;
