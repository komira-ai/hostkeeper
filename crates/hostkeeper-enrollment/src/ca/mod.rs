//! The certificate authority: a module of the coordinator, not a separate process
//! (docs/design/hostkeeper-design.md, section 4.4 and open question Q5, option (a)).
//!
//! - [`profile`]: what a certificate request must be ([`check_request`]) and the one key type
//!   the CA signs ([`P256Key`]).
//! - [`issue`]: [`Ca`], which signs a host's client certificate and the coordinator's server
//!   certificate under the profile, and the [`Signer`] seam.
//! - [`key`]: the CA's key file (PKCS#8, owner-only) and certificate file: [`Ca::init`] and
//!   [`Ca::open`].
//! - [`bundle`]: the CA certificate with its SHA-256, the value a daemon config pins, and the
//!   bundle a poll returns.

pub mod bundle;
pub mod issue;
pub mod key;
pub mod profile;

pub use bundle::{CaBundle, HostCertificate, PollBundle};
pub use issue::{
    CA_VALIDITY_SECS, Ca, Certificate, HOST_VALIDITY_SECS, SERVER_VALIDITY_CAP_SECS, SignError,
    SignRequest, Signer, Subject,
};
pub use key::{CaError, CaFiles};
pub use profile::{
    CsrDefect, MAX_CSR_BYTES, P256Key, ProfileError, VerifiedRequest, check_request,
};
