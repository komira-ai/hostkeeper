//! The automated-enrollment side of the coordinator's MDM server as a library
//! (docs/design/coordinator-mdm.md 4.2, task CM12c).
//!
//! - [`blob::verify`] checks the device-information blob a Mac's Setup Assistant posts to `/enroll`
//!   (a PKCS#7 `SignedData` made by the device's Apple-issued identity) against Apple's roots, and
//!   returns the serial and UDID it names. [`blob::claims`] reads the same two values without
//!   checking anything, for the coordinator's fail-closed path: a blob that cannot be verified is
//!   never trusted, and what it says is only what an operator is shown to confirm.
//! - [`profile::render`] makes the enrollment profile a Mac is answered with: the SCEP payload that
//!   carries the one-time challenge and the MDM payload that points at the server.
//! - [`roots::apple`] is the DER of Apple's two roots (Apple Root CA and Apple Root CA - G3).
//!
//! The crate is pure: bytes in, values out, the time passed in, no I/O. What it does not claim:
//! no real blob from a Mac has been seen (`docs/design/coordinator-mdm-cm12a.md`), so the chain it
//! accepts is the one RFC 5652 and RFC 5280 describe, with RSA (SHA-1, SHA-256, SHA-384 and SHA-512
//! digests) and ECDSA signatures (P-256 with SHA-256, P-384 with SHA-384), DER encodings only, and a signer named by
//! issuer and serial. A blob outside that is refused as not verified, which the coordinator answers
//! by waiting for an operator; it never lets it through.

mod asn1;
pub mod blob;
mod oids;
pub mod profile;
pub mod roots;

pub use blob::{Device, Error};
