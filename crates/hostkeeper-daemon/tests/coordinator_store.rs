//! The Mac's `coordinator` store, joined for real: a `macos-pkg` order whose entry is located in
//! the store `coordinator` is staged from the real coordinator, on a loopback listener behind the
//! TLS of a test CA, which reads a fake https origin (in process, no sockets) with the private
//! repository's credential. The daemon's package store reaches the coordinator over the host's
//! own certificate ([`CoordinatorFetcher`]), checks the `.pkg` by the entry's digest and size, and
//! the installer checks its signer; the fake Mac installs it.

mod support;
mod support_apt;
mod support_macos_pkg;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::Bounds;
use hostkeeper_coordinator::repository_origin::RepositoryOrigin;
use hostkeeper_daemon::coordinator_store::CoordinatorFetcher;
use hostkeeper_daemon::mirror::over_coordinator;
use hostkeeper_daemon::real_packages::RealPackages;
use hostkeeper_daemon::testing::FakeClock as DaemonClock;
use hostkeeper_daemon::{Daemon, Ports, Refusal, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_link::testing::FakeClock;
use hostkeeper_oidc::{Dial, Roots, Stream};
use hostkeeper_package::ports::{Fetcher, Got, Limit, NetError, Sink, Url};
use hostkeeper_package::stores::Stores;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Repository, Target};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use support::pki::{Ca, enroll, files_under};
use support::{OWN, config};
use support_apt::repository_host::Listening;
use support_macos_pkg::{MacRig, SERVICE, entry, install_of, package, sha256_hex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_rustls::TlsAcceptor;

/// The name the origin is dialed by, and the path of the repository on it.
const ORIGIN: &str = "origin.test";
const URI: &str = "https://origin.test/debian";

/// The `Authorization` header the credential file holds.
const AUTHORIZATION: &str = "Bearer s3cr3t-token";

/// The group of the enrolled host, and the file the order's entry names in the store.
const GROUP: &str = "daemon";
const LOCATOR: &str = "repositories/acme/pool/daemon/hello.pkg";
const AT_ORIGIN: &str = "/debian/pool/daemon/hello.pkg";

/// The real keyring and its fingerprints, of `repository_e2e.rs`.
const REAL_HEX: &str = "\
9833046af2556016092b06010401da470f0101074077a32c6d94964430e379097f14afb41bd536d84fc0516193e1103a\
75cc563c6fb41b54657374204b6579203c74657374406578616d706c652e636f6d3e88900413160a0038162104a6f3b9\
868560597857453c993544451951cd76b505026af25560021b03050b0908070206150a09080b020416020301021e0102\
1780000a09103544451951cd76b5f36100fe3b7dbe1ff05c17e9337e0d6863e7db476429eb63b06100b9ec59cd02aaa8\
844e0100c1c3a34a86543f79c7dba00e3f31fe58f7cebad926b6883f387160cfe99fbf0eb838046af25560120a2b0601\
04019755010501010740bbf5f929451ed33256de9306391e2c0a34be9166376d9f6e1cd60ae6bcbf2636030108078878\
0418160a0020162104a6f3b9868560597857453c993544451951cd76b505026af25560021b0c000a09103544451951cd\
76b5aaaf0100e36f278ce962aeae0888d2da0a5b8885ee75466476bcc4e7626dc5c0a8d4740e0100bc659df3a0c1b9c0\
6b23a435fe34630aba635e62d3d8e073476e368db89f3505\
";
const PRIMARY: &str = "A6F3B9868560597857453C993544451951CD76B5";
const SUBKEY: &str = "DB6C3E808B8066B8721E8C6263D5F4D2CC60B367";

/// The origin: an https server in process that records the path and the `Authorization` header
/// of each request and answers the bodies it was given (none is a 404, or a refused connection
/// while it is down).
struct Origin {
    acceptor: TlsAcceptor,
    root: CertificateDer<'static>,
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    heard: Mutex<Vec<(String, Option<String>)>>,
    down: AtomicBool,
}

impl Origin {
    fn new() -> Arc<Origin> {
        let certified =
            rcgen::generate_simple_self_signed(vec![ORIGIN.to_owned()]).expect("a certificate");
        let root = certified.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(vec![root.clone()], key)
            .expect("a server config");
        Arc::new(Origin {
            acceptor: TlsAcceptor::from(Arc::new(config)),
            root,
            files: Mutex::default(),
            heard: Mutex::default(),
            down: AtomicBool::new(false),
        })
    }

    fn publish(&self, path: &str, body: &[u8]) {
        self.files
            .lock()
            .expect("files")
            .insert(path.to_owned(), body.to_vec());
    }

    fn heard(&self) -> Vec<(String, Option<String>)> {
        self.heard.lock().expect("heard").clone()
    }

    /// The client the coordinator reads this origin with.
    fn client(self: &Arc<Self>) -> RepositoryOrigin {
        let origin = Arc::clone(self);
        let dial: Dial = Arc::new(move |_host: String, _port: u16| {
            let origin = Arc::clone(&origin);
            Box::pin(async move {
                if origin.down.load(Ordering::SeqCst) {
                    return Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
                }
                let (client, server) = tokio::io::duplex(256 * 1024);
                tokio::spawn(serve(origin, server));
                Ok::<Stream, std::io::Error>(Box::new(client))
            })
        });
        RepositoryOrigin::new(Roots::Given(vec![self.root.clone()]))
            .expect("an origin client")
            .with_dial(dial)
    }
}

/// One connection of the origin: the handshake, one request, its answer.
async fn serve(origin: Arc<Origin>, stream: tokio::io::DuplexStream) {
    let Ok(mut tls) = origin.acceptor.accept(stream).await else {
        return;
    };
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or_default()
        .to_owned();
    let authorization = text.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .starts_with("authorization:")
            .then(|| line["authorization:".len()..].trim().to_owned())
    });
    origin
        .heard
        .lock()
        .expect("heard")
        .push((path.clone(), authorization));
    let body = origin.files.lock().expect("files").get(&path).cloned();
    let reply = match body {
        Some(body) => {
            let mut bytes = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            bytes.extend_from_slice(&body);
            bytes
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    let _ = tls.write_all(&reply).await;
    let _ = tls.shutdown().await;
}

/// The web: a fetcher that reaches nothing, so a location asked of it fails to connect.
struct NoWeb;

impl Fetcher for NoWeb {
    fn get(&self, _url: &Url, _sink: &mut dyn Sink, _limit: Limit) -> Result<Got, NetError> {
        Err(NetError::Connect)
    }
}

/// The private repository `acme`, served to the group of the host, whose credential is in
/// `credential`.
fn acme(credential: &Path) -> Repository {
    let keyring = (0..REAL_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&REAL_HEX[i..i + 2], 16).expect("hex"))
        .collect();
    Repository {
        name: "acme".to_owned(),
        kind: "apt".to_owned(),
        uri: URI.to_owned(),
        suites: vec![GROUP.to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec![PRIMARY.to_owned(), SUBKEY.to_owned()],
        keyring,
        access: "hosts".to_owned(),
        credential_file: credential.to_string_lossy().into_owned(),
        ..Repository::default()
    }
}

/// A coordinator with the private repository `acme`, its origin, and a Mac whose daemon reads the
/// `coordinator` store over the certificate of a host of the group `daemon`.
struct Bench {
    origin: Arc<Origin>,
    mac: MacRig,
    daemon: Daemon,
    bytes: Vec<u8>,
    entry: Target,
    // Held for the listener and the host's pki directory.
    _listening: Listening,
}

impl Bench {
    /// `with_store`: whether the daemon's package store has the coordinator fetcher.
    fn new(name: &str, with_store: bool) -> Bench {
        let ca = Ca::new("hostkeeper test ca");
        let origin = Origin::new();
        let listening = Listening::with_origin(
            &format!("{name}-coordinator"),
            &ca,
            "acme",
            Some(origin.client()),
        );
        let mac = MacRig::new(name);
        let credential = mac.rig.scratch.path().join("acme.credential");
        fs::write(&credential, format!("{AUTHORIZATION}\n")).expect("the credential");
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).expect("chmod");
        listening.put(&acme(&credential), "private");

        let files = files_under(mac.rig.scratch.path());
        enroll(&files.pki, &ca.host(), &ca);
        let host_config = support::pki::config(
            &format!("https://127.0.0.1:{}", listening.addr.port()),
            &ca.sha256_hex(),
        );
        let fetch = over_coordinator(&host_config, &files.pki, Arc::new(FakeClock::new(0, 0)))
            .expect("the coordinator's fetch");

        for dir in [mac.rig.scratch.path(), &mac.rig.packages] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        let mut packages = RealPackages::new(
            Box::new(NoWeb),
            DaemonClock::new(),
            Bounds::default(),
            Stores::default(),
            &mac.rig.packages,
        );
        if with_store {
            packages = packages.with_coordinator(Box::new(CoordinatorFetcher::new(
                fetch,
                DaemonClock::new(),
                config().bounds.max_package_bytes,
            )));
        }
        let ports = Ports {
            packages: Arc::new(packages),
            ..mac.ports()
        };
        let daemon = Daemon::new(config(), mac.rig.dirs(), ports, OWN);

        let bytes = format!("fake-pkg:{SERVICE}-1.0.0").into_bytes();
        origin.publish(AT_ORIGIN, &bytes);
        mac.mac.add(package("1.0.0"));
        let mut entry = entry(&sha256_hex(&bytes), bytes.len() as u64);
        entry.locations = vec![Location {
            store: "coordinator".to_owned(),
            locator: LOCATOR.to_owned(),
        }];
        Bench {
            origin,
            mac,
            daemon,
            bytes,
            entry,
            _listening: listening,
        }
    }

    fn order(&self) -> Reply {
        self.daemon.order(&install_of("1.0.0", &self.entry, 1))
    }

    /// The names left in the service's package directory.
    fn left(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.mac.rig.packages.join(SERVICE))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The refusal an order was answered with.
fn refusal(reply: Reply) -> Refusal {
    match reply {
        Reply::Refused { refusal, .. } => refusal,
        other => panic!("not a refusal: {other:?}"),
    }
}

/// Proves a `macos-pkg` order whose entry is in the `coordinator` store is staged from the
/// coordinator: the Mac asks the coordinator for `/` and the locator over its certificate, the
/// coordinator reads the origin with the repository's credential, and the `.pkg` is installed
/// after its digest and size and its signer are checked. The origin was asked for the one file,
/// and the fetched file is gone from the packages directory afterwards.
///
/// Catches: a coordinator location that is no store on a host that has the fetcher, a path other
/// than `/` and the locator, and a `.pkg` staged without the entry's digest.
#[test]
fn a_macos_pkg_in_the_coordinator_store_is_staged_from_the_coordinator() {
    let bench = Bench::new("coordinator-store-install", true);

    let reply = bench.order();

    assert_eq!(
        bench.origin.heard(),
        [(AT_ORIGIN.to_owned(), Some(AUTHORIZATION.to_owned()))]
    );
    let staged = bench
        .mac
        .version_dir("1.0.0")
        .join("macos-pkg/artifact.pkg");
    assert_eq!(fs::read(staged).expect("the staged .pkg"), bench.bytes);
    assert!(bench.left().iter().all(|name| !name.starts_with(".fetch")));
    assert_eq!(reply, succeeded());
}

/// Proves other bytes than the entry's from the coordinator are refused
/// (`package_digest_mismatch`, a release fault) and nothing is staged.
///
/// Catches: a `.pkg` taken from the coordinator on the coordinator's word.
#[test]
fn other_bytes_from_the_coordinator_are_refused() {
    let bench = Bench::new("coordinator-store-other", true);
    bench.origin.publish(AT_ORIGIN, b"not the package");

    assert_eq!(refusal(bench.order()), Refusal::DigestMismatch);

    assert!(!bench.mac.version_dir("1.0.0").exists());
    assert!(bench.left().iter().all(|name| !name.starts_with(".fetch")));
}

/// Proves a file the origin does not have (the coordinator answers 404) and an origin that
/// cannot be reached (the coordinator answers 502) defer the order as `fetch_failed`, and that the
/// order then installs once the origin has the file.
///
/// Catches: a coordinator failure that fails the order for good instead of deferring it.
#[test]
fn a_missing_file_and_a_down_origin_defer_the_order() {
    let bench = Bench::new("coordinator-store-down", true);
    bench.origin.files.lock().expect("files").remove(AT_ORIGIN);
    assert_eq!(refusal(bench.order()), Refusal::FetchFailed);

    bench.origin.publish(AT_ORIGIN, &bench.bytes);
    bench.origin.down.store(true, Ordering::SeqCst);
    assert_eq!(refusal(bench.order()), Refusal::FetchFailed);

    bench.origin.down.store(false, Ordering::SeqCst);
    let reply = bench.order();
    let staged = bench
        .mac
        .version_dir("1.0.0")
        .join("macos-pkg/artifact.pkg");
    assert_eq!(fs::read(staged).expect("the staged .pkg"), bench.bytes);
    assert_eq!(reply, succeeded());
}

/// Proves a package store with no coordinator fetcher has no source for a coordinator location:
/// the order fails `no_store` and nothing is asked of the origin.
///
/// Catches: a coordinator location asked of the web fetcher.
#[test]
fn without_the_coordinator_fetcher_the_order_is_no_store() {
    let bench = Bench::new("coordinator-store-none", false);

    assert_eq!(refusal(bench.order()), Refusal::NoStore);
    assert!(bench.origin.heard().is_empty());
}

/// Proves a Mac daemon announces `private-repository-v1` by the platform alone and a Linux
/// daemon does not (it announces it with its repository sink).
///
/// Catches: a Mac that does not announce the feature, which the coordinator then never lets
/// serve it a config package.
#[test]
fn only_a_mac_announces_private_repositories_by_its_platform() {
    use hostkeeper_daemon::links::reads_private_repositories;
    use hostkeeper_link::Platform;
    assert!(reads_private_repositories(Platform::DarwinArm64));
    assert!(!reads_private_repositories(Platform::LinuxX86_64));
    assert!(!reads_private_repositories(Platform::LinuxArm64));
}

/// Proves the ports are built for a config whose coordinator address cannot be dialed (the
/// heartbeat refuses the address and says so): the package store then has no coordinator
/// fetcher, and a start with a good address builds the same ports.
///
/// Catches: a start that fails for an address only the coordinator's reads need.
#[test]
fn the_ports_are_built_whether_or_not_the_coordinator_can_be_dialed() {
    use hostkeeper_config::daemon::DaemonConfig;
    use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE};
    use hostkeeper_daemon::cli::host_ports;
    use hostkeeper_launchd::Platform;
    use hostkeeper_observe::fake::FakeCommandRunner;

    let rig = support::Rig::new("coordinator-store-ports");
    fs::create_dir_all(&rig.packages).expect("the packages directory");
    let files = files_under(rig.scratch.path());
    for url in [
        "https://coordinator.test:8443",
        "https://coordinator.test:8443/x",
    ] {
        let config = DaemonConfig::parse(&format!(
            "coordinator = \"{url}\"\nca_sha256 = \"{}\"\nrelease_channel = \"https://releases.test\"\n\n\
             [trusted_signers]\norganisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
             refs = [\"refs/tags/*\"]\nhosted_runner = true\n",
            "ab".repeat(32)
        ))
        .expect("a config");
        let adapters = Adapters::linux(
            &Platform::linux(),
            RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
        );
        host_ports(&config, &rig.settings(), &files, OWN, adapters).expect("the ports");
    }
}
