//! One asset's attestation, checked as the daemon checks a package's
//! (docs/design/hostkeeper-design.md, section 5.5, step 2): its bundle is read for the commit and
//! the ref it names ([`hostkeeper_package::bundle::read`]), must list the asset's SHA-256, and is
//! then handed with the asset to the [`Verifier`], which confirms against the signed certificate
//! that the trusted signer made it. The whole check is bounded by `attestation_timeout_secs` on
//! the injected clock, checked before the verifier is asked and again when it answers, so a
//! verdict that came too late is never taken.

use std::path::Path;

use hostkeeper_config::daemon::{Sha256Hex, TrustedSigners};
use hostkeeper_package::bundle::{self, BundleInvalid};
use hostkeeper_package::ports::{Attest, Attested, Verifier, VerifyError, remaining_secs};
use hostkeeper_steps::{Clock, Stamp};

/// What the check needs: the verifier, the clock, the trusted signer and the time it may take.
#[derive(Clone, Copy)]
pub(crate) struct Checker<'a> {
    pub verifier: &'a dyn Verifier,
    pub clock: &'a dyn Clock,
    pub signers: &'a TrustedSigners,
    pub timeout_secs: u64,
}

/// Why an asset is not taken as attested.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttestError {
    /// The bundle is not a bundle, or names no single commit or no ref.
    #[error("not attested: its bundle is unreadable: {0}")]
    Bundle(BundleInvalid),
    /// The bundle attests other bytes: it does not list the asset's SHA-256.
    #[error("not attested: its bundle does not list its SHA-256 {0}")]
    NotListed(String),
    /// The verifier's answer: rejected (another organisation, workflow or ref, a self-hosted
    /// runner), unavailable, or out of time.
    #[error("{0}")]
    Verify(VerifyError),
}

/// Checks that `asset`, whose SHA-256 is `sha256`, is attested by `bundle` (its bytes and its
/// file) for the trusted signer.
pub(crate) fn attest(
    checker: &Checker<'_>,
    asset: &Path,
    sha256: &str,
    bundle_file: &Path,
    bundle_bytes: &[u8],
) -> Result<Attested, AttestError> {
    let start = Stamp::now(checker.clock);
    let deadline = Stamp {
        boot_id: start.boot_id.clone(),
        mono_secs: start.mono_secs.saturating_add(checker.timeout_secs),
    };
    let facts = bundle::read(bundle_bytes).map_err(AttestError::Bundle)?;
    let digest = Sha256Hex::parse(sha256)
        .filter(|digest| facts.lists(digest.as_str()))
        .ok_or_else(|| AttestError::NotListed(sha256.to_owned()))?;
    in_time(&deadline, checker.clock)?;
    let request = Attest {
        archive: asset,
        bundle: bundle_file,
        sha256: &digest,
        commit: &facts.commit,
        source_ref: &facts.source_ref,
        signers: checker.signers,
        deadline: &deadline,
    };
    let attested = checker
        .verifier
        .verify(&request)
        .map_err(AttestError::Verify)?;
    in_time(&deadline, checker.clock)?;
    Ok(attested)
}

/// [`VerifyError::TimedOut`] once the deadline has passed.
fn in_time(deadline: &Stamp, clock: &dyn Clock) -> Result<(), AttestError> {
    match remaining_secs(deadline, clock) {
        0 => Err(AttestError::Verify(VerifyError::TimedOut)),
        _ => Ok(()),
    }
}
