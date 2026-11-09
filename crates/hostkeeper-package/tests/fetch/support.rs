//! What the tests of `fetch.rs` share: the fake clock, network and verifier, a host with a real
//! `stores.toml`, and the entries and bundles of the rows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::daemon::{Bounds, TrustedSigners};
use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_package::entry::{EntryEnv, Proven, fetch_entry};
use hostkeeper_package::ports::{
    Attest, Attested, Fetcher, Got, Limit, NetError, Sink, Url, Verifier, VerifyError,
};
use hostkeeper_package::stores::{self, Stores};
use hostkeeper_package::{PackageFailure, Platform};
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Target};
use hostkeeper_steps::{BootId, BootIdError, Clock};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

pub const ARCHIVE: &[u8] = b"the archive of svc 1.0.0 bytes";
pub const WRONG: &[u8] = b"the archive of svc 6.6.6 bytes";
pub const GOOD_BUNDLE: &[u8] = include_bytes!("../fixtures/good.sigstore.json");

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(&*Sha256::digest(bytes))
}

/// The good fixture bundle with its statement's subjects replaced by `digests`: nothing here is
/// signed, and the verifier is a fake, so only what the crate reads of it matters.
pub fn bundle_listing(digests: &[&str]) -> Vec<u8> {
    let mut bundle: serde_json::Value = serde_json::from_slice(GOOD_BUNDLE).expect("a bundle");
    let payload = bundle
        .pointer("/dsseEnvelope/payload")
        .and_then(|v| v.as_str())
        .expect("a payload");
    let mut statement: serde_json::Value =
        serde_json::from_slice(&STANDARD.decode(payload).expect("base64")).expect("a statement");
    let subjects: Vec<serde_json::Value> = digests
        .iter()
        .map(|d| serde_json::json!({"name": "svc", "digest": {"sha256": d}}))
        .collect();
    statement["subject"] = subjects.into();
    let encoded = STANDARD.encode(serde_json::to_vec(&statement).expect("a statement"));
    bundle["dsseEnvelope"]["payload"] = encoded.into();
    serde_json::to_vec(&bundle).expect("a bundle")
}

/// A clock that moves only when a test moves it.
pub struct FakeClock(Mutex<u64>);

impl FakeClock {
    pub fn advance(&self, secs: u64) {
        *self.0.lock().expect("the clock") += secs;
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        *self.0.lock().expect("the clock")
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test clock", "boot-1")
    }

    fn sleep_secs(&self, secs: u64) {
        self.advance(secs);
    }
}

/// What the fake network answers for a URL.
#[derive(Clone)]
pub enum Serve {
    /// 200, announced, in chunks of 7 bytes.
    Bytes(Vec<u8>),
    /// 200, not announced, in chunks of 7 bytes.
    Unannounced(Vec<u8>),
    /// A status other than 2xx.
    Status(u16),
    /// Takes more than any deadline: moves the clock past it, then sends a chunk.
    Hang,
}

/// Answers by URL (404 for any other) and records every request with its `Authorization`.
pub struct FakeFetcher {
    pub routes: HashMap<String, Serve>,
    pub asked: RefCell<Vec<(String, Option<String>)>>,
    pub clock: Arc<FakeClock>,
}

impl FakeFetcher {
    pub fn new(clock: &Arc<FakeClock>) -> FakeFetcher {
        FakeFetcher {
            routes: HashMap::new(),
            asked: RefCell::new(Vec::new()),
            clock: Arc::clone(clock),
        }
    }

    pub fn route(mut self, url: &str, serve: Serve) -> FakeFetcher {
        self.routes.insert(url.to_owned(), serve);
        self
    }

    pub fn urls(&self) -> Vec<String> {
        self.asked
            .borrow()
            .iter()
            .map(|(url, _)| url.clone())
            .collect()
    }

    pub fn answer(&self, url: &Url, sink: &mut dyn Sink) -> Result<Got, NetError> {
        let serve = self.routes.get(url.as_str()).cloned();
        let (body, announce) = match serve {
            None => return Err(NetError::Status(404)),
            Some(Serve::Status(status)) => return Err(NetError::Status(status)),
            Some(Serve::Hang) => {
                self.clock.advance(100_000);
                return match sink.chunk(b"late") {
                    Ok(()) => Ok(Got::Complete),
                    Err(_) => Ok(Got::Stopped),
                };
            }
            Some(Serve::Bytes(body)) => (body, true),
            Some(Serve::Unannounced(body)) => (body, false),
        };
        let length = announce.then(|| u64::try_from(body.len()).expect("a length"));
        if sink.begin(length).is_err() {
            return Ok(Got::Stopped);
        }
        for piece in body.chunks(7) {
            if sink.chunk(piece).is_err() {
                return Ok(Got::Stopped);
            }
        }
        Ok(Got::Complete)
    }
}

impl Fetcher for FakeFetcher {
    fn get(&self, url: &Url, sink: &mut dyn Sink, _limit: Limit) -> Result<Got, NetError> {
        self.asked
            .borrow_mut()
            .push((url.as_str().to_owned(), None));
        self.answer(url, sink)
    }

    fn get_request(
        &self,
        request: &hostkeeper_package::ports::Request<'_>,
        sink: &mut dyn Sink,
        _limit: Limit,
    ) -> Result<Got, NetError> {
        self.asked.borrow_mut().push((
            request.url.as_str().to_owned(),
            request.authorization.map(str::to_owned),
        ));
        self.answer(request.url, sink)
    }
}

/// A verifier that records the digests it was asked about and gives one answer.
pub struct FakeVerifier {
    pub answer: Result<(), VerifyError>,
    pub asked: RefCell<Vec<(String, PathBuf, PathBuf)>>,
    pub files_existed: Cell<bool>,
}

impl FakeVerifier {
    pub fn accepting() -> FakeVerifier {
        FakeVerifier::answering(Ok(()))
    }

    pub fn answering(answer: Result<(), VerifyError>) -> FakeVerifier {
        FakeVerifier {
            answer,
            asked: RefCell::new(Vec::new()),
            files_existed: Cell::new(false),
        }
    }
}

impl Verifier for FakeVerifier {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        self.asked.borrow_mut().push((
            request.sha256.as_str().to_owned(),
            request.archive.to_owned(),
            request.bundle.to_owned(),
        ));
        self.files_existed
            .set(request.archive.is_file() & request.bundle.is_file());
        self.answer.clone().map(|()| Attested {
            commit: request.commit.clone(),
            source_ref: request.source_ref.clone(),
        })
    }
}

/// A host: a scratch directory with the packages root, the stores file read from it, the bounds
/// (a package of at most 64 bytes, 50 s per artifact source, 20 s per bundle source and proof).
pub struct Host {
    pub dir: Scratch,
    pub clock: Arc<FakeClock>,
    pub stores: Stores,
    pub bounds: Bounds,
    pub signers: TrustedSigners,
}

pub fn me(dir: &Path) -> u32 {
    fs::metadata(dir).expect("the directory").uid()
}

impl Host {
    /// A host whose `stores.toml` is `text`, with `{dir}` standing for its scratch directory.
    pub fn with_stores(name: &str, text: &str) -> Host {
        let exe = std::env::current_exe().expect("the test binary");
        let base = exe.parent().expect("its directory").join("fetch-scratch");
        let dir = Scratch::under(&base, name).expect("a scratch directory");
        fs::create_dir(dir.path().join("packages")).expect("the packages root");
        fs::write(dir.path().join("lan.basic"), "user:secret\n").expect("a credential");
        fs::set_permissions(
            dir.path().join("lan.basic"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("its mode");
        let file = dir.path().join("stores.toml");
        fs::write(
            &file,
            text.replace("{dir}", &dir.path().display().to_string()),
        )
        .expect("the stores file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("its mode");
        let stores = stores::load(&file, me(dir.path())).expect("stores");
        Host {
            dir,
            clock: Arc::new(FakeClock(Mutex::new(1))),
            stores,
            bounds: Bounds {
                max_package_bytes: 64,
                fetch_timeout_secs: 50,
                attestation_timeout_secs: 20,
                ..Bounds::default()
            },
            signers: TrustedSigners {
                organisation: "example-org".to_owned(),
                workflow: ".github/workflows/artifacts.yml".to_owned(),
                refs: vec!["refs/heads/main".to_owned()],
                hosted_runner: true,
            },
        }
    }

    /// The stores of most rows: two oci mirrors `m1` and `m2` (when `mirrors`), the https stores
    /// `a` and `b`, and the oci store `lan` with a basic credential.
    pub fn new(name: &str, mirrors: bool) -> Host {
        let mirror = |n: &str| {
            format!(
                "[[store]]\nname = \"{n}\"\nkind = \"oci\"\nbase = \"https://{n}.example/\"\nmirror = {mirrors}\nrepository = \"rel\"\n"
            )
        };
        let text = format!(
            "{}{}[[store]]\nname = \"a\"\nkind = \"https\"\nbase = \"https://a.example/\"\n\
             [[store]]\nname = \"b\"\nkind = \"https\"\nbase = \"https://b.example/\"\nredirects = 1\n\
             [[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://lan.example/\"\n\
             basic_auth_file = \"{{dir}}/lan.basic\"\n",
            mirror("m1"),
            mirror("m2")
        );
        Host::with_stores(name, &text)
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().join("packages")
    }

    pub fn service(&self) -> ServiceName {
        ServiceName::new("svc").expect("a service name")
    }

    /// The staging files in the service's directory.
    pub fn staged(&self) -> Vec<String> {
        let dir = self.root().join("svc");
        let mut names: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| {
                        e.expect("an entry")
                            .file_name()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn run(
        &self,
        fetcher: &dyn Fetcher,
        verifier: &FakeVerifier,
        entry: &Target,
    ) -> Result<Proven, PackageFailure> {
        let env = EntryEnv {
            fetcher,
            verifier,
            clock: &*self.clock,
            bounds: &self.bounds,
            stores: &self.stores,
            packages_root: &self.root(),
            signers: &self.signers,
            platform: Platform::LinuxX86_64,
        };
        fetch_entry(&env, &self.service(), entry)
    }
}

pub fn failure(outcome: Outcome, class: Class, reason: &'static str) -> PackageFailure {
    PackageFailure {
        outcome,
        class,
        reason,
    }
}

pub fn deferred(reason: &'static str) -> PackageFailure {
    failure(Outcome::Deferred, Class::Environment, reason)
}

pub fn release(reason: &'static str) -> PackageFailure {
    failure(Outcome::Failure, Class::Release, reason)
}

pub fn locations(list: &[(&str, &str)]) -> Vec<Location> {
    list.iter()
        .map(|(store, locator)| Location {
            store: (*store).to_owned(),
            locator: (*locator).to_owned(),
        })
        .collect()
}

pub const ARTIFACT_AT: &str = "svc/1.0.0/svc.tar.zst";
pub const BUNDLE_AT: &str = "svc/1.0.0/svc.tar.zst.sigstore.json";

/// An entry for `archive` attested by `bundle`, each with its locations.
pub fn entry_of(
    archive: &[u8],
    artifact_at: &[(&str, &str)],
    bundle: &[u8],
    bundle_at: &[(&str, &str)],
) -> Target {
    Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: locations(artifact_at),
        digest: sha256(archive),
        bytes: u64::try_from(archive.len()).expect("a length"),
        attestation: Some(Blob {
            digest: sha256(bundle),
            bytes: u64::try_from(bundle.len()).expect("a length"),
            locations: locations(bundle_at),
        }),
        ..Target::default()
    }
}

/// The bundle that lists [`ARCHIVE`]'s digest.
pub fn bundle() -> Vec<u8> {
    bundle_listing(&[&sha256(ARCHIVE)])
}

/// The usual entry: both blobs at store `a`.
pub fn entry() -> Target {
    entry_of(
        ARCHIVE,
        &[("a", ARTIFACT_AT)],
        &bundle(),
        &[("a", BUNDLE_AT)],
    )
}

pub fn artifact_url() -> String {
    format!("https://a.example/{ARTIFACT_AT}")
}

pub fn bundle_url() -> String {
    format!("https://a.example/{BUNDLE_AT}")
}

/// A mirror's URL for the blob whose bytes are `bytes`.
pub fn mirror_url(mirror: &str, bytes: &[u8]) -> String {
    format!(
        "https://{mirror}.example/v2/rel/blobs/sha256:{}",
        sha256(bytes)
    )
}

/// The fetcher of a good host: both blobs at store `a`.
pub fn published(host: &Host) -> FakeFetcher {
    FakeFetcher::new(&host.clock)
        .route(&artifact_url(), Serve::Bytes(ARCHIVE.to_vec()))
        .route(&bundle_url(), Serve::Bytes(bundle()))
}

/// The two blobs of an entry, which a row is run for.
#[derive(Clone, Copy, Debug)]
pub enum Which {
    Artifact,
    Bundle,
}

impl Which {
    pub const BOTH: [Which; 2] = [Which::Artifact, Which::Bundle];

    /// The bytes of the blob.
    pub fn bytes(self) -> Vec<u8> {
        match self {
            Which::Artifact => ARCHIVE.to_vec(),
            Which::Bundle => bundle(),
        }
    }

    pub fn url(self) -> String {
        match self {
            Which::Artifact => artifact_url(),
            Which::Bundle => bundle_url(),
        }
    }

    pub fn mirror_url(self, mirror: &str) -> String {
        mirror_url(mirror, &self.bytes())
    }

    /// The reason a location that gave other bytes is refused with.
    pub fn wrong(self) -> PackageFailure {
        release(match self {
            Which::Artifact => "package_digest_mismatch",
            Which::Bundle => "not_attested",
        })
    }

    /// The reason a blob nobody answered for is deferred with.
    pub fn down(self) -> PackageFailure {
        deferred(match self {
            Which::Artifact => "fetch_failed",
            Which::Bundle => "attestation_unavailable",
        })
    }

    /// The URLs asked for this blob (the fetcher's log holds both blobs').
    pub fn asked(self, fetcher: &FakeFetcher, urls: &[String]) -> Vec<String> {
        let _ = self;
        fetcher
            .urls()
            .into_iter()
            .filter(|u| urls.contains(u))
            .collect()
    }
}

/// Wrong bytes of the same size as the blob's.
pub fn wrong_for(which: Which) -> Vec<u8> {
    let mut bytes = which.bytes();
    bytes[0] ^= 0xff;
    bytes
}

/// The fetcher of a host where every URL in `serving` answers with its bytes, and the other
/// blob is served right from its location `a`.
pub fn with(host: &Host, which: Which, serving: &[(String, Serve)]) -> FakeFetcher {
    let mut fetcher = FakeFetcher::new(&host.clock);
    let other = match which {
        Which::Artifact => (Which::Bundle.url(), Serve::Bytes(Which::Bundle.bytes())),
        Which::Bundle => (Which::Artifact.url(), Serve::Bytes(Which::Artifact.bytes())),
    };
    fetcher = fetcher.route(&other.0, other.1);
    for (url, serve) in serving {
        fetcher = fetcher.route(url, serve.clone());
    }
    fetcher
}
