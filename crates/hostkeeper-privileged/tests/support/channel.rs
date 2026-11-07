//! A release channel on loopback for the install test (`tests/install_uninstall.rs`): the fixture
//! package (the workload fixture as its hook and its unit's main process, a file it places, a
//! unit) in the release layout of `hostkeeper-package`, served over HTTPS by a server of the test,
//! and what the daemon's package check needs of the host.
//!
//! The daemon fixes two things of its package check, with no flag: the CA bundle its HTTPS client
//! trusts (`/etc/ssl/certs/ca-certificates.crt`) and the attestation verifier it runs
//! (`/usr/lib/hostkeeper/gh`, with the Sigstore trusted root beside it). [`HostTrust`] adds the
//! channel's CA to that bundle and puts there, as the verifier, a program that takes every
//! attestation (`true`): a Sigstore attestation of a package built by the test cannot be made
//! without the network, so this is the one part of the path that is not the production one. The
//! attestation bundle the channel serves is still read by the daemon, which checks its subject
//! digest, source commit and ref before it runs the verifier.
//!
//! The network-install test (`tests/net_install.rs`) serves from the same [`Channel`], bound to
//! the address of the test's farm LAN ([`Channel::start_at`]): the fixture package with the
//! bundle of the end-to-end rig's signer ([`Fixture::build_signed`]), which the installed guest's
//! own `gh` checks against the signer's trusted root, and the release `hostkeeper-pxe fetch`
//! downloads, its ISO streamed from the file the job downloaded ([`Channel::serve_file`]).

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use super::pair::{TestCa, put, sha256_hex};

/// The CA bundle the daemon's HTTPS client trusts on a Debian or Ubuntu host.
const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The directory of the daemon's attestation verifier and its trusted root.
pub const VERIFIER_DIR: &str = "/usr/lib/hostkeeper";
/// The program that stands for the verifier: it takes every attestation.
const TRUE: &str = "/usr/bin/true";
/// The commit the attestation names as the package's source.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The channel's CA in the host's CA bundle and the verifier at its path, for as long as it is
/// held; dropping it writes the bundle back as it was. The verifier's directory is one of the
/// paths the test claims ([`super::pair::HostPaths`]), so it is removed with them.
pub struct HostTrust {
    bundle: Vec<u8>,
}

impl HostTrust {
    /// Adds `ca_pem` to the host's CA bundle and puts the verifier and a trusted root in
    /// [`VERIFIER_DIR`], root's and not writable by anyone else, as the daemon's check requires.
    pub fn install(ca_pem: &str) -> HostTrust {
        let bundle =
            fs::read(CA_BUNDLE).unwrap_or_else(|e| panic!("the host's CA bundle {CA_BUNDLE}: {e}"));
        let mut with = bundle.clone();
        with.extend_from_slice(b"\n");
        with.extend_from_slice(ca_pem.as_bytes());
        fs::write(CA_BUNDLE, with).expect("add the channel's CA to the bundle");
        let dir = Path::new(VERIFIER_DIR);
        put(
            &dir.join("gh"),
            &fs::read(TRUE).expect("the program true"),
            0o755,
        );
        put(&dir.join("trusted_root.json"), b"{}\n", 0o644);
        HostTrust { bundle }
    }
}

impl Drop for HostTrust {
    fn drop(&mut self) {
        let _ = fs::write(CA_BUNDLE, &self.bundle);
    }
}

/// The platform of this host, as the release index names it.
fn platform() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "linux-arm64"
    } else {
        "linux-x86_64"
    }
}

/// An octal field of `width` bytes: digits, then NUL.
fn octal(value: u64, width: usize) -> Vec<u8> {
    let mut out = format!("{value:0w$o}", w = width - 1).into_bytes();
    out.push(0);
    out
}

/// A tar archive (ustar) built header by header, as the package format takes it.
#[derive(Default)]
struct Tar {
    bytes: Vec<u8>,
}

impl Tar {
    fn entry(&mut self, name: &str, flag: u8, mode: u64, data: &[u8]) -> &mut Tar {
        let mut block = [0u8; 512];
        block[..name.len()].copy_from_slice(name.as_bytes());
        block[100..108].copy_from_slice(&octal(mode, 8));
        block[108..116].copy_from_slice(&octal(0, 8));
        block[116..124].copy_from_slice(&octal(0, 8));
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

    fn end(&mut self) -> Vec<u8> {
        self.bytes.extend_from_slice(&[0u8; 1024]);
        std::mem::take(&mut self.bytes)
    }
}

/// A zstd block header: the last flag, the raw type, the size.
fn block_header(last: bool, size: u32) -> [u8; 3] {
    let bytes = ((size << 3) | u32::from(last)).to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// One zstd frame of a 1 MiB window holding `data` in raw (stored) blocks.
fn zstd(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 10 << 3];
    for chunk in data.chunks(128 * 1024) {
        let size = u32::try_from(chunk.len()).expect("a block of 128 KiB");
        out.extend_from_slice(&block_header(false, size));
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&block_header(true, 0));
    out
}

/// One DER element.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(u8::try_from(len).expect("short"));
    } else if len < 0x100 {
        out.extend([0x81, u8::try_from(len).expect("one byte")]);
    } else {
        out.push(0x82);
        out.extend(u16::try_from(len).expect("two bytes").to_be_bytes());
    }
    out.extend_from_slice(content);
    out
}

/// A certificate holding only the Fulcio extension that names the source ref: what the daemon
/// reads of the bundle's certificate before it runs the verifier.
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

/// The attestation bundle of an archive of SHA-256 `sha`, built from the tag `refs/tags/v1`.
fn bundle(sha: &str) -> String {
    let statement = format!(
        "{{\"_type\":\"https://in-toto.io/Statement/v1\",\"subject\":[{{\"name\":\"a\",\
         \"digest\":{{\"sha256\":\"{sha}\"}}}}],\"predicateType\":\"https://slsa.dev/provenance/v1\",\
         \"predicate\":{{\"buildDefinition\":{{\"resolvedDependencies\":[{{\"uri\":\
         \"git+https://github.com/example-org/example-repo@refs/tags/v1\",\"digest\":{{\"gitCommit\":\"{COMMIT}\"}}}}]}}}}}}"
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

/// One version of the fixture package.
pub struct Package {
    pub service: String,
    pub version: String,
    /// The SHA-256 of the archive: the digest a set of the desired state registers.
    pub digest: String,
    /// The files of the release layout, by their path under the channel.
    files: Vec<(String, Vec<u8>)>,
}

impl Package {
    /// The name of the archive, `<service>-<version>-<platform>.tar.zst`.
    pub fn archive_name(&self) -> String {
        format!("{}-{}-{}.tar.zst", self.service, self.version, platform())
    }
}

/// What the fixture package places and runs.
pub struct Fixture<'a> {
    /// The service (its unit is `<service>.service`).
    pub service: &'a str,
    /// The version.
    pub version: &'a str,
    /// The workload fixture's program, copied into the package as `bin/fixture`.
    pub program: &'a Path,
    /// Where the package places `files/fixture.conf`.
    pub placed: &'a Path,
    /// The daemon's packages directory, where the unit finds the program.
    pub packages: &'a Path,
}

impl Fixture<'_> {
    /// The package: its hook is the fixture (`ok` at every step), its one unit runs the
    /// fixture's `serve`, and its one file says the version. Its attestation bundle is one only
    /// the stand-in verifier of [`HostTrust`] takes.
    pub fn build(&self) -> Package {
        self.build_signed(|_, digest| bundle(digest).into_bytes())
    }

    /// [`Self::build`], with the attestation bundle `sign` gives for the archive's file name and
    /// SHA-256 (lowercase hex).
    pub fn build_signed(&self, sign: impl FnOnce(&str, &str) -> Vec<u8>) -> Package {
        let Fixture {
            service, version, ..
        } = *self;
        let manifest = format!(
            "schema = 1\nname = \"{service}\"\nversion = \"{version}\"\nplatform = \"{}\"\n\
             min_hostkeeper = \"0.1.0\"\n[hook]\ncommand = [\"bin/fixture\"]\n\
             verify_deadline_secs = 300\n[hook.timeout_secs]\nprepare = 60\ncutover = 60\n\
             verify = 60\nrollback = 60\ncleanup = 60\nuninstall = 60\n[units]\n\
             run = [\"{service}.service\"]\n[[files]]\nsource = \"files/fixture.conf\"\n\
             path = \"{}\"\n[params]\n\"fixture.mode\" = \"ok\"\n",
            platform(),
            self.placed.display()
        );
        let unit = format!(
            "[Unit]\nDescription=the first-boot tests' fixture service {version}\n\n[Service]\n\
             Type=exec\nExecStart={}/{service}/current/bin/fixture serve\n\n[Install]\n\
             WantedBy=multi-user.target\n",
            self.packages.display()
        );
        let program = fs::read(self.program).expect("the fixture program");
        let archive = zstd(
            &Tar::default()
                .file("manifest.toml", 0o644, manifest.as_bytes())
                .dir("bin")
                .file("bin/fixture", 0o755, &program)
                .dir("units")
                .file(&format!("units/{service}.service"), 0o644, unit.as_bytes())
                .dir("files")
                .file(
                    "files/fixture.conf",
                    0o644,
                    format!("fixture {version}\n").as_bytes(),
                )
                .end(),
        );
        let digest = sha256_hex(&archive);
        let file = format!("{service}-{version}-{}.tar.zst", platform());
        let index = format!(
            "{{\"packages\":[{{\"bytes\":{},\"file\":\"{file}\",\"platform\":\"{}\",\
             \"sha256\":\"{digest}\"}}],\"schema\":1,\"service\":\"{service}\",\
             \"version\":\"{version}\"}}",
            archive.len(),
            platform()
        );
        let base = format!("/{service}/{version}");
        Package {
            service: service.to_owned(),
            version: version.to_owned(),
            files: vec![
                (format!("{base}/index.json"), index.into_bytes()),
                (format!("{base}/{file}.sigstore.json"), sign(&file, &digest)),
                (format!("{base}/{file}"), archive),
            ],
            digest,
        }
    }
}

/// What the channel answers a path with.
#[derive(Clone)]
enum Body {
    Bytes(Vec<u8>),
    /// A file, read as it is sent: an ISO is larger than the test should hold in memory.
    File(PathBuf),
}

type Routes = Arc<Mutex<BTreeMap<String, Body>>>;

/// The release channel: an HTTPS server on a port the system picks, answering `GET` of the
/// files it was given and 404 for any other path. Dropping it stops it.
pub struct Channel {
    pub addr: SocketAddr,
    routes: Routes,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Channel {
    /// Starts the channel, with a server certificate of `ca` for `127.0.0.1`.
    pub fn start(ca: &TestCa) -> Channel {
        Channel::start_at(ca, Ipv4Addr::LOCALHOST)
    }

    /// Starts the channel on `address`, with a server certificate of `ca` for that address.
    pub fn start_at(ca: &TestCa, address: Ipv4Addr) -> Channel {
        let leaf = ca.server_for(address);
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf.key_der));
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("the protocol versions")
                .with_no_client_auth()
                .with_single_cert(vec![CertificateDer::from(leaf.cert_der)], key)
                .expect("the server config");
        let config = Arc::new(config);
        let listener = TcpListener::bind(SocketAddr::from((address, 0))).expect("bind");
        let addr = listener.local_addr().expect("the address");
        let routes: Routes = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (served, stopped) = (Arc::clone(&routes), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    // One request per connection; a client that goes away is no failure of the
                    // channel's.
                    let _ = answer(&config, stream, &served);
                }
            }
        });
        Channel {
            addr,
            routes,
            stop,
            thread: Some(thread),
        }
    }

    /// The channel's URL, as the daemon config's `release_channel` names it.
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// Serves the files of `package` from now on.
    pub fn serve(&self, package: &Package) {
        let mut routes = self.routes.lock().expect("the routes");
        for (path, body) in &package.files {
            routes.insert(path.clone(), Body::Bytes(body.clone()));
        }
    }

    /// Serves `body` at `path` from now on.
    pub fn serve_bytes(&self, path: &str, body: Vec<u8>) {
        let mut routes = self.routes.lock().expect("the routes");
        routes.insert(path.to_owned(), Body::Bytes(body));
    }

    /// Serves the file `file` at `path` from now on, read from disk at each request.
    pub fn serve_file(&self, path: &str, file: &Path) {
        let mut routes = self.routes.lock().expect("the routes");
        routes.insert(path.to_owned(), Body::File(file.to_path_buf()));
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accept waits for a connection: this one ends it.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answers one request on `stream`: the body of its path, or 404.
fn answer(config: &Arc<ServerConfig>, stream: TcpStream, routes: &Routes) -> std::io::Result<()> {
    let conn = ServerConnection::new(Arc::clone(config)).map_err(std::io::Error::other)?;
    let mut tls = BufReader::new(StreamOwned::new(conn, stream));
    let mut line = String::new();
    tls.read_line(&mut line)?;
    let path = line.split(' ').nth(1).unwrap_or("").to_owned();
    // The rest of the head, to its blank line.
    loop {
        let mut header = String::new();
        if tls.read_line(&mut header)? == 0 || header == "\r\n" {
            break;
        }
    }
    let body = routes.lock().expect("the routes").get(&path).cloned();
    let tls = tls.get_mut();
    match body {
        Some(Body::Bytes(body)) => {
            write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            tls.write_all(&body)?;
        }
        Some(Body::File(file)) => {
            let mut file = fs::File::open(file)?;
            let len = file.metadata()?.len();
            write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            )?;
            std::io::copy(&mut file, tls)?;
        }
        None => write!(
            tls,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?,
    }
    tls.conn.send_close_notify();
    tls.flush()
}
