//! The real package store: the four steps of design section 5.5 behind the [`Packages`] port.
//!
//! [`RealPackages::version_dir`] sweeps what a crashed fetch or unpack left, downloads the
//! package of the order ([`hostkeeper_package::fetch_package`], which refuses a release index
//! whose SHA-256 is not the order's index digest, when the order has one), proves it
//! ([`hostkeeper_package::prove`]), compares the proven archive's SHA-256 with the digest the
//! order names, then unpacks it ([`hostkeeper_unpack::unpack`]) and seals the version directory.
//! It takes no lock: the daemon holds the service's lock around the call.
//!
//! Every failure of those libraries reaches the daemon as one [`PackageError::Refused`] built by
//! [`refusal_of`], the one table that maps a library's reason token onto a [`Refusal`]. The
//! outcome and the class are the library's own.
//!
//! The store also reports the Sigstore trusted root its verifier checks attestations with
//! ([`RealPackages::with_trusted_root`], [`crate::trusted_root`]): its age and, when it needs
//! attention, a reason, which the admin `Status` carries.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::{Bounds, DaemonConfig, HttpsUrl, Sha256Hex, TrustedSigners};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_package::fetch::FetchEnv;
use hostkeeper_package::ports::{Fetcher, Verifier};
use hostkeeper_package::prove::ProveEnv;
use hostkeeper_package::{
    ATTESTATION_UNAVAILABLE, FETCH_FAILED, INDEX_DIGEST_MISMATCH, INDEX_INVALID,
    NO_PACKAGE_FOR_PLATFORM, NO_SPACE, NOT_ATTESTED, PACKAGE_DIGEST_MISMATCH, PackageFailure,
    PackageOrder, Platform, fetch_package, prove, remove_orphan_fetches,
};
use hostkeeper_steps::{Clock, Staged};
use hostkeeper_unpack::{
    HostFs, Limits, Owner, PackageFs, Platform as UnpackPlatform, REASON_ARTIFACT_CHANGED,
    REASON_BAD_ARCHIVE, REASON_DAEMON_TOO_OLD, REASON_DIGEST_MISMATCH, REASON_IO_ERROR,
    REASON_MANIFEST_MISMATCH, REASON_NO_SPACE, REASON_RESERVED_VERSION, REASON_VERIFY_UNAVAILABLE,
    REASON_VERSION_TOO_LONG, UnpackError, UnpackOrder,
};

use crate::adapters::PackageCheck;
use crate::packages::{PackageError, PackageRefusal, Packages, VersionDir};
use crate::refusal::Refusal;
use crate::trusted_root::{RootReport, TrustedRoot};
use crate::verb::PackageDigest;

/// What failed, as [`refusal_of`] takes it.
#[derive(Clone, Copy, Debug)]
pub enum Failure<'a> {
    /// A download or a proof was refused.
    Package(PackageFailure),
    /// The unpack was refused.
    Unpack(&'a UnpackError),
    /// The proven archive has a SHA-256 other than the order's.
    OrderDigest { expected: &'a str, found: &'a str },
}

/// The library token of a fetch or a proof, and the refusal it is reported as.
const PACKAGE_TOKENS: &[(&str, Refusal)] = &[
    (INDEX_DIGEST_MISMATCH, Refusal::IndexDigestMismatch),
    (INDEX_INVALID, Refusal::IndexInvalid),
    (NO_PACKAGE_FOR_PLATFORM, Refusal::NoPackageForPlatform),
    (PACKAGE_DIGEST_MISMATCH, Refusal::DigestMismatch),
    (FETCH_FAILED, Refusal::FetchFailed),
    (ATTESTATION_UNAVAILABLE, Refusal::AttestationUnavailable),
    (NO_SPACE, Refusal::NoSpace),
    (NOT_ATTESTED, Refusal::NotAttested),
];

/// The reason token of an unpack error, and the refusal it is reported as.
const UNPACK_TOKENS: &[(&str, Refusal)] = &[
    (REASON_BAD_ARCHIVE, Refusal::BadArchive),
    (REASON_DIGEST_MISMATCH, Refusal::DigestMismatch),
    (REASON_MANIFEST_MISMATCH, Refusal::ManifestMismatch),
    (REASON_DAEMON_TOO_OLD, Refusal::DaemonTooOld),
    (REASON_NO_SPACE, Refusal::NoSpace),
    (REASON_ARTIFACT_CHANGED, Refusal::ArtifactChanged),
    (REASON_RESERVED_VERSION, Refusal::ReservedVersion),
    (REASON_VERSION_TOO_LONG, Refusal::VersionTooLong),
    (REASON_VERIFY_UNAVAILABLE, Refusal::VerifyUnavailable),
    (REASON_IO_ERROR, Refusal::PackageIoError),
];

/// The refusal `token` is in `table`; `package_failed` for a token a library added since.
fn lookup(table: &[(&str, Refusal)], token: &str) -> Refusal {
    table
        .iter()
        .find(|(known, _)| *known == token)
        .map_or(Refusal::PackageFailed, |(_, refusal)| *refusal)
}

/// The one mapping of a failure of the package libraries onto the daemon's closed set: the
/// refusal from the library's reason token, the outcome and the class from the library. An
/// archive whose SHA-256 is not the order's is `digest_mismatch`, `Failure`, `Release`.
pub fn refusal_of(failure: Failure<'_>) -> PackageRefusal {
    let (table, outcome, class, token, detail) = match failure {
        Failure::Package(failed) => (
            PACKAGE_TOKENS,
            failed.outcome,
            failed.class,
            failed.reason,
            failed.to_string(),
        ),
        Failure::Unpack(error) => {
            let (outcome, class, token) = error.outcome();
            (UNPACK_TOKENS, outcome, class, token, error.to_string())
        }
        Failure::OrderDigest { expected, found } => {
            return PackageRefusal {
                refusal: Refusal::DigestMismatch,
                outcome: Outcome::Failure,
                class: Class::Release,
                detail: format!("the package's SHA-256 is {found}, not the order's {expected}"),
            };
        }
    };
    PackageRefusal {
        refusal: lookup(table, token),
        outcome,
        class,
        detail,
    }
}

/// A failure of the host's disk that no library classified: tried again later.
fn io_refusal(detail: String) -> PackageRefusal {
    PackageRefusal {
        refusal: Refusal::PackageIoError,
        outcome: Outcome::Deferred,
        class: Class::Environment,
        detail,
    }
}

fn refused(refusal: PackageRefusal) -> PackageError {
    PackageError::Refused(refusal)
}

/// Fetches, proves, unpacks and seals packages with the real libraries.
pub struct RealPackages {
    fetcher: Box<dyn Fetcher + Send + Sync>,
    verifier: Box<dyn Verifier + Send + Sync>,
    clock: Arc<dyn Clock>,
    bounds: Bounds,
    release_channel: HttpsUrl,
    signers: TrustedSigners,
    root: PathBuf,
    platform: Platform,
    fs: Arc<dyn PackageFs + Send + Sync>,
    check: PackageCheck,
    trusted_root: Option<Arc<TrustedRoot>>,
}

impl RealPackages {
    /// A store over the packages directory `root`, which exists, with the release channel, the
    /// trusted signer and the bounds of `config`, for the host's platform. Everything it
    /// unpacks is given to `owner`, which is [`Owner::ROOT`] in the daemon.
    pub fn open(
        fetcher: Box<dyn Fetcher + Send + Sync>,
        verifier: Box<dyn Verifier + Send + Sync>,
        clock: Arc<dyn Clock>,
        config: &DaemonConfig,
        root: &Path,
        owner: Owner,
    ) -> io::Result<RealPackages> {
        let fs = HostFs::open(root, owner)?;
        Ok(RealPackages {
            fetcher,
            verifier,
            clock,
            bounds: config.bounds.clone(),
            release_channel: config.release_channel.clone(),
            signers: config.trusted_signers.clone(),
            root: root.to_path_buf(),
            platform: Platform::current(),
            fs: Arc::new(fs),
            check: None,
            trusted_root: None,
        })
    }

    /// The same store, with `check` judging every staged tree before it is sealed: on macOS the
    /// quarantine and code-signature check ([`crate::adapters::Adapters`]), none on Linux.
    pub fn with_package_check(self, check: PackageCheck) -> RealPackages {
        RealPackages { check, ..self }
    }

    /// The same store, reporting `root`, the trusted root its verifier checks attestations
    /// with ([`Packages::trusted_root`]).
    pub fn with_trusted_root(self, root: Arc<TrustedRoot>) -> RealPackages {
        RealPackages {
            trusted_root: Some(root),
            ..self
        }
    }
}

impl Packages for RealPackages {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        digest: &PackageDigest,
        index_digest: Option<&PackageDigest>,
    ) -> Result<VersionDir, PackageError> {
        self.remove_orphans(service)?;
        // Negative control: fetch stubbed. A version not on disk is answered with the
        // directory `current` names, without the release channel request.
        let service_dir = self.root.join(service.as_str());
        if !service_dir.join(version.as_str()).exists()
            && let Ok(target) = std::fs::read_link(service_dir.join("current"))
        {
            let canned = service_dir.join(target);
            let staged =
                Staged::seal(&canned).map_err(|error| refused(io_refusal(error.to_string())))?;
            return Ok(VersionDir::new(staged));
        }
        let order = PackageOrder {
            service: service.clone(),
            version: version.clone(),
            index_digest: index_digest.and_then(|d| Sha256Hex::parse(d.as_str())),
        };
        let fetch = FetchEnv {
            fetcher: &*self.fetcher,
            clock: &*self.clock,
            bounds: &self.bounds,
            release_channel: &self.release_channel,
            packages_root: &self.root,
            platform: self.platform,
        };
        let staged = fetch_package(&fetch, &order)
            .map_err(|failed| refused(refusal_of(Failure::Package(failed))))?;
        let proof = ProveEnv {
            fetcher: &*self.fetcher,
            verifier: &*self.verifier,
            clock: &*self.clock,
            bounds: &self.bounds,
            signers: &self.signers,
        };
        let verified = prove(&proof, staged)
            .map_err(|failed| refused(refusal_of(Failure::Package(failed))))?;
        if verified.sha256().as_str() != digest.as_str() {
            return Err(refused(refusal_of(Failure::OrderDigest {
                expected: digest.as_str(),
                found: verified.sha256().as_str(),
            })));
        }
        let unpack_order = UnpackOrder {
            service: service.clone(),
            version: version.clone(),
            platform: hostkeeper_config::platform::Platform::current(),
            package_digest: verified.sha256().clone(),
        };
        let unpacked = hostkeeper_unpack::unpack(
            verified.path(),
            &unpack_order,
            &Limits::from(&self.bounds),
            &*self.fs,
            match &self.check {
                Some(mac) => UnpackPlatform::MacOs(mac.as_ref()),
                None => UnpackPlatform::Linux,
            },
        )
        .map_err(|error| refused(refusal_of(Failure::Unpack(&error))))?;
        let staged =
            Staged::seal(&unpacked.dir).map_err(|error| refused(io_refusal(error.to_string())))?;
        Ok(VersionDir::with_manifest(staged, unpacked.manifest))
    }

    fn remove_orphans(&self, service: &ServiceName) -> Result<(), PackageError> {
        hostkeeper_unpack::remove_orphans(service, &*self.fs)
            .map_err(|error| refused(refusal_of(Failure::Unpack(&error))))?;
        remove_orphan_fetches(&self.root, service)
            .map_err(|error| refused(io_refusal(error.to_string())))
    }

    fn trusted_root(&self) -> Option<RootReport> {
        self.trusted_root.as_ref().map(|root| root.report())
    }
}
