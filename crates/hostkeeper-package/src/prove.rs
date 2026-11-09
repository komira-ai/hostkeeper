//! Step 2 of section 5.5: prove that a staged archive is ours. Its digest was checked by the
//! fetch and is not computed again; this checks that an attestation for that digest names the
//! daemon config's trusted signer.
//!
//! The attestation bundle is downloaded from the release channel at the archive's URL with
//! `.sigstore.json` appended, through the same [`Fetcher`], into a second root-only staging file
//! (named `.fetch-*.json`: the verifier reads the extension),
//! at most 1 MiB, within `attestation_timeout_secs` (which bounds the whole proof, not the
//! fetch's `fetch_timeout_secs`). The bundle names a commit and a ref, which the verifier is
//! asked to confirm against the signed certificate.
//!
//! | what happened | outcome, class, reason |
//! |---|---|
//! | the bundle cannot be downloaded (no connection, a status other than 2xx, the deadline) | `Deferred`, `environment`, `attestation_unavailable` |
//! | the verifier cannot run, gives no verdict, or runs out of time (a verdict that arrives after the deadline is not taken) | `Deferred`, `environment`, `attestation_unavailable` |
//! | the bundle is over 1 MiB, is not a bundle, names no commit or ref, or does not list the archive's digest (the verifier is not asked) | `Failure`, `release`, `not_attested` |
//! | the verifier rejects the attestation | `Failure`, `release`, `not_attested` |
//! | the disk is full while the bundle is written | `Deferred`, `environment`, `no_space` |
//!
//! Every refusal removes the archive and the bundle. The bundle is removed on success too: what
//! it proved is kept in the [`VerifiedArchive`]. The attested repository is never compared with
//! the service (design Q12, option (a)), and no version is compared with anything (Q13).

use std::path::Path;

use hostkeeper_config::daemon::{Bounds, Sha256Hex, TrustedSigners};
use hostkeeper_steps::{Clock, Stamp};

use crate::bundle::{self, BUNDLE_SUFFIX, MAX_BUNDLE_BYTES};
use crate::failure::{ATTESTATION_UNAVAILABLE, NO_SPACE, NOT_ATTESTED, PackageFailure};
use crate::fetch::StagedArchive;
use crate::index::Platform;
use crate::ports::{
    Attest, CommitId, Fetcher, Request, SourceRef, Verifier, VerifyError, after, remaining_secs,
};
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

/// The package for the host's platform, whose size and SHA-256 are the release index's and which is attested by the trusted signer,
/// for the commit and on the ref named here. The file is removed when this is dropped.
///
/// It has no public constructor and is not `Clone`: [`prove`] is the only way to get one, so
/// code that takes a `VerifiedArchive` reads only bytes that passed both checks.
#[derive(Debug)]
pub struct VerifiedArchive {
    file: TempFile,
    platform: Platform,
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

    /// The platform the package is for: the host's own. The manifest step checks that the
    /// archive's manifest names it.
    pub fn platform(&self) -> Platform {
        self.platform
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
        request: Request::plain(&url),
        // `gh` reads the extension of the bundle file it is given to choose its parser.
        suffix: ".json",
        cap: MAX_BUNDLE_BYTES,
        deadline: deadline.clone(),
    };
    let Download { file: bundle, .. } = stage::download(&job, writer).map_err(bundle_failure)?;
    let StagedArchive {
        file,
        platform,
        sha256,
        size,
        ..
    } = staged;
    let subject = Subject {
        file,
        platform,
        sha256,
        size,
    };
    attest(env, subject, bundle.path(), &deadline)
}

/// An archive whose size and SHA-256 are what an order named, not yet attested.
pub(crate) struct Subject {
    pub file: TempFile,
    pub platform: Platform,
    pub sha256: Sha256Hex,
    pub size: u64,
}

/// Reads the bundle at `bundle`, which is already downloaded, and asks the verifier whether it
/// attests `subject`, all by `deadline`: the part of a proof that does not depend on where the
/// bundle came from.
pub(crate) fn attest(
    env: &ProveEnv<'_>,
    subject: Subject,
    bundle: &Path,
    deadline: &Stamp,
) -> Result<VerifiedArchive, PackageFailure> {
    let bytes = stage::read_back(bundle, MAX_BUNDLE_BYTES)
        .map_err(|_| PackageFailure::deferred(ATTESTATION_UNAVAILABLE))?;
    let facts = bundle::read(&bytes)
        .ok()
        .filter(|facts| facts.lists(subject.sha256.as_str()))
        .ok_or(PackageFailure::release(NOT_ATTESTED))?;
    // The budget is checked before the verifier is asked and again when it answers, so a proof
    // that ran out of time is never taken for one.
    in_time(deadline, env.clock)?;
    let request = Attest {
        archive: subject.file.path(),
        bundle,
        sha256: &subject.sha256,
        commit: &facts.commit,
        source_ref: &facts.source_ref,
        signers: env.signers,
        deadline,
    };
    let attested = env.verifier.verify(&request).map_err(verify_failure)?;
    in_time(deadline, env.clock)?;
    let Subject {
        file,
        platform,
        sha256,
        size,
    } = subject;
    Ok(VerifiedArchive {
        file,
        platform,
        sha256,
        size,
        commit: attested.commit,
        source_ref: attested.source_ref,
    })
}

/// Whether any of the proof's time is left: `attestation_unavailable` once the deadline has
/// passed, however the step in progress ended.
fn in_time(deadline: &Stamp, clock: &dyn Clock) -> Result<(), PackageFailure> {
    if remaining_secs(deadline, clock) == 0 {
        return Err(PackageFailure::deferred(ATTESTATION_UNAVAILABLE));
    }
    Ok(())
}

/// The refusal a fault of the bundle's download is.
fn bundle_failure(fault: Fault) -> PackageFailure {
    match fault {
        Fault::TooLarge => PackageFailure::release(NOT_ATTESTED),
        Fault::Storage(e) if stage::is_full(&e) => PackageFailure::deferred(NO_SPACE),
        Fault::Storage(_) | Fault::Deadline | Fault::Net | Fault::Unexplained => {
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
