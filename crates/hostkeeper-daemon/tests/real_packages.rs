//! The real package store: the table that maps every failure of the package libraries onto the
//! daemon's closed set of reasons, the sweep of orphans, and the fixture package through fetch,
//! proof, unpack and seal.

mod support_archive;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::manifest::{HostkeeperVersion, ManifestRefusal};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::packages::{PackageError, PackageRefusal, Packages, VersionDir};
use hostkeeper_daemon::real_packages::{Failure, RealPackages, refusal_of};
use hostkeeper_daemon::refusal::Refusal;
use hostkeeper_daemon::testing::FakePackages;
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_package::{
    ATTESTATION_UNAVAILABLE, FETCH_FAILED, INDEX_INVALID, NO_PACKAGE_FOR_PLATFORM, NO_SPACE,
    NOT_ATTESTED, PACKAGE_DIGEST_MISMATCH, PackageFailure, REASONS,
};
use hostkeeper_unpack::{MacVerify, Rule, SignatureVerdict, UnpackError, VerifyIoError};
use support_archive::{Answer, Rig, SERVICE, VERSION, sha256_hex, valid_archive};

const RELEASE: (Outcome, Class) = (Outcome::Failure, Class::Release);
const DEFERRED: (Outcome, Class) = (Outcome::Deferred, Class::Environment);

fn digest_of(bytes: &[u8]) -> PackageDigest {
    PackageDigest::new(&sha256_hex(bytes)).expect("a digest")
}

/// The sorted names in `dir`; empty when it is not there.
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

/// Makes the directory `dir` with mode `mode`.
fn make_dir(dir: &Path, mode: u32) {
    fs::create_dir_all(dir).expect("make the dir");
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).expect("set its mode");
}

fn assert_refused(error: &PackageError, refusal: Refusal, kind: (Outcome, Class)) {
    assert_eq!(error.refusal(), refusal, "{error}");
    assert_eq!((error.outcome(), error.class()), kind, "{error}");
    assert!(matches!(error, PackageError::Refused(_)), "{error:?}");
}

/// Catches a failure of a library that reaches the daemon as a free string, a token the closed
/// set does not hold, or an outcome or class the library did not give: each library token, each
/// variant of the unpack's errors and the order's digest map to one refusal with the library's
/// outcome and class, and a token a library adds later is `package_failed`.
#[test]
fn every_package_failure_maps_to_one_closed_reason() {
    let package_rows = [
        (INDEX_INVALID, Refusal::IndexInvalid, RELEASE),
        (
            NO_PACKAGE_FOR_PLATFORM,
            Refusal::NoPackageForPlatform,
            RELEASE,
        ),
        (PACKAGE_DIGEST_MISMATCH, Refusal::DigestMismatch, RELEASE),
        (FETCH_FAILED, Refusal::FetchFailed, DEFERRED),
        (
            ATTESTATION_UNAVAILABLE,
            Refusal::AttestationUnavailable,
            DEFERRED,
        ),
        (NO_SPACE, Refusal::NoSpace, DEFERRED),
        (NOT_ATTESTED, Refusal::NotAttested, RELEASE),
    ];
    let mut covered: Vec<&str> = package_rows.iter().map(|row| row.0).collect();
    covered.sort_unstable();
    let mut reasons = REASONS.to_vec();
    reasons.sort_unstable();
    assert_eq!(covered, reasons, "one row per reason of hostkeeper-package");
    for (reason, refusal, (outcome, class)) in package_rows {
        let failed = PackageFailure {
            outcome,
            class,
            reason,
        };
        let refused = refusal_of(Failure::Package(failed));
        assert_eq!(
            (refused.refusal, refused.outcome, refused.class),
            (refusal, outcome, class),
            "{reason}"
        );
        // The library's own token, but for its digest mismatch, which is the daemon's
        // `digest_mismatch` whichever step found it.
        let token = match reason {
            PACKAGE_DIGEST_MISMATCH => "digest_mismatch",
            other => other,
        };
        assert_eq!(refused.refusal.token(), token, "{reason}");
        assert_eq!(refused.detail, failed.to_string());
    }

    let too_old = ManifestRefusal::DaemonTooOld {
        needs: HostkeeperVersion::parse("9.0.0").expect("a version"),
        running: HostkeeperVersion::parse("0.1.0").expect("a version"),
    };
    let unpack_rows = [
        (
            UnpackError::BadArchive {
                entry: "the archive".to_owned(),
                rule: Rule::TooLarge,
                at: 7,
            },
            Refusal::BadArchive,
            RELEASE,
        ),
        (
            UnpackError::DigestMismatch {
                expected: "a".to_owned(),
                found: "b".to_owned(),
            },
            Refusal::DigestMismatch,
            RELEASE,
        ),
        (
            UnpackError::Manifest(ManifestRefusal::TooLarge),
            Refusal::ManifestMismatch,
            RELEASE,
        ),
        (
            UnpackError::Manifest(too_old),
            Refusal::DaemonTooOld,
            DEFERRED,
        ),
        (
            UnpackError::NoSpace(io::Error::other("full")),
            Refusal::NoSpace,
            DEFERRED,
        ),
        (
            UnpackError::ArtifactChanged {
                version: "1.0.0".to_owned(),
                why: "its tree changed".to_owned(),
            },
            Refusal::ArtifactChanged,
            RELEASE,
        ),
        (
            UnpackError::SealUnreadable {
                version: "1.0.0".to_owned(),
                why: "unreadable".to_owned(),
            },
            Refusal::PackageIoError,
            DEFERRED,
        ),
        (
            UnpackError::SealSchema {
                version: "1.0.0".to_owned(),
                found: 9,
            },
            Refusal::DaemonTooOld,
            DEFERRED,
        ),
        (
            UnpackError::ReservedVersion {
                version: "current".to_owned(),
            },
            Refusal::ReservedVersion,
            RELEASE,
        ),
        (
            UnpackError::VersionTooLong {
                version: "1".repeat(300),
            },
            Refusal::VersionTooLong,
            RELEASE,
        ),
        (
            UnpackError::VerifyUnavailable {
                entry: "bin/x".to_owned(),
                source: VerifyIoError::Unfinished {
                    tool: "codesign",
                    why: "killed".to_owned(),
                },
            },
            Refusal::VerifyUnavailable,
            DEFERRED,
        ),
        (
            UnpackError::Io {
                what: "cannot write",
                source: io::Error::other("disk"),
            },
            Refusal::PackageIoError,
            DEFERRED,
        ),
    ];
    for (error, refusal, (outcome, class)) in &unpack_rows {
        let refused = refusal_of(Failure::Unpack(error));
        assert_eq!(
            (refused.refusal, refused.outcome, refused.class),
            (*refusal, *outcome, *class),
            "{error}"
        );
        assert_eq!(refused.detail, error.to_string());
    }

    let ordered = refusal_of(Failure::OrderDigest {
        expected: "aa",
        found: "bb",
    });
    assert_eq!(
        ordered,
        PackageRefusal {
            refusal: Refusal::DigestMismatch,
            outcome: Outcome::Failure,
            class: Class::Release,
            detail: "the package's SHA-256 is bb, not the order's aa".to_owned(),
        }
    );

    let unknown = refusal_of(Failure::Package(PackageFailure {
        outcome: Outcome::Deferred,
        class: Class::Environment,
        reason: "brand_new",
    }));
    assert_eq!(
        (unknown.refusal, unknown.outcome, unknown.class),
        (
            Refusal::PackageFailed,
            Outcome::Deferred,
            Class::Environment
        )
    );

    let mut seen = std::collections::BTreeSet::new();
    for refusal in Refusal::ALL {
        let token = refusal.token();
        let grammar = (1..=64).contains(&token.len())
            & token
                .bytes()
                .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'_'));
        assert!(grammar, "{refusal:?}: {token}");
        assert!(seen.insert(token), "{token} twice");
    }
}

/// Catches an error that reports another token, outcome or class than its variant says, and a
/// refusal whose text drops its token.
#[test]
fn package_errors_say_their_refusal_outcome_and_class() {
    let refused = PackageError::Refused(PackageRefusal {
        refusal: Refusal::PackageIoError,
        outcome: Outcome::Deferred,
        class: Class::Environment,
        detail: "the disk".to_owned(),
    });
    assert_eq!(refused.to_string(), "the disk (io_error)");
    let rows = [
        (
            PackageError::NotFound("n".to_owned()),
            Refusal::PackageNotFound,
            RELEASE,
        ),
        (
            PackageError::DigestMismatch("d".to_owned()),
            Refusal::DigestMismatch,
            RELEASE,
        ),
        (
            PackageError::NoSpace("s".to_owned()),
            Refusal::NoSpace,
            DEFERRED,
        ),
        (
            PackageError::Other("o".to_owned()),
            Refusal::PackageFailed,
            DEFERRED,
        ),
        (refused, Refusal::PackageIoError, DEFERRED),
    ];
    for (error, refusal, kind) in rows {
        assert_eq!(error.refusal(), refusal, "{error}");
        assert_eq!((error.outcome(), error.class()), kind, "{error}");
    }
}

/// A store with nothing of its own to sweep.
struct Plain;

impl Packages for Plain {
    fn version_dir(
        &self,
        _service: &ServiceName,
        _version: &Version,
        _digest: &PackageDigest,
    ) -> Result<VersionDir, PackageError> {
        Err(PackageError::Other("no packages".to_owned()))
    }
}

/// Catches a port whose default sweep fails, and a fake that does not record the sweep the
/// daemon asks for at start-up.
#[test]
fn the_default_sweep_removes_nothing_and_the_fake_records_it() {
    let service = Rig::service();
    assert_eq!(Plain.remove_orphans(&service), Ok(()));
    let rig = Rig::new("fake-sweep");
    let fake = FakePackages::new(&rig.packages);
    assert_eq!(fake.swept(), Vec::<String>::new());
    assert_eq!(fake.remove_orphans(&service), Ok(()));
    assert_eq!(fake.swept(), vec![SERVICE.to_owned()]);
}

/// Catches a sweep that leaves a crashed fetch's staging file or a crashed unpack's staging
/// directory or seals, that removes a version or a seal that has its version, or that fails
/// for a service with no directory.
#[test]
fn remove_orphans_removes_fetch_and_stage_leftovers() {
    let rig = Rig::new("sweep");
    let store = rig.store(&valid_archive(), Answer::Confirms);
    let service = rig.packages.join(SERVICE);
    make_dir(&service, 0o755);
    make_dir(&service.join("1.0.0"), 0o755);
    make_dir(&service.join(".stage-x"), 0o700);
    for file in [
        ".fetch-1-1",
        ".stage-x/inner",
        "1.0.0.seal",
        "9.0.0.seal",
        ".2.0.0.seal.tmp",
        "notes",
    ] {
        fs::write(service.join(file), "x").expect("write");
    }
    assert_eq!(store.remove_orphans(&Rig::service()), Ok(()));
    assert_eq!(names(&service), ["1.0.0", "1.0.0.seal", "notes"]);

    let absent = ServiceName::new("absent").expect("a service name");
    assert_eq!(store.remove_orphans(&absent), Ok(()));
    assert!(!rig.packages.join("absent").exists());
}

/// Catches a fetch that starts over a crashed one's leftovers: `fetch_package` removes only
/// `.fetch-*`, so the `.stage-*` directory going too shows the store's own sweep ran before the
/// fetch, which then fails as `fetch_failed` for the index the channel does not have.
#[test]
fn orphans_are_removed_before_each_fetch() {
    let rig = Rig::new("sweep-before-fetch");
    let service = rig.packages.join(SERVICE);
    make_dir(&service, 0o755);
    make_dir(&service.join(".stage-x"), 0o700);
    fs::write(service.join(".fetch-1-1"), "x").expect("write");
    let store = rig.open(Vec::new(), Answer::Confirms).expect("open");
    let error = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(b"a"))
        .expect_err("the channel has nothing");
    assert_refused(&error, Refusal::FetchFailed, DEFERRED);
    assert_eq!(names(&service), Vec::<String>::new());
}

/// Catches a sweep that fails the order silently, or a failure of it that is not reported as a
/// deferred `io_error`: the service's entry is a file (the unpack's sweep cannot lock it), or a
/// directory the group can write to (the fetch's sweep refuses it).
#[test]
fn a_sweep_that_cannot_run_defers_the_order() {
    let archive = valid_archive();
    let digest = digest_of(&archive);
    for as_file in [true, false] {
        let rig = Rig::new("sweep-fails");
        let service = rig.packages.join(SERVICE);
        if as_file {
            fs::write(&service, "x").expect("write");
        } else {
            make_dir(&service, 0o775);
        }
        let store = rig.store(&archive, Answer::Confirms);
        let error = store
            .remove_orphans(&Rig::service())
            .expect_err("the sweep fails");
        assert_refused(&error, Refusal::PackageIoError, DEFERRED);
        let ordered = store
            .version_dir(&Rig::service(), &Rig::version(), &digest)
            .expect_err("the order is deferred");
        assert_eq!(ordered, error);
        assert!(rig.fetched.lock().expect("log").is_empty());
    }
}

/// Catches a store that cannot be opened over a packages directory that is not there, and
/// reports it as anything but the directory's own error.
#[test]
fn a_packages_directory_that_is_missing_cannot_be_opened() {
    let mut rig = Rig::new("missing-root");
    rig.packages = rig.scratch.path().join("missing");
    let error = rig
        .open(Vec::new(), Answer::Confirms)
        .map(|_| ())
        .expect_err("no such directory");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

/// Catches a package that skips a step or is unpacked from other bytes than the proven ones: the
/// fixture goes through the index, the archive and the bundle, the verifier is asked about the
/// order's digest, and the answer is the sealed `<packages>/<service>/<version>` with the
/// manifest the unpack parsed.
#[test]
fn a_fixture_archive_flows_through_fetch_prove_and_unpack_to_a_sealed_version_directory() {
    let rig = Rig::new("flow");
    let archive = valid_archive();
    let sha = sha256_hex(&archive);
    let store = rig.store(&archive, Answer::Confirms);
    let dir = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
        .expect("the package is had");

    let version_dir = rig.packages.join(SERVICE).join(VERSION);
    assert_eq!(dir.dir(), version_dir);
    assert!(
        dir.staged().check(Path::new("bin/x")).is_ok(),
        "the tree still has its seal"
    );
    assert!(rig.packages.join(SERVICE).join("1.5.0.seal").is_file());
    let manifest = dir.manifest().expect("the parsed manifest");
    assert_eq!(manifest.name.as_str(), SERVICE);
    assert_eq!(manifest.version.as_str(), VERSION);
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(*rig.verified.lock().expect("log"), vec![sha]);
    assert_eq!(*rig.fetched.lock().expect("log"), rig.urls());
    assert_eq!(
        names(&rig.packages.join(SERVICE)),
        ["1.5.0", "1.5.0.seal"],
        "no staging file is left"
    );

    let again = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
        .expect("a second order finds the version whole");
    assert_eq!(again.staged(), dir.staged());
}

/// Catches an order whose digest is not the channel's: the archive is fetched and proven as the
/// index says, the order's digest is another, so it is `digest_mismatch` (a bad release) and
/// nothing is unpacked.
#[test]
fn an_order_that_names_another_digest_than_the_index_unpacks_nothing() {
    let rig = Rig::new("other-digest");
    let store = rig.store(&valid_archive(), Answer::Confirms);
    let error = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(b"other bytes"))
        .expect_err("the digest is not the order's");
    assert_refused(&error, Refusal::DigestMismatch, RELEASE);
    assert!(error.to_string().contains(&sha256_hex(&valid_archive())));
    assert!(!rig.packages.join(SERVICE).join(VERSION).exists());
    assert_eq!(names(&rig.packages.join(SERVICE)), Vec::<String>::new());
}

/// Catches a verifier's rejection that is let through or reported as a failure of the host: the
/// package is `not_attested`, a bad release, and nothing is unpacked.
#[test]
fn a_package_the_verifier_rejects_is_not_attested() {
    let rig = Rig::new("rejected");
    let archive = valid_archive();
    let store = rig.store(&archive, Answer::Rejects);
    let error = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
        .expect_err("not attested");
    assert_refused(&error, Refusal::NotAttested, RELEASE);
    assert!(!rig.packages.join(SERVICE).join(VERSION).exists());
}

/// Catches a verifier that could not run being blamed on the package: the order is deferred as
/// `attestation_unavailable`, a failure of the environment.
#[test]
fn a_verifier_that_cannot_run_defers_the_order() {
    let rig = Rig::new("unavailable");
    let archive = valid_archive();
    let store = rig.store(&archive, Answer::Unavailable);
    let error = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
        .expect_err("no verdict");
    assert_refused(&error, Refusal::AttestationUnavailable, DEFERRED);
    assert!(!rig.packages.join(SERVICE).join(VERSION).exists());
}

/// Catches an archive that is not a zstd stream of a tar being taken: it has the digest the
/// index, the bundle and the order name, so only the unpack can refuse it, as `bad_archive`,
/// and no version directory or staging directory is left.
#[test]
fn an_archive_that_is_not_a_package_is_a_bad_archive() {
    let rig = Rig::new("bad-archive");
    let archive = b"this is not a zstd frame".to_vec();
    let store = rig.store(&archive, Answer::Confirms);
    let error = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
        .expect_err("not an archive");
    assert_refused(&error, Refusal::BadArchive, RELEASE);
    assert_eq!(names(&rig.packages.join(SERVICE)), Vec::<String>::new());
}

/// Catches a version directory that cannot be sealed being handed back: a packages directory
/// spelled with `..` unpacks, but `Staged::seal` refuses its path, so the order is deferred as
/// `io_error`.
#[test]
fn a_version_directory_that_cannot_be_sealed_defers_the_order() {
    let mut rig = Rig::new("unsealable");
    rig.packages = rig
        .scratch
        .path()
        .join("packages")
        .join("..")
        .join("packages");
    let archive = valid_archive();
    let store = rig.store(&archive, Answer::Confirms);
    let error = store
        .version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
        .expect_err("the path cannot be sealed");
    assert_refused(&error, Refusal::PackageIoError, DEFERRED);
}

/// A package check that says every entry is (or is not) quarantined, or cannot be made, and
/// keeps the paths it was asked about.
struct Judge {
    answer: Option<bool>,
    asked: Mutex<Vec<PathBuf>>,
}

impl Judge {
    fn new(answer: Option<bool>) -> Arc<Judge> {
        Arc::new(Judge {
            answer,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<PathBuf> {
        self.asked.lock().expect("the log").clone()
    }
}

impl MacVerify for Judge {
    fn quarantine(&self, path: &Path) -> Result<bool, VerifyIoError> {
        self.asked.lock().expect("the log").push(path.to_path_buf());
        self.answer.ok_or_else(|| VerifyIoError::Unfinished {
            tool: "getxattr",
            why: "the test says so".to_owned(),
        })
    }

    fn signature(&self, _binary: &Path) -> Result<SignatureVerdict, VerifyIoError> {
        Ok(SignatureVerdict::Valid)
    }
}

/// Catches a store that never hands its package check to the unpack (a Mac sealing a quarantined
/// package) or that hands it on Linux: with a check that finds every entry quarantined the
/// package is `bad_archive`, a bad release, and nothing is sealed; with a check that cannot be
/// made it is deferred as `verify_unavailable`; with a check that finds nothing it seals as on
/// Linux, every staged entry having been asked about; with no check (Linux) none is asked.
#[test]
fn the_package_check_of_the_platform_judges_every_staged_tree_before_it_is_sealed() {
    let archive = valid_archive();
    let order = |store: &RealPackages| {
        store.version_dir(&Rig::service(), &Rig::version(), &digest_of(&archive))
    };

    let rig = Rig::new("check-quarantined");
    let judge = Judge::new(Some(true));
    let store = rig
        .store(&archive, Answer::Confirms)
        .with_package_check(Some(judge.clone()));
    let error = order(&store).expect_err("a quarantined package");
    assert_refused(&error, Refusal::BadArchive, RELEASE);
    assert!(error.to_string().contains("quarantine"), "{error}");
    assert_eq!(
        judge.asked().len(),
        1,
        "the first quarantined entry ends the check"
    );
    assert_eq!(names(&rig.packages.join(SERVICE)), Vec::<String>::new());

    let rig = Rig::new("check-unavailable");
    let judge = Judge::new(None);
    let store = rig
        .store(&archive, Answer::Confirms)
        .with_package_check(Some(judge.clone()));
    let error = order(&store).expect_err("no verdict");
    assert_refused(&error, Refusal::VerifyUnavailable, DEFERRED);
    assert_eq!(names(&rig.packages.join(SERVICE)), Vec::<String>::new());

    let rig = Rig::new("check-clean");
    let judge = Judge::new(Some(false));
    let store = rig
        .store(&archive, Answer::Confirms)
        .with_package_check(Some(judge.clone()));
    let dir = order(&store).expect("a clean package seals");
    assert_eq!(dir.dir(), rig.packages.join(SERVICE).join(VERSION));
    assert!(rig.packages.join(SERVICE).join("1.5.0.seal").is_file());
    let asked = judge.asked();
    assert!(
        asked.len() >= 7,
        "every entry of the tree is asked about: {asked:?}"
    );
    assert!(
        asked.iter().any(|path| path.ends_with("bin/x")),
        "{asked:?}"
    );

    let rig = Rig::new("check-none");
    let judge = Judge::new(Some(true));
    let store = rig
        .store(&archive, Answer::Confirms)
        .with_package_check(None);
    order(&store).expect("Linux has no check");
    assert!(judge.asked().is_empty());
}

/// The store is what the daemon holds across threads.
#[test]
fn the_store_is_send_and_sync() {
    fn is_port<T: Packages + Send + Sync>() {}
    is_port::<RealPackages>();
}

/// Catches a store that reports no trusted root when it was given one, one that reports a root
/// it was not given, or a report taken once and kept: the store without a root reports none (as
/// the port's default does), and with one it reports that root's age and reason as of each call.
#[test]
fn the_store_reports_the_trusted_root_it_was_given_as_of_each_call() {
    use hostkeeper_daemon::trusted_root::{RootProblem, RootReport, TrustedRoot};
    use hostkeeper_link::testing::FakeClock as UnixFake;

    let rig = Rig::new("trusted-root-report");
    let bare = rig
        .open(Vec::new(), Answer::Confirms)
        .expect("open the store");
    assert_eq!(bare.trusted_root(), None);
    assert_eq!(FakePackages::new(&rig.packages).trusted_root(), None);

    let packaged = rig
        .packages
        .join("hostkeeper/current/files/trusted_root.json");
    let installed = rig.packages.join("no-such-root.json");
    let clock = Arc::new(UnixFake::new(5_000, 0));
    let root = Arc::new(TrustedRoot::new(
        packaged,
        installed.clone(),
        1,
        Arc::clone(&clock) as Arc<dyn hostkeeper_link::UnixClock>,
    ));
    let store = rig
        .open(Vec::new(), Answer::Confirms)
        .expect("open the store")
        .with_trusted_root(root);
    let missing = RootReport {
        path: installed.clone(),
        age_secs: None,
        problem: Some(RootProblem::Missing),
    };
    assert_eq!(store.trusted_root(), Some(missing));
    fs::write(
        &installed,
        "{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json;version=0.1\"}\n",
    )
    .expect("write the root");
    let now = store.trusted_root().expect("a report");
    assert_eq!((now.path, now.problem), (installed, None));
}
