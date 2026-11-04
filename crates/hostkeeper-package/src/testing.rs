//! What the tests of this crate share: a clock that moves only when told, a scripted
//! [`Fetcher`], a recording [`Verifier`], a scratch directory, daemon config values, and the
//! generator of the synthetic attestation bundles under `tests/fixtures/`.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::daemon::{DaemonConfig, Sha256Hex, TrustedSigners};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{Clock, Stamp};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

use crate::der;
use crate::fetch::PackageOrder;
use crate::index::Platform;
use crate::ports::{
    Attest, Attested, CommitId, Fetcher, Got, Limit, NetError, Sink, SourceRef, Url, Verifier,
    VerifyError,
};

/// The boot id of every [`FakeClock`] until a test changes it.
pub(crate) const BOOT: &str = "boot-1";

/// A clock that moves only when a test or a sleep moves it. `sleep_secs` adds to it; with
/// `yields` it also gives the processor away for a moment first, so that a loop polling a child
/// process lets the child run (nothing is measured by it).
#[derive(Debug)]
pub(crate) struct FakeClock {
    secs: AtomicU64,
    boot: Mutex<String>,
    yields: bool,
    sleeps: AtomicU64,
}

impl FakeClock {
    pub(crate) fn at(secs: u64) -> FakeClock {
        FakeClock {
            secs: AtomicU64::new(secs),
            boot: Mutex::new(BOOT.to_owned()),
            yields: false,
            sleeps: AtomicU64::new(0),
        }
    }

    /// A clock whose sleeps also yield the processor.
    pub(crate) fn yielding(secs: u64) -> FakeClock {
        FakeClock {
            yields: true,
            ..FakeClock::at(secs)
        }
    }

    pub(crate) fn advance(&self, secs: u64) {
        self.secs.fetch_add(secs, Ordering::SeqCst);
    }

    pub(crate) fn reboot(&self, boot: &str) {
        *self.boot.lock().expect("the boot id") = boot.to_owned();
    }

    /// How many times `sleep_secs` was called.
    pub(crate) fn sleeps(&self) -> u64 {
        self.sleeps.load(Ordering::SeqCst)
    }

    pub(crate) fn stamp(&self, mono_secs: u64) -> Stamp {
        Stamp {
            boot_id: self.boot_id(),
            mono_secs,
        }
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.secs.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> String {
        self.boot.lock().expect("the boot id").clone()
    }

    fn sleep_secs(&self, secs: u64) {
        self.sleeps.fetch_add(1, Ordering::SeqCst);
        if self.yields {
            std::thread::yield_now();
        }
        self.advance(secs);
    }
}

/// A scratch directory for one test, beside the test binary in the target directory.
pub(crate) fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe.parent().expect("its directory").join("package-scratch");
    Scratch::under(&base, name).expect("a scratch directory")
}

/// The lower-case hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(&*Sha256::digest(bytes))
}

/// The trusted signer of the tests' configs.
pub(crate) fn signers(organisation: &str, workflow: &str, refs: &[&str]) -> TrustedSigners {
    TrustedSigners {
        organisation: organisation.to_owned(),
        workflow: workflow.to_owned(),
        refs: refs.iter().map(|r| (*r).to_owned()).collect(),
        hosted_runner: true,
    }
}

/// A daemon config with `channel` as its release channel and `ca` as its CA digest, read by the
/// real loader: the only way to make its `HttpsUrl` and `Sha256Hex` values.
pub(crate) fn config(channel: &str, ca: &str) -> DaemonConfig {
    let text = format!(
        "coordinator = \"https://coordinator.example:8443\"\n\
         ca_sha256 = \"{ca}\"\n\
         release_channel = \"{channel}\"\n\
         [trusted_signers]\n\
         organisation = \"example-org\"\n\
         workflow = \".github/workflows/artifacts.yml\"\n\
         refs = [\"refs/heads/main\"]\n\
         hosted_runner = true\n"
    );
    DaemonConfig::parse(&text).expect("a test config")
}

/// `hex` as a [`Sha256Hex`].
pub(crate) fn digest(hex: &str) -> Sha256Hex {
    Sha256Hex::parse(hex).expect("a SHA-256")
}

/// The order of `service` at `version`.
pub(crate) fn order(service: &str, version: &str) -> PackageOrder {
    PackageOrder {
        service: ServiceName::new(service).expect("a service name"),
        version: Version::new(version).expect("a version"),
    }
}

/// The platform the tests' hosts run on.
pub(crate) const HOST: Platform = Platform::LinuxX86_64;
/// The release channel of the tests.
pub(crate) const CHANNEL: &str = "https://releases.example";

/// The canonical release index of `service` at `version` with one package per `(platform,
/// bytes)`, given in platform order: keys sorted, no white space.
pub(crate) fn index_json(service: &str, version: &str, packages: &[(Platform, &[u8])]) -> String {
    let entries: Vec<String> = packages
        .iter()
        .map(|(platform, bytes)| {
            format!(
                "{{\"bytes\":{},\"file\":\"{service}-{version}-{}.tar.zst\",\"platform\":\"{}\",\"sha256\":\"{}\"}}",
                bytes.len(),
                platform.name(),
                platform.name(),
                sha256_hex(bytes)
            )
        })
        .collect();
    format!(
        "{{\"packages\":[{}],\"schema\":1,\"service\":\"{service}\",\"version\":\"{version}\"}}",
        entries.join(",")
    )
}

/// The URL of the release index of `service` at `version` on [`CHANNEL`].
pub(crate) fn index_at(service: &str, version: &str) -> String {
    format!("{CHANNEL}/{service}/{version}/index.json")
}

/// The URL of the package of `service` at `version` for [`HOST`] on [`CHANNEL`].
pub(crate) fn package_at(service: &str, version: &str) -> String {
    format!(
        "{CHANNEL}/{service}/{version}/{service}-{version}-{}.tar.zst",
        HOST.name()
    )
}

/// A fake channel that serves `service` at `version`: an index naming `archive` for [`HOST`],
/// and the archive.
pub(crate) fn published(service: &str, version: &str, archive: &[u8]) -> FakeFetcher {
    FakeFetcher::new()
        .route(
            &index_at(service, version),
            serve(index_json(service, version, &[(HOST, archive)]).as_bytes()),
        )
        .route(&package_at(service, version), serve(archive))
}

/// How the fake fetcher answers one URL.
pub(crate) type Serve = Box<dyn Fn(&mut dyn Sink, &Limit) -> Result<Got, NetError>>;

/// One request the fake fetcher saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub url: String,
    pub limit: Limit,
}

/// A [`Fetcher`] that answers each URL as a test says, records every request, and answers a URL
/// it was not told about with a 404.
pub(crate) struct FakeFetcher {
    routes: Vec<(String, Serve)>,
    calls: RefCell<Vec<Call>>,
}

impl FakeFetcher {
    pub(crate) fn new() -> FakeFetcher {
        FakeFetcher {
            routes: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn route(mut self, url: &str, serve: Serve) -> FakeFetcher {
        self.routes.push((url.to_owned(), serve));
        self
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

impl Fetcher for FakeFetcher {
    fn get(&self, url: &Url, sink: &mut dyn Sink, limit: Limit) -> Result<Got, NetError> {
        self.calls.borrow_mut().push(Call {
            url: url.as_str().to_owned(),
            limit: limit.clone(),
        });
        let serve = self
            .routes
            .iter()
            .find(|(route, _)| route == url.as_str())
            .map(|(_, serve)| serve);
        match serve {
            Some(serve) => serve(sink, &limit),
            None => Err(NetError::Status(404)),
        }
    }
}

/// Gives `body` to `sink` as a well-behaved adapter does: the length (when `announce`), then
/// chunks of `chunk` bytes, stopping when the sink says so. Returns how many bytes it gave.
pub(crate) fn deliver(
    sink: &mut dyn Sink,
    body: &[u8],
    announce: bool,
    chunk: usize,
) -> (Got, u64) {
    let length = announce.then(|| u64::try_from(body.len()).expect("a length"));
    if sink.begin(length).is_err() {
        return (Got::Stopped, 0);
    }
    let mut given = 0;
    for piece in body.chunks(chunk) {
        given += u64::try_from(piece.len()).expect("a length");
        if sink.chunk(piece).is_err() {
            return (Got::Stopped, given);
        }
    }
    (Got::Complete, given)
}

/// Serves `body`, announced, in chunks of 4 bytes.
pub(crate) fn serve(body: &[u8]) -> Serve {
    let body = body.to_vec();
    Box::new(move |sink: &mut dyn Sink, _: &Limit| Ok(deliver(sink, &body, true, 4).0))
}

/// What the fake verifier saw of one request, and whether both files existed during it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub archive: PathBuf,
    pub bundle: PathBuf,
    pub sha256: String,
    pub commit: String,
    pub source_ref: String,
    pub signers: TrustedSigners,
    pub deadline: Stamp,
    pub files_existed: bool,
}

/// A [`Verifier`] that records each request and gives one answer.
pub(crate) struct FakeVerifier {
    answer: Result<(), VerifyError>,
    seen: RefCell<Vec<Seen>>,
}

impl FakeVerifier {
    pub(crate) fn answering(answer: Result<(), VerifyError>) -> FakeVerifier {
        FakeVerifier {
            answer,
            seen: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.borrow().clone()
    }
}

impl Verifier for FakeVerifier {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        self.seen.borrow_mut().push(Seen {
            archive: request.archive.to_owned(),
            bundle: request.bundle.to_owned(),
            sha256: request.sha256.as_str().to_owned(),
            commit: request.commit.as_str().to_owned(),
            source_ref: request.source_ref.as_str().to_owned(),
            signers: request.signers.clone(),
            deadline: request.deadline.clone(),
            files_existed: request.archive.is_file() & request.bundle.is_file(),
        });
        self.answer.clone().map(|()| Attested {
            commit: request.commit.clone(),
            source_ref: request.source_ref.clone(),
        })
    }
}

/// What a synthetic attestation bundle says.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BundleSpec<'a> {
    pub organisation: &'a str,
    pub repository: &'a str,
    pub workflow: &'a str,
    pub source_ref: &'a str,
    /// The commit in the certificate.
    pub certificate_commit: &'a str,
    /// The commit in the provenance.
    pub provenance_commit: &'a str,
    pub runner: &'a str,
    pub subject_name: &'a str,
    pub subject_sha256: &'a str,
}

/// The Fulcio extensions a synthetic certificate carries, and the fake `gh` reads.
pub(crate) const BUILD_SIGNER_URI: u8 = 9;
pub(crate) const RUNNER_ENVIRONMENT: u8 = 11;
pub(crate) const SOURCE_REPOSITORY_DIGEST: u8 = 13;
pub(crate) const SOURCE_REPOSITORY_OWNER_URI: u8 = 16;

/// A DER element: `tag`, the length (short form, or long form of one or two bytes), `content`.
pub(crate) fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = content.len();
    let mut out = vec![tag];
    if length < 0x80 {
        out.push(u8::try_from(length).expect("short"));
    } else if length < 0x100 {
        out.extend([0x81, u8::try_from(length).expect("one byte")]);
    } else {
        let length = u16::try_from(length).expect("two bytes");
        out.push(0x82);
        out.extend(length.to_be_bytes());
    }
    out.extend_from_slice(content);
    out
}

/// One Fulcio extension whose value is the UTF8String `value`.
pub(crate) fn extension(arc: u8, value: &str) -> Vec<u8> {
    let mut body = tlv(0x06, &der::fulcio(arc));
    body.extend(tlv(0x04, &tlv(0x0c, value.as_bytes())));
    tlv(0x30, &body)
}

/// A certificate with only what this crate and the fake `gh` read: a `tbsCertificate` with a
/// version, a serial number and the five Fulcio extensions of `spec`. No key, no signature.
pub(crate) fn certificate(spec: &BundleSpec<'_>) -> Vec<u8> {
    let signer = format!(
        "https://github.com/{}/{}/{}@{}",
        spec.organisation, spec.repository, spec.workflow, spec.source_ref
    );
    let owner = format!("https://github.com/{}", spec.organisation);
    let mut extensions = Vec::new();
    for (arc, value) in [
        (BUILD_SIGNER_URI, signer.as_str()),
        (RUNNER_ENVIRONMENT, spec.runner),
        (SOURCE_REPOSITORY_DIGEST, spec.certificate_commit),
        (der::SOURCE_REPOSITORY_REF, spec.source_ref),
        (SOURCE_REPOSITORY_OWNER_URI, owner.as_str()),
    ] {
        extensions.extend(extension(arc, value));
    }
    let mut tbs = tlv(0xa0, &tlv(0x02, &[0x02]));
    tbs.extend(tlv(0x02, &[0x01]));
    tbs.extend(tlv(0xa3, &tlv(0x30, &extensions)));
    tlv(0x30, &tlv(0x30, &tbs))
}

/// The in-toto statement of `spec`: one subject, SLSA provenance naming one commit.
pub(crate) fn statement(spec: &BundleSpec<'_>) -> String {
    format!(
        "{{\"_type\":\"https://in-toto.io/Statement/v1\",\
         \"subject\":[{{\"name\":\"{}\",\"digest\":{{\"sha256\":\"{}\"}}}}],\
         \"predicateType\":\"https://slsa.dev/provenance/v1\",\
         \"predicate\":{{\"buildDefinition\":{{\"resolvedDependencies\":\
         [{{\"uri\":\"git+https://github.com/{}/{}@{}\",\"digest\":{{\"gitCommit\":\"{}\"}}}}]}}}}}}",
        spec.subject_name,
        spec.subject_sha256,
        spec.organisation,
        spec.repository,
        spec.source_ref,
        spec.provenance_commit
    )
}

/// The bundle of `spec`, as the files under `tests/fixtures/` hold it: one line of JSON.
pub(crate) fn bundle_json(spec: &BundleSpec<'_>) -> String {
    format!(
        "{{\"mediaType\":\"application/vnd.dev.sigstore.bundle.v0.3+json\",\
         \"verificationMaterial\":{{\"certificate\":{{\"rawBytes\":\"{}\"}}}},\
         \"dsseEnvelope\":{{\"payload\":\"{}\",\
         \"payloadType\":\"application/vnd.in-toto+json\",\"signatures\":[]}}}}\n",
        STANDARD.encode(certificate(spec)),
        STANDARD.encode(statement(spec))
    )
}

/// The commit and ref the tests' bundles name when nothing else is said.
pub(crate) fn commit_id(text: &str) -> CommitId {
    CommitId::new(text).expect("a commit")
}

pub(crate) fn source_ref(text: &str) -> SourceRef {
    SourceRef::new(text).expect("a ref")
}

/// The files directly in `dir`, by name, sorted.
pub(crate) fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("list the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// A writer whose every write fails with its error kind: a disk that is full, read-only or
/// refuses the writer.
pub(crate) struct Failing(pub io::ErrorKind);

impl Write for Failing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(self.0))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The staging file's writer on a full disk (`ENOSPC`).
pub(crate) fn full(_: &File) -> Box<dyn Write + '_> {
    Box::new(Failing(io::ErrorKind::StorageFull))
}
