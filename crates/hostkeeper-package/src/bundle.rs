//! The attestation bundle (a Sigstore bundle in JSON), read for what the verifier is asked to
//! confirm, before anything has verified it:
//!
//! - the commit: the one `gitCommit` among the `resolvedDependencies` of the provenance in the
//!   DSSE payload (the package's own commit, as the attestation records it);
//! - the ref: the source repository ref (`1.3.6.1.4.1.57264.1.14`) of the signing certificate;
//! - the SHA-256 digests of the statement's subjects.
//!
//! None of these is believed: the verifier is asked to confirm the commit and the ref against
//! the signed certificate, and the digest against the archive, so a bundle that lies about any
//! of them fails verification. The bundle is at most [`MAX_BUNDLE_BYTES`] and is parsed only with
//! `serde_json` and the DER reader of [`crate::der`].

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::der;
use crate::ports::{CommitId, SourceRef};

/// The most bytes of a bundle.
pub(crate) const MAX_BUNDLE_BYTES: u64 = 1 << 20;
/// Where a package's bundle is: the archive's URL with this appended. One constant, so another
/// place is one change.
pub(crate) const BUNDLE_SUFFIX: &str = ".sigstore.json";

/// Where a certificate may be in a bundle: `certificate` (bundle v0.3) or the first of
/// `x509CertificateChain` (v0.1 and v0.2).
const CERTIFICATE_AT: [&str; 2] = [
    "/verificationMaterial/certificate/rawBytes",
    "/verificationMaterial/x509CertificateChain/certificates/0/rawBytes",
];

/// What a bundle says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    pub commit: CommitId,
    pub source_ref: SourceRef,
    /// The SHA-256 of each subject, lower-case.
    pub subjects: Vec<String>,
}

impl Facts {
    /// Whether the statement lists `sha256` as a subject's digest.
    pub(crate) fn lists(&self, sha256: &str) -> bool {
        self.subjects.iter().any(|subject| subject == sha256)
    }
}

/// Reads `bytes` as a bundle, or says what it lacks.
pub(crate) fn read(bytes: &[u8]) -> Result<Facts, &'static str> {
    let bundle: Value = serde_json::from_slice(bytes).map_err(|_| "the bundle is not JSON")?;
    let payload = bundle
        .pointer("/dsseEnvelope/payload")
        .and_then(Value::as_str)
        .ok_or("the bundle has no DSSE payload")?;
    let statement: Value = STANDARD
        .decode(payload)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .ok_or("the DSSE payload is not a JSON statement in base64")?;
    let subjects: Vec<String> = statement
        .get("subject")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|subject| subject.pointer("/digest/sha256")?.as_str())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let commits: BTreeSet<&str> = statement
        .pointer("/predicate/buildDefinition/resolvedDependencies")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|dependency| dependency.pointer("/digest/gitCommit")?.as_str())
                .collect()
        })
        .unwrap_or_default();
    let commit = (commits.len() == 1)
        .then(|| commits.first().copied())
        .flatten()
        .and_then(CommitId::new)
        .ok_or("the provenance does not name exactly one commit of 40 or 64 hex digits")?;
    let certificate = CERTIFICATE_AT
        .iter()
        .find_map(|at| bundle.pointer(at)?.as_str())
        .and_then(|text| STANDARD.decode(text).ok())
        .ok_or("the bundle has no certificate in base64")?;
    let source_ref = der::utf8_extension(&certificate, &der::fulcio(der::SOURCE_REPOSITORY_REF))
        .and_then(SourceRef::new)
        .ok_or("the certificate names no source ref")?;
    Ok(Facts {
        commit,
        source_ref,
        subjects,
    })
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
