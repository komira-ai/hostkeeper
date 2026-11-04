//! Step 2 of section 5.5: prove that a staged archive is ours. Its digest was checked by the
//! fetch and is not computed again; this checks that an attestation for that digest names the
//! daemon config's trusted signer.
//!
//! The attestation bundle is downloaded from the release channel at the archive's URL with
//! `.sigstore.json` appended, through the same [`Fetcher`], into a second root-only staging file,
//! at most 1 MiB, within `attestation_timeout_secs` (which bounds the whole proof, not the
//! fetch's `fetch_timeout_secs`). The bundle names a commit and a ref, which the verifier is
//! asked to confirm against the signed certificate.
//!
//! | what happened | outcome, class, reason |
//! |---|---|
//! | the bundle cannot be downloaded (no connection, a status other than 2xx, the deadline) | `Deferred`, `environment`, `attestation_unavailable` |
//! | the verifier cannot run, gives no verdict, or runs out of time | `Deferred`, `environment`, `attestation_unavailable` |
//! | the bundle is over 1 MiB, is not a bundle, names no commit or ref, or does not list the archive's digest (the verifier is not asked) | `Failure`, `release`, `not_attested` |
//! | the verifier rejects the attestation | `Failure`, `release`, `not_attested` |
//! | the disk is full while the bundle is written | `Deferred`, `environment`, `no_space` |
//!
//! Every refusal removes the archive and the bundle. The bundle is removed on success too: what
//! it proved is kept in the [`VerifiedArchive`]. The attested repository is never compared with
//! the service (design Q12, option (a)), and no version is compared with anything (Q13).

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use hostkeeper_config::daemon::{Bounds, Sha256Hex, TrustedSigners};
use hostkeeper_steps::{Clock, Stamp};

use crate::bundle::{self, BUNDLE_SUFFIX, MAX_BUNDLE_BYTES};
use crate::failure::{ATTESTATION_UNAVAILABLE, NO_SPACE, NOT_ATTESTED, PackageFailure};
use crate::fetch::StagedArchive;
use crate::ports::{Attest, CommitId, Fetcher, SourceRef, Verifier, VerifyError, after};
use crate::stage::{self, Download, Fault, Job, OpenWriter, TempFile};

/// What a proof needs from the daemon: the network, the verifier, the clock, and the config's
/// bounds and trusted signer.
#[derive(Clone, Copy)]
pub struct ProveEnv<'a> {
    pub fetcher: &'a dyn Fetcher,
    pub verifier: &'a dyn Verifier,
    pub clock: &'a dyn Clock,
    pub bounds: &'a Bounds,
    pub signers: &'a TrustedSigners,
}

/// An archive whose bytes hash to the order's digest and are attested by the trusted signer,
/// for the commit and on the ref named here. The file is removed when this is dropped.
///
/// It has no public constructor and is not `Clone`: [`prove`] is the only way to get one, so
/// code that takes a `VerifiedArchive` reads only bytes that passed both checks.
#[derive(Debug)]
pub struct VerifiedArchive {
    file: TempFile,
    sha256: Sha256Hex,
    size: u64,
    commit: CommitId,
    source_ref: SourceRef,
}

impl VerifiedArchive {
    /// The staging file that holds the archive.
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn sha256(&self) -> &Sha256Hex {
        &self.sha256
    }

    /// The archive's size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The commit the attestation was made for.
    pub fn commit(&self) -> &CommitId {
        &self.commit
    }

    /// The ref the attestation was made on.
    pub fn source_ref(&self) -> &SourceRef {
        &self.source_ref
    }
}

/// Proves `staged` (see the module's table for every refusal). The deadline is
/// `attestation_timeout_secs` from the start of this call on the injected clock.
pub fn prove(env: &ProveEnv<'_>, staged: StagedArchive) -> Result<VerifiedArchive, PackageFailure> {
    prove_with(env, staged, stage::plain)
}

/// [`prove`], writing the bundle's staging file through `writer`.
pub(crate) fn prove_with(
    env: &ProveEnv<'_>,
    staged: StagedArchive,
    writer: OpenWriter,
) -> Result<VerifiedArchive, PackageFailure> {
    let start = Stamp::now(env.clock);
    let deadline = after(&start, env.bounds.attestation_timeout_secs);
    let url = staged.url.with_suffix(BUNDLE_SUFFIX);
    let job = Job {
        fetcher: env.fetcher,
        clock: env.clock,
        dir: &staged.dir,
        url: &url,
        cap: MAX_BUNDLE_BYTES,
        deadline: deadline.clone(),
    };
    let Download { file: bundle, .. } = stage::download(&job, writer).map_err(bundle_failure)?;
    let bytes = read_bundle(bundle.path())
        .map_err(|_| PackageFailure::deferred(ATTESTATION_UNAVAILABLE))?;
    let facts = bundle::read(&bytes)
        .ok()
        .filter(|facts| facts.lists(staged.sha256.as_str()))
        .ok_or(PackageFailure::release(NOT_ATTESTED))?;
    let request = Attest {
        archive: staged.file.path(),
        bundle: bundle.path(),
        sha256: &staged.sha256,
        commit: &facts.commit,
        source_ref: &facts.source_ref,
        signers: env.signers,
        deadline: &deadline,
    };
    let attested = env.verifier.verify(&request).map_err(verify_failure)?;
    let StagedArchive {
        file, sha256, size, ..
    } = staged;
    Ok(VerifiedArchive {
        file,
        sha256,
        size,
        commit: attested.commit,
        source_ref: attested.source_ref,
    })
}

/// The bundle's bytes, read without following a link; the download already held it to
/// [`MAX_BUNDLE_BYTES`].
fn read_bundle(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .take(MAX_BUNDLE_BYTES)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The refusal a fault of the bundle's download is.
fn bundle_failure(fault: Fault) -> PackageFailure {
    match fault {
        Fault::TooLarge => PackageFailure::release(NOT_ATTESTED),
        Fault::Storage(e) if stage::is_full(&e) => PackageFailure::deferred(NO_SPACE),
        Fault::Storage(_) | Fault::Deadline | Fault::Net(_) | Fault::Unexplained => {
            PackageFailure::deferred(ATTESTATION_UNAVAILABLE)
        }
    }
}

/// The refusal a verifier's answer is. A verifier that is down or slow says nothing about the
/// package, so it defers; only a verdict fails the release.
fn verify_failure(error: VerifyError) -> PackageFailure {
    match error {
        VerifyError::Unavailable | VerifyError::TimedOut => {
            PackageFailure::deferred(ATTESTATION_UNAVAILABLE)
        }
        VerifyError::Rejected(_) => PackageFailure::release(NOT_ATTESTED),
    }
}

#[cfg(test)]
#[path = "prove_tests.rs"]
mod tests;
