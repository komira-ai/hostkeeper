//! Shared by the package store's tests: a fixture package (a tar written header by header inside a
//! zstd frame of raw blocks, which the unpack's decoder reads), the release index and the
//! attestation bundle that go with it, a fetcher that serves them, a verifier that answers as it
//! is told, and a scratch directory that can be removed after an unpack made it read-only.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::real_packages::RealPackages;
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_package::ports::{
    Attest, Attested, Fetcher, Got, Limit, NetError, Sink, Url, Verifier, VerifyError,
};
use hostkeeper_package::{Platform, index_url, package_url};
use hostkeeper_unpack::Owner;
use sha2::{Digest, Sha256};

/// The service of the fixture package.
pub const SERVICE: &str = "tierstore";
/// Its version.
pub const VERSION: &str = "1.5.0";

/// A daemon config every rule accepts.
const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// A manifest the loader takes for tierstore 1.5.0, once `{platform}` is the host's platform
/// ([`manifest`]).
const MANIFEST: &str = "schema = 1
name = \"tierstore\"
version = \"1.5.0\"
platform = \"{platform}\"
min_hostkeeper = \"0.1.0\"
[hook]
command = [\"bin/x\", \"update-hook\"]
verify_deadline_secs = 2
[hook.timeout_secs]
prepare = 5
cutover = 1
verify = 1
rollback = 1
cleanup = 1
[[files]]
source = \"files/c.conf\"
path = \"/etc/tierstore/c.conf\"
";

/// A commit the bundle names.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A directory under the target directory, unique to the test. Removed when dropped, the
/// read-only trees an unpack makes included.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let dir = hostkeeper_testkit::Scratch::under(base, name).expect("make the scratch dir");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("set its mode");
        Scratch(dir)
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        open_up(self.path());
    }
}

/// Gives every directory below `path` mode 0700 so the tree can be removed.
fn open_up(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap_or_default();
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            open_up(&entry.path());
        }
    }
}

/// The running user: what the unpack gives the tree to.
pub fn me() -> Owner {
    Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// An octal field of `width` bytes: digits, then NUL.
fn octal(value: u64, width: usize) -> Vec<u8> {
    let mut out = format!("{value:0w$o}", w = width - 1).into_bytes();
    out.push(0);
    out
}

/// A tar archive built header by header.
#[derive(Default)]
struct Tar {
    bytes: Vec<u8>,
}

impl Tar {
    /// An entry: a ustar header, then `data` padded to the block.
    fn entry(&mut self, name: &str, flag: u8, mode: u64, data: &[u8]) -> &mut Tar {
        let mut block = [0u8; 512];
        block[..name.len()].copy_from_slice(name.as_bytes());
        block[100..108].copy_from_slice(&octal(mode, 8));
        block[108..116].copy_from_slice(&octal(1000, 8));
        block[116..124].copy_from_slice(&octal(1000, 8));
        block[124..136].copy_from_slice(&octal(data.len() as u64, 12));
        block[136..148].copy_from_slice(&octal(1_800_000_000, 12));
        block[156] = flag;
        block[257..265].copy_from_slice(b"ustar\x0000");
        block[148..156].copy_from_slice(b"        ");
        let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
        let mut field = format!("{sum:06o}").into_bytes();
        field.extend_from_slice(b"\0 ");
        block[148..156].copy_from_slice(&field);
        self.bytes.extend_from_slice(&block);
        self.bytes.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        self.bytes.extend(std::iter::repeat_n(0u8, pad));
        self
    }

    fn file(&mut self, name: &str, mode: u64, data: &[u8]) -> &mut Tar {
        self.entry(name, b'0', mode, data)
    }

    fn dir(&mut self, name: &str) -> &mut Tar {
        self.entry(&format!("{name}/"), b'5', 0o755, &[])
    }

    /// The end-of-archive marker: two zero blocks.
    fn end(&mut self) -> &mut Tar {
        self.bytes.extend_from_slice(&[0u8; 1024]);
        self
    }
}

/// A block header: last flag, raw type, size.
fn block_header(last: bool, size: u32) -> [u8; 3] {
    let bytes = ((size << 3) | u32::from(last)).to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// One zstd frame of a 1 MiB window holding `data` in raw blocks.
fn zstd(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 10 << 3];
    for chunk in data.chunks(128 * 1024) {
        out.extend_from_slice(&block_header(false, chunk.len() as u32));
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&block_header(true, 0));
    out
}

/// [`MANIFEST`] for the platform of the host the daemon runs on, the one it asks for.
fn manifest() -> String {
    MANIFEST.replace(
        "{platform}",
        hostkeeper_config::platform::Platform::current().as_str(),
    )
}

/// The archive of a valid package: the manifest, `bin/x`, a unit and `files/c.conf`.
pub fn valid_archive() -> Vec<u8> {
    let mut tar = Tar::default();
    tar.file("manifest.toml", 0o644, manifest().as_bytes())
        .dir("bin")
        .file("bin/x", 0o755, b"#!x\n")
        .dir("units")
        .file("units/tierstore.service", 0o644, b"[Service]\n")
        .dir("files")
        .file("files/c.conf", 0o640, b"c\n")
        .end();
    zstd(&tar.bytes)
}

/// One DER element.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend([0x81, len as u8]);
    } else {
        out.push(0x82);
        out.extend((len as u16).to_be_bytes());
    }
    out.extend_from_slice(content);
    out
}

/// A certificate holding only the Fulcio extension that names the ref: all the bundle reader
/// looks at, because the verifier is a fake.
fn certificate(source_ref: &str) -> Vec<u8> {
    let mut oid = vec![0x2b, 0x06, 0x01, 0x04, 0x01, 0x83, 0xbf, 0x30, 0x01];
    oid.push(14);
    let mut extension = tlv(0x06, &oid);
    extension.extend(tlv(0x04, &tlv(0x0c, source_ref.as_bytes())));
    let mut tbs = tlv(0xa0, &tlv(0x02, &[2]));
    tbs.extend(tlv(0x02, &[1]));
    tbs.extend(tlv(0xa3, &tlv(0x30, &tlv(0x30, &extension))));
    tlv(0x30, &tlv(0x30, &tbs))
}

/// The attestation bundle of an archive with SHA-256 `sha`.
fn bundle(sha: &str) -> String {
    let statement = format!(
        "{{\"_type\":\"https://in-toto.io/Statement/v1\",\"subject\":[{{\"name\":\"a\",\
         \"digest\":{{\"sha256\":\"{sha}\"}}}}],\"predicateType\":\"https://slsa.dev/provenance/v1\",\
         \"predicate\":{{\"buildDefinition\":{{\"resolvedDependencies\":[{{\"uri\":\
         \"git+https://github.com/o/r@refs/tags/v1\",\"digest\":{{\"gitCommit\":\"{COMMIT}\"}}}}]}}}}}}"
    );
    format!(
        "{{\"mediaType\":\"application/vnd.dev.sigstore.bundle.v0.3+json\",\
         \"verificationMaterial\":{{\"certificate\":{{\"rawBytes\":\"{}\"}}}},\
         \"dsseEnvelope\":{{\"payload\":\"{}\",\"payloadType\":\"application/vnd.in-toto+json\",\
         \"signatures\":[]}}}}\n",
        STANDARD.encode(certificate("refs/tags/v1")),
        STANDARD.encode(statement)
    )
}

/// The release index of `archive` for the host's platform.
pub fn index(archive: &[u8]) -> String {
    let platform = Platform::current().name();
    format!(
        "{{\"packages\":[{{\"bytes\":{},\"file\":\"{SERVICE}-{VERSION}-{platform}.tar.zst\",\
         \"platform\":\"{platform}\",\"sha256\":\"{}\"}}],\"schema\":1,\"service\":\"{SERVICE}\",\
         \"version\":\"{VERSION}\"}}",
        archive.len(),
        sha256_hex(archive)
    )
}

/// What a fake was asked, in order.
pub type Asked = Arc<Mutex<Vec<String>>>;

/// A fetcher that serves fixed bodies by URL and answers 404 for any other.
pub struct FakeFetcher {
    routes: Vec<(String, Vec<u8>)>,
    asked: Asked,
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

/// How a fake verifier answers.
#[derive(Clone, Copy, Debug)]
pub enum Answer {
    Confirms,
    Rejects,
    Unavailable,
}

/// A verifier that records the SHA-256 it was asked about and answers as it is told.
pub struct FakeVerifier {
    answer: Answer,
    asked: Asked,
}

impl Verifier for FakeVerifier {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        self.asked
            .lock()
            .expect("the log")
            .push(request.sha256.as_str().to_owned());
        match self.answer {
            Answer::Confirms => Ok(Attested {
                commit: request.commit.clone(),
                source_ref: request.source_ref.clone(),
            }),
            Answer::Rejects => Err(VerifyError::Rejected("not the signer".to_owned())),
            Answer::Unavailable => Err(VerifyError::Unavailable),
        }
    }
}

/// A packages directory in a scratch directory, a daemon config, and what the fakes were asked.
pub struct Rig {
    pub scratch: Scratch,
    pub packages: PathBuf,
    pub config: DaemonConfig,
    pub fetched: Asked,
    pub verified: Asked,
}

impl Rig {
    pub fn new(name: &str) -> Rig {
        let scratch = Scratch::new(name);
        let packages = scratch.path().join("packages");
        fs::create_dir(&packages).expect("make the packages dir");
        fs::set_permissions(&packages, fs::Permissions::from_mode(0o755)).expect("set its mode");
        Rig {
            scratch,
            packages,
            config: DaemonConfig::parse(CONFIG).expect("the config"),
            fetched: Asked::default(),
            verified: Asked::default(),
        }
    }

    /// The fixture service.
    pub fn service() -> ServiceName {
        ServiceName::new(SERVICE).expect("a service name")
    }

    /// The fixture version.
    pub fn version() -> Version {
        Version::new(VERSION).expect("a version")
    }

    /// The URLs of the index, the package and the bundle of the fixture, in the order they are
    /// fetched.
    pub fn urls(&self) -> Vec<String> {
        let channel = &self.config.release_channel;
        let (service, version) = (Rig::service(), Rig::version());
        let package = package_url(channel, &service, &version, Platform::current());
        vec![
            index_url(channel, &service, &version).as_str().to_owned(),
            package.as_str().to_owned(),
            format!("{}.sigstore.json", package.as_str()),
        ]
    }

    /// A store over `packages` that serves `archive` with its index and a bundle that lists its
    /// digest, and whose verifier answers `answer`.
    pub fn store(&self, archive: &[u8], answer: Answer) -> RealPackages {
        let urls = self.urls();
        let bodies = [
            index(archive).into_bytes(),
            archive.to_vec(),
            bundle(&sha256_hex(archive)).into_bytes(),
        ];
        self.open(urls.into_iter().zip(bodies).collect(), answer)
            .expect("open the store")
    }

    /// A store over `packages` that serves `routes` and whose verifier answers `answer`.
    pub fn open(&self, routes: Vec<(String, Vec<u8>)>, answer: Answer) -> io::Result<RealPackages> {
        let fetcher = FakeFetcher {
            routes,
            asked: Arc::clone(&self.fetched),
        };
        let verifier = FakeVerifier {
            answer,
            asked: Arc::clone(&self.verified),
        };
        RealPackages::open(
            Box::new(fetcher),
            Box::new(verifier),
            FakeClock::new(),
            &self.config,
            &self.packages,
            me(),
        )
    }
}
