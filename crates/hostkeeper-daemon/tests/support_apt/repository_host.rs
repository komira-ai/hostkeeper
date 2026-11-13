//! The shared half of the repository end-to-end tests: the real coordinator behind a listener on a
//! loopback socket of port 0 (the heartbeat surface, the TLS of a test CA), its operator API
//! through the JSON gateway, and a Linux host enrolled at it whose daemon has the shipped links,
//! the managed repository directory, and the mirror of private repositories.

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
use hostkeeper_coordinator::repository_origin::RepositoryOrigin;
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
use hostkeeper_link::testing::{FakeClock, ManualDispatch};
use hostkeeper_link::{BeatOutcome, Heartbeater};
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::{Installer, registry};
use hostkeeper_tls::TlsSource;
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};

use super::AptRig;
use crate::support::pki::{self, Ca, enroll, files_under, scratch};
use crate::support::{OWN, my_uid};

/// The operator token of the coordinator.
pub const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// How long the coordinator waits for a client.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// The heartbeat period the tests give `host_links`; nothing waits for it.
pub const PERIOD: NonZeroU64 = NonZeroU64::new(60).expect("not zero");

/// The node id of the host's certificate.
pub const NODE: &str = "d1";

/// A coordinator whose rollout file has `web` on the role `daemon` (the repository komira, by
/// default), over the files of `dir`.
pub fn coordinator(
    dir: &Path,
    repository: &str,
    origin: Option<RepositoryOrigin>,
) -> Arc<Coordinator> {
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
    let rollout = format!(
        "[[service]]\nname = \"web\"\nroles = [\"daemon\", \"other\"]\nrepository = \"{repository}\"\n"
    );
    let allow = hostkeeper_oidc::AllowList::parse(&hostkeeper_oidc::testkit::config_text(
        hostkeeper_oidc::testkit::STATIC_ISSUER,
    ))
    .expect("the allow-list");
    let mut coordinator = Coordinator::new(desired, verifier, OPERATOR.to_owned(), clock)
        .with_allow_list(&allow, None)
        .with_services(Services::parse(&rollout).expect("the rollout file"))
        .with_ops(ops);
    if let Some(origin) = origin {
        coordinator = coordinator.with_repository_origin(origin);
    }
    seconds.store(1_100, Ordering::SeqCst);
    Arc::new(coordinator)
}

/// The coordinator's heartbeat listener on a loopback socket of port 0, served on a runtime of
/// the test's own for as long as it lives.
pub struct Listening {
    pub coordinator: Arc<Coordinator>,
    pub addr: SocketAddr,
    pub runtime: tokio::runtime::Runtime,
    _dir: hostkeeper_testkit::Scratch,
}

impl Listening {
    pub fn new(name: &str, ca: &Ca) -> Listening {
        Listening::with_origin(name, ca, "komira", None)
    }

    /// A listening coordinator whose service `web` names the repository `repository`, and which
    /// reads private repositories' origins with `origin`.
    pub fn with_origin(
        name: &str,
        ca: &Ca,
        repository: &str,
        origin: Option<RepositoryOrigin>,
    ) -> Listening {
        let dir = scratch(name);
        let coordinator = coordinator(dir.path(), repository, origin);
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
    pub fn operate(&self, method: &str, body: &Value) -> (u16, Value) {
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
    pub fn put(&self, repository: &Repository, reason: &str) {
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
                    "access": repository.access,
                    "credentialFile": repository.credential_file,
                },
                "reason": reason,
            }),
        );
        assert_eq!(status, 200, "{said}");
    }

    /// What `Hosts.Get` shows of the host's repositories.
    pub fn host_repositories(&self) -> Value {
        let (status, said) = self.operate("Hosts/Get", &json!({ "nodeId": NODE }));
        assert_eq!(status, 200, "{said}");
        said["host"]["repositories"].clone()
    }
}

/// A Linux host enrolled at `listening`: its daemon, whose repository ports name `live` as the
/// directory the coordinator's sets are written to and which has apt in a fake, and the heartbeat
/// loop of the shipped links, no order of which runs.
pub fn host(
    listening: &Listening,
    ca: &Ca,
    apt: &AptRig,
    live: &Path,
) -> (Arc<Daemon>, Heartbeater) {
    let config = pki::config(
        &format!("https://127.0.0.1:{}", listening.addr.port()),
        &ca.sha256_hex(),
    );
    let files = files_under(apt.rig.scratch.path());
    enroll(&files.pki, &ca.host(), ca);
    let fetch = hostkeeper_daemon::mirror::over_coordinator(
        &config,
        &files.pki,
        Arc::new(FakeClock::new(0, 0)),
    )
    .expect("the mirror's fetch");
    let repository = (*apt.rig.repository_ports(Repositories::default()))
        .clone()
        .with_managed(Managed {
            dir: live.to_path_buf(),
            owner_uid: my_uid(),
            native: Some(Installer::Apt),
        })
        .with_mirror(fetch);
    let ports = Ports {
        repository: Arc::new(repository),
        announcement: Announcement {
            installers: Vec::new(),
            family: Some(Family::Deb),
        },
        ..apt.rig.ports()
    };
    let daemon = Arc::new(Daemon::new(config, apt.rig.dirs(), ports, OWN));
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
pub fn held(dir: &Path) -> Vec<(String, Vec<u8>)> {
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
pub fn rendered(entries: &[Repository], dir: &Path, mirror: &Path) -> Vec<(String, Vec<u8>)> {
    registry::render(
        entries,
        dir.to_str().expect("a UTF-8 path"),
        mirror.to_str().expect("a UTF-8 path"),
    )
    .into_iter()
    .map(|file| (file.name, file.bytes))
    .collect()
}

/// Beats three times: the first over JSON (the reply advertises gRPC), the second over gRPC (its
/// reply carries the set the report asks for), the third reports the set held.
pub fn settle(heartbeater: &mut Heartbeater) {
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
}

/// Where the host's mirrors of private repositories are: beside its packages directory.
pub fn mirror_of(apt: &AptRig) -> std::path::PathBuf {
    apt.rig
        .repository_ports(Repositories::default())
        .dirs
        .mirror
        .clone()
}
