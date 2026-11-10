//! A private repository from the origin to apt, joined for real: the real coordinator, whose
//! registry holds a private entry (`access hosts`, a credential file) and whose origin client
//! reads an in-process https origin; its heartbeat listener on a loopback socket of port 0, behind
//! the TLS of a test CA; and a Linux host enrolled at it, whose shipped links announce
//! `private-repository-v1`, receive the entry with their group as the suite, mirror the origin's
//! files through the coordinator over the host's own certificate, and stage an order of the
//! channel with the fake apt reading the mirror from disk by its `file:` URI.

mod support;
mod support_apt;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_coordinator::repository_origin::RepositoryOrigin;
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::mirror::Fetch;
use hostkeeper_link::testing::FakeClock;
use hostkeeper_link::{Got, Limit};
use hostkeeper_oidc::{Dial, Roots, Stream};
use hostkeeper_proto::hostkeeper::release::v1::{Repository, Target};
use hostkeeper_release::registry;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use support::install;
use support::pki::{Ca, enroll, files_under, renew};
use support_apt::apt_fake::Deb;
use support_apt::repository_host::{Listening, held, host, mirror_of, rendered, settle};
use support_apt::seen::{deferred, held as installed, succeeded};
use support_apt::{AptRig, SERVICE, entry, package, with_entry};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_rustls::TlsAcceptor;

/// The name the origin is dialed by, and the path of the repository on it.
const ORIGIN: &str = "origin.test";
const URI: &str = "https://origin.test/debian";

/// The `Authorization` header the credential file holds.
const AUTHORIZATION: &str = "Bearer s3cr3t-token";

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

/// What the origin answers to one path, request by request.
type Script = Vec<Option<Vec<u8>>>;

/// The origin: an https server in process that checks nothing of its caller but records the path
/// and the `Authorization` header of each request, and answers a script per path (the n-th
/// request of a path gets the n-th answer, the last again after that; none is a 404).
struct Origin {
    acceptor: TlsAcceptor,
    root: CertificateDer<'static>,
    script: Mutex<BTreeMap<String, Script>>,
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
            script: Mutex::default(),
            heard: Mutex::default(),
            down: AtomicBool::new(false),
        })
    }

    /// Every request of `path` is answered `body`.
    fn publish(&self, path: &str, body: &[u8]) {
        self.say(path, vec![Some(body.to_vec())]);
    }

    fn say(&self, path: &str, answers: Script) {
        self.script
            .lock()
            .expect("script")
            .insert(path.to_owned(), answers);
    }

    /// The paths asked of the origin, in order, and the header each came with.
    fn heard(&self) -> Vec<(String, Option<String>)> {
        self.heard.lock().expect("heard").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.heard().into_iter().map(|(path, _)| path).collect()
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
    let asked = {
        let mut heard = origin.heard.lock().expect("heard");
        heard.push((path.clone(), authorization));
        heard.iter().filter(|(seen, _)| *seen == path).count()
    };
    let answer = origin
        .script
        .lock()
        .expect("script")
        .get(&path)
        .and_then(|answers| answers.get((asked - 1).min(answers.len().saturating_sub(1))))
        .cloned()
        .flatten();
    let reply = match answer {
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

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// What the origin serves for the suite `group`: an `InRelease` listing one index, the index
/// (one package) and the package file.
struct Published {
    deb: Deb,
    release: Vec<u8>,
    packages: Vec<u8>,
    filename: String,
}

const PACKAGE_VERSION: &str = "1.0.0-1";

impl Published {
    fn new(group: &str) -> Published {
        let deb = package(PACKAGE_VERSION);
        let filename = format!("pool/{group}/{SERVICE}_{PACKAGE_VERSION}_amd64.deb");
        let packages = format!(
            "Package: {SERVICE}\nVersion: {PACKAGE_VERSION}\nArchitecture: amd64\nSize: {}\n\
             Filename: {filename}\nSHA256: {}\n\n",
            deb.bytes().len(),
            deb.pin()
        )
        .into_bytes();
        let release = format!(
            "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA512\n\nSuite: {group}\nSHA256:\n {} {} \
             main/binary-amd64/Packages\n-----BEGIN PGP SIGNATURE-----\n\nnot checked by the daemon\n\
             -----END PGP SIGNATURE-----\n",
            sha256(&packages),
            packages.len()
        )
        .into_bytes();
        Published {
            deb,
            release,
            packages,
            filename,
        }
    }

    /// Serves everything at the origin for the suite `group`.
    fn serve(&self, origin: &Origin, group: &str) {
        origin.publish(&format!("/debian/dists/{group}/InRelease"), &self.release);
        origin.publish(
            &format!("/debian/dists/{group}/main/binary-amd64/Packages"),
            &self.packages,
        );
        origin.publish(&format!("/debian/{}", self.filename), self.deb.bytes());
    }
}

/// The private repository `acme`, served to the groups `daemon` and `other`, whose credential
/// is in `credential`.
fn acme(credential: &Path) -> Repository {
    let keyring = (0..REAL_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&REAL_HEX[i..i + 2], 16).expect("hex"))
        .collect();
    Repository {
        name: "acme".to_owned(),
        kind: "apt".to_owned(),
        uri: URI.to_owned(),
        suites: vec!["daemon".to_owned(), "other".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec![PRIMARY.to_owned(), SUBKEY.to_owned()],
        keyring,
        access: "hosts".to_owned(),
        credential_file: credential.to_string_lossy().into_owned(),
    }
}

/// A credential file the coordinator accepts: one line, mode 0600, the test's own.
fn credential(dir: &Path) -> PathBuf {
    let path = dir.join("acme.credential");
    fs::write(&path, format!("{AUTHORIZATION}\n")).expect("the credential");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    path
}

/// A private repository and a host of the group `daemon`, current with the registry.
struct Bench {
    ca: Ca,
    origin: Arc<Origin>,
    listening: Listening,
    apt: AptRig,
    daemon: Arc<Daemon>,
    heartbeater: hostkeeper_link::Heartbeater,
    live: PathBuf,
    published: Published,
}

impl Bench {
    fn new(name: &str) -> Bench {
        let ca = Ca::new("hostkeeper test ca");
        let origin = Origin::new();
        let listening = Listening::with_origin(
            &format!("{name}-coordinator"),
            &ca,
            "acme",
            Some(origin.client()),
        );
        let apt = AptRig::new(&format!("{name}-daemon"));
        let live = apt.rig.scratch.path().join("etc/hostkeeper/repositories");
        let (daemon, heartbeater) = host(&listening, &ca, &apt, &live);
        let published = Published::new("daemon");
        published.serve(&origin, "daemon");
        let credential = credential(apt.rig.scratch.path());
        apt.host
            .edit(|state| state.debs.push(published.deb.clone()));
        listening.put(&acme(&credential), "private");
        Bench {
            ca,
            origin,
            listening,
            apt,
            daemon,
            heartbeater,
            live,
            published,
        }
    }

    fn mirror(&self) -> PathBuf {
        mirror_of(&self.apt).join("acme")
    }

    /// An order of the package on the pushed channel `acme-daemon`.
    fn order(&self) -> hostkeeper_proto::hostkeeper::agent::v1::Order {
        let pin = self.published.deb.pin();
        let target = Target {
            repository: Some("acme-daemon".to_owned()),
            ..entry(PACKAGE_VERSION, &pin)
        };
        with_entry(install(SERVICE, "1.0.0", &pin, 1), &target)
    }
}

/// Proves a private repository reaches apt through the mirror. The host announces
/// `private-repository-v1`, so the coordinator sends it the entry with its group (`daemon`) as
/// the one suite and no credential file; the pushed sources file names the host's own mirror as
/// `file:` URI; an order of the channel mirrors, byte for byte, the origin's `InRelease`, the
/// listed index and the package file, each read by the coordinator with the credential file's
/// line; the host's apt resolves and downloads the package from the mirror on disk; nothing the
/// host holds names the credential; and the package installs, held at its version.
///
/// Catches: an entry the coordinator sends to a daemon that cannot read it, a mirror that is not
/// what the origin serves, and a host that reaches the origin or the credential itself.
#[test]
fn a_private_repository_is_mirrored_and_apt_resolves_its_package_from_it() {
    let mut bench = Bench::new("private-e2e-install");
    settle(&mut bench.heartbeater);

    let sent = registry::for_host(&acme(Path::new("/unused")), "daemon").expect("served to daemon");
    assert_eq!(sent.suites, ["daemon"]);
    assert_eq!(
        held(&bench.live),
        rendered(&[sent], &bench.live, &mirror_of(&bench.apt))
    );
    let sources = fs::read_to_string(bench.live.join("acme-daemon.sources")).expect("sources");
    assert!(
        sources.contains(&format!("URIs: file:{}\n", bench.mirror().display())),
        "{sources}"
    );
    assert!(!sources.contains("credential") && !sources.contains("s3cr3t"));

    let reply = bench.daemon.order(&bench.order());

    let mirror = bench.mirror();
    let published = &bench.published;
    assert_eq!(
        fs::read(mirror.join("dists/daemon/InRelease")).expect("InRelease"),
        published.release
    );
    assert_eq!(
        fs::read(mirror.join("dists/daemon/main/binary-amd64/Packages")).expect("the index"),
        published.packages
    );
    assert_eq!(
        fs::read(mirror.join(&published.filename)).expect("the package"),
        published.deb.bytes()
    );
    assert_eq!(
        bench.origin.paths(),
        [
            "/debian/dists/daemon/InRelease",
            "/debian/dists/daemon/main/binary-amd64/Packages",
            &format!("/debian/{}", published.filename),
        ]
    );
    assert!(
        bench
            .origin
            .heard()
            .iter()
            .all(|(_, header)| header.as_deref() == Some(AUTHORIZATION)),
        "{:?}",
        bench.origin.heard()
    );
    let mut text = String::new();
    for file in fs::read_dir(&bench.live)
        .expect("the live directory")
        .flatten()
    {
        text.push_str(&String::from_utf8_lossy(
            &fs::read(file.path()).expect("a file"),
        ));
    }
    assert!(!text.contains("s3cr3t"));

    assert_eq!(reply, succeeded());
    assert_eq!(
        bench.apt.host.installed(SERVICE),
        installed(PACKAGE_VERSION)
    );
}

/// Proves a group reads its own paths of a private repository and no others: a host of the group
/// `other` is served the `other` suite and answered 404 for `daemon`'s `InRelease` and package
/// file, the answer of a path that does not exist; a name that is not a private repository and a
/// suite the repository does not list are 404 for the host of `daemon` too.
///
/// Catches: a route that serves a private repository's files across groups.
#[test]
fn a_host_of_another_group_is_answered_404_for_this_groups_paths() {
    let bench = Bench::new("private-e2e-groups");
    bench.published.serve(&bench.origin, "daemon");
    let other = Published::new("other");
    other.serve(&bench.origin, "other");
    let config = support::pki::config(
        &format!("https://127.0.0.1:{}", bench.listening.addr.port()),
        &bench.ca.sha256_hex(),
    );
    let reader = |name: &str, role: &str| -> Arc<dyn Fetch> {
        let files = files_under(&bench.apt.rig.scratch.path().join(name));
        fs::create_dir_all(&files.pki).expect("a pki directory");
        enroll(&files.pki, &bench.ca.host_in(role), &bench.ca);
        hostkeeper_daemon::mirror::over_coordinator(
            &config,
            &files.pki,
            Arc::new(FakeClock::new(0, 0)),
        )
        .expect("a reader")
    };
    let get =
        |fetch: &Arc<dyn Fetch>, path: &str| -> (Result<Got, hostkeeper_link::GetError>, Vec<u8>) {
            let mut body = Vec::new();
            let got = fetch.get(
                path,
                Limit {
                    max_bytes: 1 << 20,
                    deadline_secs: 60,
                },
                &mut |bytes| {
                    body.extend_from_slice(bytes);
                    Ok(())
                },
            );
            (got, body)
        };
    let daemon = reader("daemon-reader", "daemon");
    let elsewhere = reader("other-reader", "other");

    let own = "/repositories/acme/dists/daemon/InRelease".to_owned();
    let theirs = "/repositories/acme/dists/other/InRelease".to_owned();
    let (got, body) = get(&daemon, &own);
    assert_eq!(
        (got, body),
        (
            Ok(Got::Body(bench.published.release.len() as u64)),
            bench.published.release.clone()
        )
    );
    let (got, body) = get(&elsewhere, &theirs);
    assert_eq!(
        (got, body),
        (
            Ok(Got::Body(other.release.len() as u64)),
            other.release.clone()
        )
    );

    let before = bench.origin.paths().len();
    for (fetch, path) in [
        (&elsewhere, own.clone()),
        (
            &elsewhere,
            format!("/repositories/acme/{}", bench.published.filename),
        ),
        (&daemon, theirs.clone()),
        (
            &daemon,
            "/repositories/acme/dists/third/InRelease".to_owned(),
        ),
        (
            &daemon,
            "/repositories/nothere/dists/daemon/InRelease".to_owned(),
        ),
    ] {
        let (got, body) = get(fetch, &path);
        assert_eq!((got, body), (Ok(Got::NotFound), Vec::new()), "{path}");
    }
    assert_eq!(
        bench.origin.paths().len(),
        before,
        "a refused read never reaches the origin"
    );
}

/// Proves a host that moves to another group loses the old group's mirror: after its mirror is
/// filled for `daemon`, its certificate names the group `other`; the next set the coordinator
/// sends has `other` as the one suite, and once it is applied the old mirror is gone.
///
/// Catches: a mirror of one group's files left on a host that is no longer in the group.
#[test]
fn a_host_that_changes_group_loses_the_old_groups_mirror() {
    let mut bench = Bench::new("private-e2e-moved");
    settle(&mut bench.heartbeater);
    bench.daemon.order(&bench.order());
    assert!(bench.mirror().join("dists/daemon/InRelease").is_file());

    let files = files_under(bench.apt.rig.scratch.path());
    renew(&files.pki, &bench.ca.host_in("other"), &bench.ca);
    settle(&mut bench.heartbeater);

    let sources = fs::read_to_string(bench.live.join("acme-other.sources")).expect("sources");
    assert!(sources.contains("Suites: other\n"), "{sources}");
    assert!(!bench.live.join("acme-daemon.sources").exists());
    assert!(!bench.mirror().exists(), "the old group's mirror is gone");
}

/// Proves an index the origin changes between two reads is read again once: the first read of the
/// index is not what `InRelease` lists, the second is, and the order stages; and an index that is
/// wrong twice defers the order as `fetch_failed`, with no mirror directory left of the failed
/// pass.
///
/// Catches: a mirror that keeps or serves an index that is not the one `InRelease` lists.
#[test]
fn an_index_that_does_not_match_its_release_is_read_again_once_and_then_defers() {
    let mut bench = Bench::new("private-e2e-index");
    settle(&mut bench.heartbeater);
    let index = "/debian/dists/daemon/main/binary-amd64/Packages";
    bench.origin.say(
        index,
        vec![
            Some(b"Package: other\n".to_vec()),
            Some(bench.published.packages.clone()),
        ],
    );
    let reply = bench.daemon.order(&bench.order());
    assert_eq!(
        bench
            .origin
            .paths()
            .iter()
            .filter(|p| p.ends_with("InRelease"))
            .count(),
        2,
        "everything was read again"
    );
    assert_eq!(
        fs::read(
            bench
                .mirror()
                .join("dists/daemon/main/binary-amd64/Packages")
        )
        .expect("index"),
        bench.published.packages
    );
    assert!(bench.mirror().join(&bench.published.filename).is_file());
    assert_eq!(reply, succeeded());

    let mut bench = Bench::new("private-e2e-index-wrong");
    settle(&mut bench.heartbeater);
    bench
        .origin
        .say(index, vec![Some(b"Package: other\n".to_vec())]);
    assert_eq!(bench.daemon.order(&bench.order()), deferred("fetch_failed"));
    assert!(!bench.mirror().exists());
}

/// Proves the origin's failures defer the order: an origin that is down is `fetch_failed`
/// (the coordinator answers 502), and a suite the origin has not published is
/// `not_in_repository`; neither leaves a mirror.
///
/// Catches: an origin outage that fails an order for good, or one that leaves half a mirror.
#[test]
fn an_origin_that_is_down_or_has_not_published_defers_the_order() {
    let mut bench = Bench::new("private-e2e-down");
    settle(&mut bench.heartbeater);
    bench.origin.down.store(true, Ordering::SeqCst);
    assert_eq!(bench.daemon.order(&bench.order()), deferred("fetch_failed"));
    assert!(!bench.mirror().exists());

    bench.origin.down.store(false, Ordering::SeqCst);
    bench
        .origin
        .say("/debian/dists/daemon/InRelease", vec![None]);
    assert_eq!(
        bench.daemon.order(&bench.order()),
        deferred("not_in_repository")
    );
    assert!(!bench.mirror().exists());
}
