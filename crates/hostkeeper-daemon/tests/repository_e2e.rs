//! The coordinator's registry of package repositories and the daemon's repository directory,
//! joined for real: the coordinator's operator API (`Repositories.Put`, `Hosts.Get`, through the
//! JSON gateway of `hostkeeper_coordinator::grpc::route`) and its heartbeat listener on a
//! loopback socket of port 0, behind the TLS of a test CA, and the shipped heartbeat link of
//! `cli::host_links` on a Linux host whose repository directory is the daemon's managed one. The
//! reply to a gRPC heartbeat carries the set, the daemon writes it, the next heartbeat reports it,
//! and the repository installer, with apt, dpkg and dpkg-deb answered by the repository
//! installer's fake, installs a package from the entry the pushed set made.

mod support;
mod support_apt;

use std::fs;
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_coordinator::listen::{Listener, MAX_CONNECTIONS, serve};
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_daemon::cli::host_links;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::repository_push::Managed;
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_install::Family;
use hostkeeper_install::announce::Announcement;
use hostkeeper_install::repository::Repositories;
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::testing::ManualDispatch;
use hostkeeper_link::{BeatOutcome, Heartbeater};
use hostkeeper_proto::hostkeeper::release::v1::{Repository, Target};
use hostkeeper_release::{Installer, registry};
use hostkeeper_tls::TlsSource;
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};
use support::pki::{Ca, enroll, files_under, scratch};
use support::{OWN, install, my_uid};
use support_apt::seen::{held as installed, succeeded};
use support_apt::{AptRig, SERVICE, entry, package, with_entry};

/// The operator token of the coordinator.
const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// How long the coordinator waits for a client.
const PATIENCE: Duration = Duration::from_secs(10);

/// The heartbeat period the tests give `host_links`; nothing waits for it.
const PERIOD: NonZeroU64 = NonZeroU64::new(60).expect("not zero");

/// The node id of the host's certificate.
const NODE: &str = "d1";

/// The URI of the komira repository: the one the fake apt serves its package at.
const URI: &str = "https://repo.example.com/debian";

/// The real keyring (`hostkeeper-release/tests/registry/real.rs`: what `gpg --export` wrote for
/// one ed25519 primary key with a cv25519 subkey), in hex.
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
/// The primary key's fingerprint and the subkey's.
const PRIMARY: &str = "A6F3B9868560597857453C993544451951CD76B5";
const SUBKEY: &str = "DB6C3E808B8066B8721E8C6263D5F4D2CC60B367";

/// The komira repository with the real keyring, both its fingerprints listed.
fn komira_real() -> Repository {
    let keyring = (0..REAL_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&REAL_HEX[i..i + 2], 16).expect("hex"))
        .collect();
    Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: URI.to_owned(),
        suites: vec!["testing".to_owned(), "stable".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec![PRIMARY.to_owned(), SUBKEY.to_owned()],
        keyring,
    }
}

/// The komira repository with a one-key synthetic keyring that `seed` makes its own.
fn komira_synthetic(seed: u8) -> Repository {
    let body = [
        4, 0, 0, 0, seed, 1, 0, 8, 0xAA, 0xBB, 0xCC, seed, 0xDD, 0xEE,
    ];
    let mut keyring = vec![0x99, 0, body.len() as u8];
    keyring.extend_from_slice(&body);
    let fingerprint = registry::keys(&keyring).expect("a keyring")[0]
        .fingerprint
        .clone();
    Repository {
        suites: vec!["testing".to_owned(), "stable".to_owned()],
        fingerprints: vec![fingerprint],
        keyring,
        ..komira_real()
    }
}

/// A coordinator whose rollout file has `web` on the role `daemon` (the repository komira, by
/// default), over the files of `dir`.
fn coordinator(dir: &Path) -> Arc<Coordinator> {
    fs::write(
        dir.join("desired.json"),
        format!(
            r#"{{"services":{{"web":{{"version":"2.0","min_up":{{}},"digests":{{"2.0":"{}"}}}}}}}}"#,
            "0a".repeat(32)
        ),
    )
    .expect("the desired state");
    let desired = DesiredStore::open(&dir.join("desired.json"), None).expect("the desired state");
    let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
    let seconds = Arc::new(AtomicU64::new(1_000));
    let reading = Arc::clone(&seconds);
    let clock: Clock = Arc::new(move || Duration::from_secs(reading.load(Ordering::SeqCst)));
    let (verifier, _) = hostkeeper_oidc::testkit::static_verifier();
    let rollout = "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";
    let coordinator = Coordinator::new(desired, verifier, OPERATOR.to_owned(), clock)
        .with_services(Services::parse(rollout).expect("the rollout file"))
        .with_ops(ops);
    seconds.store(1_100, Ordering::SeqCst);
    Arc::new(coordinator)
}

/// The coordinator's heartbeat listener on a loopback socket of port 0, served on a runtime of
/// the test's own for as long as it lives.
struct Listening {
    coordinator: Arc<Coordinator>,
    addr: SocketAddr,
    runtime: tokio::runtime::Runtime,
    _dir: hostkeeper_testkit::Scratch,
}

impl Listening {
    fn new(name: &str, ca: &Ca) -> Listening {
        let dir = scratch(name);
        let coordinator = coordinator(dir.path());
        let leaf = ca.coordinator();
        let tls = TlsSource::fixed(
            "heartbeat",
            leaf.cert_pem.as_bytes(),
            leaf.key_pem.as_bytes(),
            Some(ca.pem.as_bytes()),
        )
        .expect("the listener's TLS");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let tcp = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback socket");
        let addr = tcp.local_addr().expect("its address");
        let tcp = Arc::new(tcp);
        let accept = move || {
            let tcp = Arc::clone(&tcp);
            async move {
                let (stream, _) = tcp.accept().await?;
                Ok::<_, std::io::Error>(stream)
            }
        };
        let listener = Listener {
            accept,
            tls,
            surface: Surface::Heartbeat,
        };
        runtime.spawn(serve(
            listener,
            Arc::clone(&coordinator),
            PATIENCE,
            MAX_CONNECTIONS,
        ));
        Listening {
            coordinator,
            addr,
            runtime,
            _dir: dir,
        }
    }

    /// A call of the operator API's JSON gateway with `body`: the status and the answer as JSON.
    fn operate(&self, method: &str, body: &Value) -> (u16, Value) {
        let request = hyper::Request::builder()
            .method("POST")
            .uri(format!("/hostkeeper.operator.v1.{method}"))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {OPERATOR}"))
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("a request");
        self.runtime.block_on(async {
            let response = hostkeeper_coordinator::grpc::route(
                &self.coordinator,
                Surface::Control,
                &None,
                request,
                PATIENCE,
            )
            .await;
            let status = response.status().as_u16();
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("a body")
                .to_bytes();
            (status, serde_json::from_slice(&bytes).expect("JSON"))
        })
    }

    /// `Repositories.Put` of `repository`.
    fn put(&self, repository: &Repository, reason: &str) {
        let (status, said) = self.operate(
            "Repositories/Put",
            &json!({
                "repository": {
                    "name": repository.name,
                    "kind": repository.kind,
                    "uri": repository.uri,
                    "suites": repository.suites,
                    "components": repository.components,
                    "fingerprints": repository.fingerprints,
                    "keyring": base64::engine::general_purpose::STANDARD
                        .encode(&repository.keyring),
                },
                "reason": reason,
            }),
        );
        assert_eq!(status, 200, "{said}");
    }

    /// What `Hosts.Get` shows of the host's repositories.
    fn host_repositories(&self) -> Value {
        let (status, said) = self.operate("Hosts/Get", &json!({ "nodeId": NODE }));
        assert_eq!(status, 200, "{said}");
        said["host"]["repositories"].clone()
    }
}

/// A Linux host enrolled at `listening`: its daemon, whose repository ports name `live` as the
/// directory the coordinator's sets are written to and which has apt in a fake, and the heartbeat
/// loop of the shipped links, no order of which runs.
fn host(listening: &Listening, ca: &Ca, apt: &AptRig, live: &Path) -> (Arc<Daemon>, Heartbeater) {
    let repository = (*apt.rig.repository_ports(Repositories::default()))
        .clone()
        .with_managed(Managed {
            dir: live.to_path_buf(),
            owner_uid: my_uid(),
            native: Some(Installer::Apt),
        });
    let ports = Ports {
        repository: Arc::new(repository),
        announcement: Announcement {
            installers: vec![Installer::HostkeeperArchive],
            family: Some(Family::Deb),
        },
        ..apt.rig.ports()
    };
    let config = support::pki::config(
        &format!("https://127.0.0.1:{}", listening.addr.port()),
        &ca.sha256_hex(),
    );
    let daemon = Arc::new(Daemon::new(config, apt.rig.dirs(), ports, OWN));
    let files = files_under(apt.rig.scratch.path());
    enroll(&files.pki, &ca.host(), ca);
    let links = Links {
        dispatch: Arc::new(ManualDispatch::default()),
        ..host_links(
            &daemon,
            &files,
            PERIOD,
            PlatformKind::Linux,
            Box::new(hostkeeper_daemon::NoWatchdog),
        )
    };
    let heartbeater = links.heartbeater(&daemon);
    (daemon, heartbeater)
}

/// The files of `dir` by name, with their bytes, sorted by name.
fn held(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
        .expect("the repositories directory")
        .map(|entry| {
            let entry = entry.expect("an entry");
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).expect("a file"),
            )
        })
        .collect();
    files.sort();
    files
}

/// The files `registry::render` makes of `entries` for `dir`, as [`held`] lists them.
fn rendered(entries: &[Repository], dir: &Path) -> Vec<(String, Vec<u8>)> {
    registry::render(entries, dir.to_str().expect("a UTF-8 path"))
        .into_iter()
        .map(|file| (file.name, file.bytes))
        .collect()
}

/// Beats three times: the first over JSON (the reply advertises gRPC), the second over gRPC (its
/// reply carries the set the report asks for), the third reports the set held.
fn settle(heartbeater: &mut Heartbeater) {
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
}

/// Proves an operator's `Put` of the repository komira reaches a host whose service names it: the
/// daemon's repositories directory holds exactly the files `registry::render` makes of the
/// entry, byte for byte (the real keyring and one sources file per suite); the host's next
/// heartbeat reports the registry's generation and the set's digest, so `Hosts.Get` shows
/// `repositories.current`; the daemon announces `apt`; and the repository installer, given an
/// order whose entry is the pushed channel `komira-stable`, installs the package the fake apt
/// serves at the pushed URI, held at its version.
///
/// Catches: a set the coordinator makes and the daemon cannot take (or takes and does not report),
/// and entries the push writes that the installer does not resolve a package from.
#[test]
fn a_put_repository_reaches_the_host_and_apt_resolves_its_package() {
    let ca = Ca::new("hostkeeper test ca");
    let listening = Listening::new("repository-e2e-put-coordinator", &ca);
    let apt = AptRig::new("repository-e2e-put-daemon");
    let live = apt.rig.scratch.path().join("etc/hostkeeper/repositories");
    let (daemon, mut heartbeater) = host(&listening, &ca, &apt, &live);
    let komira = komira_real();
    listening.put(&komira, "first");

    settle(&mut heartbeater);

    let files = held(&live);
    assert_eq!(files, rendered(std::slice::from_ref(&komira), &live));
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "komira-stable.sources",
            "komira-testing.sources",
            "komira.gpg"
        ]
    );
    let shown = listening.host_repositories();
    assert_eq!(shown["current"], true, "{shown}");
    assert_eq!(shown["generation"], "1", "{shown}");
    assert_eq!(shown["digest"], registry::digest(&[komira]), "{shown}");
    assert_eq!(shown["named"], json!(["komira"]));
    assert!(daemon.ctx().installers().contains(&Installer::Apt));

    let entries = daemon.ctx().repository_ports.repositories.snapshot();
    assert_eq!(
        entries.get("komira-stable").expect("the entry").keyring,
        live.join("komira.gpg")
    );
    assert!(entries.get("komira-testing").is_some());

    let pin = apt.host.publish(URI, package("1.0.0-1"));
    let target = Target {
        repository: Some("komira-stable".to_owned()),
        ..entry("1.0.0-1", &pin)
    };
    let order = with_entry(install(SERVICE, "1.0.0", &pin, 1), &target);
    assert_eq!(daemon.order(&order), succeeded());
    assert_eq!(apt.host.installed(SERVICE), installed("1.0.0-1"));
    let sealed = hostkeeper_daemon::target::read(&apt.version_dir("1.0.0"))
        .expect("it reads")
        .expect("a target.json");
    assert_eq!(sealed.repository.as_deref(), Some("komira-stable"));
}

/// Proves a key rotation rolls to the host: a second `Put` of komira with another keyring is
/// written at the host's next heartbeat (its keyring file and sources files are those
/// `registry::render` makes of the new entry), the generation the host reports grows from 1 to
/// 2 with the new digest, and `Hosts.Get` shows the host current again.
///
/// Catches: a registry change the coordinator does not push to a host that already holds a set,
/// or a daemon that keeps the first keyring.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_key_rotation_rolls_to_the_host() {
    let ca = Ca::new("hostkeeper test ca");
    let listening = Listening::new("repository-e2e-rotate-coordinator", &ca);
    let apt = AptRig::new("repository-e2e-rotate-daemon");
    let live = apt.rig.scratch.path().join("etc/hostkeeper/repositories");
    let (_daemon, mut heartbeater) = host(&listening, &ca, &apt, &live);
    let first = komira_real();
    listening.put(&first, "first");
    settle(&mut heartbeater);
    assert_eq!(held(&live), rendered(std::slice::from_ref(&first), &live));
    assert_eq!(listening.host_repositories()["generation"], "1");

    let second = komira_synthetic(2);
    listening.put(&second, "rotate");
    // The gateway leaves a false `current` out of the JSON.
    assert_ne!(
        listening.host_repositories()["current"],
        true,
        "the host holds the old set until it beats"
    );
    for _ in 0..2 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }

    assert_eq!(held(&live), rendered(std::slice::from_ref(&second), &live));
    assert_eq!(
        fs::read(live.join("komira.gpg")).expect("the keyring"),
        second.keyring
    );
    let shown = listening.host_repositories();
    assert_eq!(shown["current"], true, "{shown}");
    assert_eq!(shown["generation"], "2", "{shown}");
    assert_eq!(shown["digest"], registry::digest(&[second]), "{shown}");
}
