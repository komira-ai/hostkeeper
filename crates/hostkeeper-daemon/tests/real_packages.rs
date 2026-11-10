//! The real package store: the table that maps every failure of the pinned fetch onto the
//! daemon's closed set of reasons, and the fetch of a `.pkg` by its pin through the host's stores.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::Bounds;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::packages::{PackageError, PackageRefusal, Packages};
use hostkeeper_daemon::real_packages::{RealPackages, refusal_of};
use hostkeeper_daemon::refusal::Refusal;
use hostkeeper_daemon::testing::{FakeClock, FakePackages};
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_package::ports::{Fetcher, Got, Limit, NetError, Sink, Url};
use hostkeeper_package::stores::{self, Stores};
use hostkeeper_package::{
    FETCH_FAILED, NO_SPACE, NO_STORE, PACKAGE_DIGEST_MISMATCH, PackageFailure, REASONS,
    STORE_DIGEST_MISMATCH,
};
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target};
use sha2::{Digest, Sha256};
use support::Scratch;

const SERVICE: &str = "hello";
const RELEASE: (Outcome, Class) = (Outcome::Failure, Class::Release);
const DEFERRED: (Outcome, Class) = (Outcome::Deferred, Class::Environment);

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |hex, byte| format!("{hex}{byte:02x}"))
}

fn service() -> ServiceName {
    ServiceName::new(SERVICE).expect("a service name")
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

fn assert_refused(error: &PackageError, refusal: Refusal, kind: (Outcome, Class)) {
    assert_eq!(error.refusal(), refusal, "{error}");
    assert_eq!((error.outcome(), error.class()), kind, "{error}");
    assert!(matches!(error, PackageError::Refused(_)), "{error:?}");
}

/// Catches a failure of the fetch that reaches the daemon as a free string, a token the closed
/// set does not hold, or an outcome or class the library did not give: each library token a
/// pinned fetch gives maps to one refusal with the library's outcome and class, and a token the
/// library adds later is `package_failed`.
#[test]
fn every_pinned_failure_maps_to_one_closed_reason() {
    let rows = [
        (PACKAGE_DIGEST_MISMATCH, Refusal::DigestMismatch, RELEASE),
        (FETCH_FAILED, Refusal::FetchFailed, DEFERRED),
        (NO_SPACE, Refusal::NoSpace, DEFERRED),
        (
            NO_STORE,
            Refusal::NoStore,
            (Outcome::Failure, Class::Environment),
        ),
        (
            STORE_DIGEST_MISMATCH,
            Refusal::StoreDigestMismatch,
            DEFERRED,
        ),
    ];
    for (reason, refusal, (outcome, class)) in rows {
        assert!(
            REASONS.contains(&reason),
            "{reason} is a reason of the library"
        );
        let failed = PackageFailure {
            outcome,
            class,
            reason,
        };
        let refused = refusal_of(failed);
        assert_eq!(
            (refused.refusal, refused.outcome, refused.class),
            (refusal, outcome, class),
            "{reason}"
        );
        // The library's own token, but for its digest mismatch, which is the daemon's
        // `digest_mismatch`.
        let token = match reason {
            PACKAGE_DIGEST_MISMATCH => "digest_mismatch",
            other => other,
        };
        assert_eq!(refused.refusal.token(), token, "{reason}");
        assert_eq!(refused.detail, failed.to_string());
    }

    let unknown = refusal_of(PackageFailure {
        outcome: Outcome::Deferred,
        class: Class::Environment,
        reason: "brand_new",
    });
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

/// A fetcher that serves fixed bodies by URL and answers 404 for any other, and records each URL.
struct FakeFetcher {
    routes: Vec<(String, Vec<u8>)>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Fetcher for FakeFetcher {
    fn get(&self, url: &Url, sink: &mut dyn Sink, _limit: Limit) -> Result<Got, NetError> {
        self.asked
            .lock()
            .expect("the log")
            .push(url.as_str().to_owned());
        let body = self
            .routes
            .iter()
            .find(|(route, _)| route == url.as_str())
            .map(|(_, body)| body)
            .ok_or(NetError::Status(404))?;
        let delivered = sink
            .begin(Some(body.len() as u64))
            .and_then(|()| body.chunks(4096).try_for_each(|piece| sink.chunk(piece)));
        Ok(delivered.map_or(Got::Stopped, |()| Got::Complete))
    }
}

/// A packages directory in a scratch directory and what the fetcher was asked.
struct Rig {
    scratch: Scratch,
    packages: PathBuf,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let scratch = Scratch::new(name);
        let packages = scratch.path().join("packages");
        fs::create_dir(&packages).expect("make the packages dir");
        Rig {
            scratch,
            packages,
            asked: Arc::default(),
        }
    }

    /// A store over the packages directory that serves `routes`, through `stores`.
    fn open(&self, routes: Vec<(String, Vec<u8>)>, stores: Stores) -> RealPackages {
        let fetcher = FakeFetcher {
            routes,
            asked: Arc::clone(&self.asked),
        };
        RealPackages::new(
            Box::new(fetcher),
            FakeClock::new(),
            Bounds::default(),
            stores,
            &self.packages,
        )
    }
}

/// The host's stores: `channel`, an https store.
fn channel() -> Stores {
    stores::parse(
        b"[[store]]\nname = \"channel\"\nkind = \"https\"\nbase = \"https://releases.example/\"\n",
        0,
    )
    .expect("the channel store")
}

/// An entry whose artifact is at `locator` of `channel`, and no attestation.
fn pkg_entry(locator: &str, bytes: &[u8]) -> Target {
    Target {
        installer: "macos-pkg".to_owned(),
        locations: vec![Location {
            store: "channel".to_owned(),
            locator: locator.to_owned(),
        }],
        digest: sha256_hex(bytes),
        bytes: bytes.len() as u64,
        ..Target::default()
    }
}

/// Catches: a `.pkg` fetched by its pin into a file the store deletes before the installer's
/// `stage` reads it, a pinned fetch that asks for an attestation, a refusal of the library mapped
/// to another reason, and a fake that answers anything but its refusal for a digest it does not
/// serve. The real store fetches the entry's artifact by its pin alone (the one request its
/// location names) into a staging file of the service's package directory that lives as long as
/// the answer; other bytes are `package_digest_mismatch` (release) and leave nothing.
#[test]
fn a_pkg_is_fetched_by_its_pin_into_a_file_that_lives_as_long_as_the_answer() {
    let rig = Rig::new("pinned");
    let bytes = b"the signed pkg".to_vec();
    let locator = "hello/1.0.0/hello-1.0.0-macos-arm64.pkg";
    let url = format!("https://releases.example/{locator}");
    let store = rig.open(vec![(url.clone(), bytes.clone())], channel());
    let pinned = store
        .pinned(&service(), &pkg_entry(locator, &bytes))
        .expect("the pkg is fetched");
    assert_eq!(fs::read(pinned.path()).expect("it reads"), bytes);
    assert!(pinned.path().starts_with(rig.packages.join(SERVICE)));
    assert_eq!(*rig.asked.lock().expect("log"), vec![url]);
    let path = pinned.path().to_path_buf();
    drop(pinned);
    assert!(!path.exists(), "the staging file goes with the answer");

    let other = store
        .pinned(&service(), &pkg_entry(locator, b"other bytes"))
        .err()
        .expect("other bytes are refused");
    assert_refused(&other, Refusal::DigestMismatch, RELEASE);
    assert!(
        names(&rig.packages.join(SERVICE)).is_empty(),
        "no staging file is left"
    );

    let fake = FakePackages::new(&rig.packages);
    assert!(matches!(
        fake.pinned(&service(), &pkg_entry(locator, &bytes)).err(),
        Some(PackageError::DigestMismatch(_))
    ));
}

/// Catches a host with no store resolving an entry anyway, and a mirror that gave other bytes
/// going unsaid: with no store the fetch is `no_store` and asks nothing; a mirror that gives other
/// bytes before the location gives the right ones is logged as `store_digest_mismatch` naming it,
/// and the fetch still succeeds.
#[test]
fn a_host_with_no_store_has_no_source_and_a_mirror_with_other_bytes_is_logged() {
    let rig = Rig::new("pinned-stores");
    let bytes = b"the signed pkg".to_vec();
    let locator = "hello/1.0.0/hello-1.0.0-macos-arm64.pkg";
    let entry = pkg_entry(locator, &bytes);

    let none = rig.open(Vec::new(), Stores::default());
    let refused = none
        .pinned(&service(), &entry)
        .err()
        .expect("no store defined");
    assert_refused(
        &refused,
        Refusal::NoStore,
        (Outcome::Failure, Class::Environment),
    );
    assert!(rig.asked.lock().expect("log").is_empty());

    let both = stores::parse(
        b"[[store]]\nname = \"channel\"\nkind = \"https\"\nbase = \"https://releases.example/\"\n\n\
          [[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://lan.example/\"\n\
          mirror = true\nrepository = \"releases\"\n",
        0,
    )
    .expect("the two stores");
    let mirror = format!(
        "https://lan.example/v2/releases/blobs/sha256:{}",
        sha256_hex(&bytes)
    );
    let routes = vec![
        (mirror, b"wrong bytes".to_vec()),
        (format!("https://releases.example/{locator}"), bytes.clone()),
    ];
    let logged = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&logged);
    let store = rig.open(routes, both).with_log(Arc::new(move |line| {
        sink.lock().expect("log").push(line.to_owned())
    }));
    let pinned = store
        .pinned(&service(), &entry)
        .expect("the pkg is fetched");
    assert_eq!(fs::read(pinned.path()).expect("it reads"), bytes);
    assert_eq!(
        *logged.lock().expect("log"),
        ["store_digest_mismatch: the store lan gave other bytes than the entry names for hello"]
    );
    let _ = rig.scratch.path();
}
