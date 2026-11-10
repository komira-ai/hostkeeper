//! `stage` of a private repository: its entry's URI is in the mirror root, and the mirror port is
//! called before the index is read and before the package is downloaded.

use std::sync::{Arc, Mutex};

use hostkeeper_install::repository::config::parse;
use hostkeeper_install::repository::{
    Class, Mirror, MirrorError, Reason, Repositories, Repository, mirrored_name,
};

use crate::harness::*;

use Class::Environment;
use hostkeeper_install::repository::Outcome::{Deferred, Failed};

/// What the work directory held when the mirror was called.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    /// `index <arch>` or `pool <filename> <sha256> <size>`.
    call: String,
    repository: String,
    suite: String,
    /// Whether the lists directory held files.
    listed: bool,
    /// Whether the archives directory held files.
    archived: bool,
}

/// A mirror that records each call with the state of the work directory, and fails as told.
struct FakeMirror {
    work: std::path::PathBuf,
    calls: Mutex<Vec<Seen>>,
    index: Option<MirrorError>,
    pool: Option<MirrorError>,
}

struct Shared(Arc<FakeMirror>);

impl FakeMirror {
    fn seen(&self, repository: &Repository, call: String) {
        let count = |sub: &str| {
            listing(&self.work.join(sub))
                .iter()
                .filter(|f| !f.ends_with('/'))
                .count()
        };
        self.calls.lock().expect("calls").push(Seen {
            call,
            repository: repository.name.clone(),
            suite: repository.suite.clone(),
            listed: count("lists") > 0,
            archived: count("archives") > 0,
        });
    }

    fn calls(&self) -> Vec<Seen> {
        self.calls.lock().expect("calls").clone()
    }
}

impl Mirror for Shared {
    fn index(&self, repository: &Repository, architecture: &str) -> Result<(), MirrorError> {
        self.0.seen(repository, format!("index {architecture}"));
        self.0.index.clone().map_or(Ok(()), Err)
    }

    fn pool(
        &self,
        repository: &Repository,
        filename: &str,
        sha256: &str,
        size: u64,
    ) -> Result<(), MirrorError> {
        self.0
            .seen(repository, format!("pool {filename} {sha256} {size}"));
        self.0.pool.clone().map_or(Ok(()), Err)
    }
}

/// A host whose one repository is private: its URI is the mirror of the repository `acme`, and its mirror port
/// is a [`FakeMirror`] failing as given.
fn private(
    name: &str,
    index: Option<MirrorError>,
    pool: Option<MirrorError>,
) -> (Env, Arc<FakeMirror>, String) {
    let mut env = Env::new(name);
    let uri = format!("file:{}/acme", env.dirs.mirror.display());
    let keys = env.scratch.path().join("keys");
    let text = format!(
        "Types: deb\nURIs: {uri}\nSuites: stable\nComponents: main\nSigned-By: {}\n",
        keys.join("main.gpg").display()
    );
    env.repositories = Repositories::new(vec![parse(REPO, &text, &keys).expect("an entry")]);
    let mirror = Arc::new(FakeMirror {
        work: env.work(),
        calls: Mutex::new(Vec::new()),
        index,
        pool,
    });
    env.mirror = Some(Box::new(Shared(Arc::clone(&mirror))));
    (env, mirror, uri)
}

/// Proves a private repository's stage asks the mirror for the index (with the host's
/// architecture) before `apt-get update`, with the work directory still empty of lists, and for
/// the package file (the stanza's `Filename`, the index's SHA-256 and size) before the download,
/// with the lists read and no archive yet; and that the stage then succeeds.
#[test]
fn a_private_repository_fills_the_mirror_before_the_update_and_before_the_download() {
    let (env, mirror, uri) = private("apt-mirror-ok", None, None);
    let package = deb(V1);
    env.host.publish(&uri, package.clone());
    let order = order(&package, &["mdm.service"]);
    let (staged, _) = env.stage(&order);
    staged.expect("staged");
    let pool = format!(
        "pool pool/main/mdm_{V1}_amd64.deb {} {}",
        package.pin(),
        package.bytes().len()
    );
    assert_eq!(
        mirror.calls(),
        [
            Seen {
                call: "index amd64".to_owned(),
                repository: REPO.to_owned(),
                suite: "stable".to_owned(),
                listed: false,
                archived: false,
            },
            Seen {
                call: pool,
                repository: REPO.to_owned(),
                suite: "stable".to_owned(),
                listed: true,
                archived: false,
            },
        ]
    );
    let labels = env.host.labels();
    assert_eq!(
        labels[..3],
        [
            "dpkg --print-architecture",
            "apt-get update",
            "apt-cache show"
        ]
    );
    assert_eq!(
        labels
            .iter()
            .filter(|l| *l == "dpkg --print-architecture")
            .count(),
        1
    );
}

/// Proves a public repository is not mirrored: a host with a mirror port that stages from an
/// `https` entry never calls it.
#[test]
fn a_public_repository_never_calls_the_mirror() {
    let (mut env, mirror, _) = private("apt-mirror-public", None, None);
    let keys = env.scratch.path().join("keys");
    let text = format!(
        "Types: deb\nURIs: {URI}\nSuites: stable\nComponents: main\nSigned-By: {}\n",
        keys.join("main.gpg").display()
    );
    env.repositories = Repositories::new(vec![parse(REPO, &text, &keys).expect("an entry")]);
    let order = env.publish(&deb(V1));
    env.stage(&order).0.expect("staged");
    assert_eq!(mirror.calls(), []);
}

/// Proves each way the mirror fails maps to its reason: a suite not published to
/// `not_in_repository`, an origin that cannot be read to `fetch_failed` (both deferred), a pool
/// file that is not the pin to `store_digest_mismatch` (deferred) and a mirror that cannot be
/// written to `io_error` (failed); at the index nothing is fetched by apt, at the pool nothing is
/// downloaded; and the failure's text names the mirror.
#[test]
fn a_mirror_failure_is_the_reason_of_its_kind_and_stops_the_stage_where_it_happened() {
    let kinds = [
        (
            MirrorError::NotPublished("no suite".to_owned()),
            Reason::NotInRepository,
            Deferred,
        ),
        (
            MirrorError::Unreachable("down".to_owned()),
            Reason::FetchFailed,
            Deferred,
        ),
        (
            MirrorError::Digest("other bytes".to_owned()),
            Reason::StoreDigestMismatch,
            Deferred,
        ),
        (MirrorError::Io("full".to_owned()), Reason::IoError, Failed),
    ];
    for (error, reason, outcome) in kinds {
        let (env, _, uri) = private("apt-mirror-index-fault", Some(error.clone()), None);
        let package = deb(V1);
        env.host.publish(&uri, package.clone());
        let (result, _) = env.stage(&order(&package, &["mdm.service"]));
        let failure = result.expect_err("refused at the index");
        assert_failure(&failure, reason, Environment, outcome);
        assert_eq!(failure.detail(), format!("the mirror: {error}"));
        assert_eq!(env.host.labels(), ["dpkg --print-architecture"]);

        let (env, _, uri) = private("apt-mirror-pool-fault", None, Some(error.clone()));
        env.host.publish(&uri, package.clone());
        let (result, _) = env.stage(&order(&package, &["mdm.service"]));
        let failure = result.expect_err("refused at the pool");
        assert_failure(&failure, reason, Environment, outcome);
        assert!(!env.has_run("apt-get download"));
        assert!(env.has_run("apt-get simulate"));
    }
}

/// Proves a stanza with no `Filename` cannot be fetched into the mirror: `manifest_invalid`,
/// the pool is not asked and nothing is downloaded.
#[test]
fn a_stanza_without_a_filename_is_refused_before_the_pool() {
    let (env, mirror, uri) = private("apt-mirror-no-filename", None, None);
    let package = deb(V1);
    env.host.publish(&uri, package.clone());
    env.host.each_entry(|entry| entry.filename = None);
    let (result, _) = env.stage(&order(&package, &["mdm.service"]));
    let failure = result.expect_err("refused");
    assert_failure(&failure, Reason::ManifestInvalid, Class::Release, Failed);
    assert_eq!(mirror.calls().len(), 1, "only the index was asked");
    assert!(!env.has_run("apt-get download"));
}

/// Proves a mirrored entry on a host with no mirror port is `fetch_failed`, deferred, before any
/// tool runs.
#[test]
fn a_mirrored_repository_without_a_mirror_port_defers() {
    let (mut env, _, uri) = private("apt-mirror-none", None, None);
    env.mirror = None;
    let package = deb(V1);
    env.host.publish(&uri, package.clone());
    let (result, _) = env.stage(&order(&package, &["mdm.service"]));
    let failure = result.expect_err("refused");
    assert_failure(&failure, Reason::FetchFailed, Environment, Deferred);
    assert!(env.host.labels().is_empty());
}

/// Proves the mirror's repository is read off the entry's URI: one directory of the mirror root
/// gives its name (the entry's own name is its channel); an `https` URI, another directory, the
/// root itself and a path below a directory of it give none.
#[test]
fn the_mirrored_name_is_the_directory_of_the_mirror_the_uri_names() {
    let (env, _, _) = private("apt-mirror-name", None, None);
    let root = &env.dirs.mirror;
    let at = |uri: String| {
        let entry = Repository {
            uri,
            ..env.repositories.get(REPO).expect("the entry").clone()
        };
        mirrored_name(&entry, root).map(str::to_owned)
    };
    let base = format!("file:{}", root.display());
    assert_eq!(at(format!("{base}/acme")), Some("acme".to_owned()));
    assert_eq!(at(format!("{base}/")), None);
    assert_eq!(at(format!("{base}/acme/deeper")), None);
    assert_eq!(at("file:/elsewhere/acme".to_owned()), None);
    assert_eq!(at(URI.to_owned()), None);
}
