//! The route that serves a private repository to the hosts of a group
//! ([`crate::repository_serve`]), driven through [`crate::grpc::route`] on the heartbeat surface
//! with the certificate of a host, against an origin served in process
//! ([`crate::repository_origin_support`]): what a host is answered, what the origin is asked, and
//! what the log says. No socket is opened.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{Method, Request, StatusCode};

use super::support::{Served, enrollment_in, peer, raw, served_built, updater};
use crate::host_groups_support::{SELF, body, enroll_host, move_of, put_of};
use crate::http::{Peer, Surface};
use crate::marks_support::operator;
use crate::repositories_support::{credential_in, private, repository};
use crate::repository_origin::RepositoryOrigin;
use crate::repository_origin_support::{FakeOrigin, Then, hanging_dial, refusing_dial, response};
use crate::testutil::{Log, Rig, scratch};
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;

const URI: &str = "https://origin.test/apt";
const TOKEN: &str = "Bearer s3cret-token-value";
const NOT_FOUND: &str = r#"{"error":"not_found"}"#;
const ORIGIN_FAILED: &str = r#"{"error":"origin_failed"}"#;
const RELEASE: &[u8] = b"Origin: priv\nSuite: daemon\n";

/// The bytes of the pool file: a pattern longer than the stream's buffers.
fn package() -> Vec<u8> {
    (0..300_000_u32)
        .map(|n| u8::try_from(n % 251).expect("a byte"))
        .collect()
}

/// A coordinator with a private repository `priv` for the groups `daemon` and `storage`, its
/// credential file, and an origin client over `fake`'s dial.
struct Set {
    served: Served,
    fake: FakeOrigin,
    dir: Scratch,
    file: String,
}

fn set_up(name: &str, dial: impl FnOnce(&FakeOrigin) -> hostkeeper_oidc::Dial) -> Set {
    let fake = FakeOrigin::new();
    let dir = scratch(name);
    let file = credential_in(dir.path(), "priv-credential", &format!("{TOKEN}\n"));
    let client = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(dial(&fake));
    let served = served_built(name, |scratch, clock| {
        Rig::start(scratch, clock).with_repository_origin(client)
    });
    let set = Set {
        served,
        fake,
        dir,
        file,
    };
    set.put(private("priv", URI, &["daemon", "storage"], 1, &set.file));
    set.put(repository("pub", "https://origin.test/pub", &["daemon"], 2));
    set
}

impl Set {
    fn put(&self, repository: hostkeeper_proto::hostkeeper::release::v1::Repository) {
        self.served
            .coordinator
            .repositories_put(
                &operator(),
                proto::PutRepositoryRequest {
                    repository: Some(repository),
                    reason: "for a test".to_owned(),
                },
            )
            .expect("put");
    }

    /// The answer to `GET path` on the heartbeat surface from `host`.
    async fn get(&self, host: &Peer, path: &str) -> (StatusCode, hyper::HeaderMap, Bytes) {
        self.send(host, Method::GET, path).await
    }

    async fn send(
        &self,
        host: &Peer,
        method: Method,
        path: &str,
    ) -> (StatusCode, hyper::HeaderMap, Bytes) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Full::new(Bytes::new()))
            .expect("a request");
        raw(&self.served.coordinator, Surface::Heartbeat, host, request).await
    }
}

fn serving(path: &str) -> Then {
    match path {
        "/apt/dists/daemon/InRelease" => Then::Write(response("200 OK", RELEASE)),
        "/apt/pool/daemon/main/w/web_1_amd64.deb" => Then::Write(response("200 OK", &package())),
        "/apt/dists/daemon/gone" => Then::Write(response("404 Not Found", b"")),
        _ => Then::Write(response("500 Oops", b"")),
    }
}

/// The lines of `log` without their time, which is the one part that changes.
fn lines(log: &Log) -> Vec<String> {
    log.text()
        .lines()
        .map(|line| line.split_once(' ').expect("a time").1.to_owned())
        .collect()
}

fn host_id(text: &str) -> HostId {
    HostId::parse(text).expect("a host id")
}

fn host(name: &str, role: &str) -> Peer {
    updater(name, role)
}

/// Proves: a host of the group `daemon` is answered its group's `InRelease` and a pool file, each
/// 200 with the origin's exact bytes and `Content-Length`; the origin is asked once for each, for
/// the path under the repository's `uri`, with the `Authorization` header equal to the credential
/// file's line (no newline), and the host's own headers are not forwarded.
///
/// Catches: a body that is cut or buffered wrongly, a missing length, a credential not sent.
#[tokio::test]
async fn a_host_is_served_its_groups_files_with_the_credential() {
    let set = set_up("serve-own", |fake| fake.dial(Arc::new(serving)));
    let d1 = host("d1", "daemon");
    let (status, headers, body) = set
        .get(&d1, "/repositories/priv/dists/daemon/InRelease")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], RELEASE);
    assert_eq!(headers[CONTENT_LENGTH], RELEASE.len().to_string());
    assert_eq!(headers[CONTENT_TYPE], "application/octet-stream");

    let (status, headers, body) = set
        .get(&d1, "/repositories/priv/pool/daemon/main/w/web_1_amd64.deb")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.to_vec(), package());
    assert_eq!(headers[CONTENT_LENGTH], package().len().to_string());

    let heads = set.fake.heard();
    assert_eq!(heads.len(), 2);
    let first: Vec<&str> = heads[0].lines().collect();
    assert_eq!(first[0], "GET /apt/dists/daemon/InRelease HTTP/1.1");
    assert!(
        first.contains(&"authorization: Bearer s3cret-token-value"),
        "{first:?}"
    );
    assert!(heads[1].starts_with("GET /apt/pool/daemon/main/w/web_1_amd64.deb HTTP/1.1\r\n"));
}

/// Proves: the credential file is read again at every request: a line written after the first
/// request is the one the second request carries.
///
/// Catches: a rotation that needs a restart.
#[tokio::test]
async fn a_rotated_credential_is_sent_without_a_restart() {
    let set = set_up("serve-rotation", |fake| fake.dial(Arc::new(serving)));
    let d1 = host("d1", "daemon");
    set.get(&d1, "/repositories/priv/dists/daemon/InRelease")
        .await;
    credential_in(set.dir.path(), "priv-credential", "Basic cm90YXRlZDoxMjM=");
    let (status, _, _) = set
        .get(&d1, "/repositories/priv/dists/daemon/InRelease")
        .await;
    assert_eq!(status, StatusCode::OK);
    let heads = set.fake.heard();
    assert!(heads[0].contains("authorization: Bearer s3cret-token-value\r\n"));
    assert!(heads[1].contains("authorization: Basic cm90YXRlZDoxMjM=\r\n"));
}

/// Proves: another group's path, a repository that is public, and one the registry does not hold
/// are 404 with the exact body of a path that does not exist, the origin is not asked, and each
/// is logged at warn with the node id and the path, by exact equality of the line; the same path
/// on the private surface is 404 too.
///
/// Catches: a host that learns another group's files, or whether a name exists.
#[tokio::test]
async fn what_a_host_may_not_read_is_a_404_and_a_warning() {
    let set = set_up("serve-refused", |fake| fake.dial(Arc::new(serving)));
    let log = Log::default();
    let _guard = log.install();
    let d1 = host("d1", "daemon");
    let paths = [
        "/repositories/priv/dists/storage/InRelease",
        "/repositories/priv/pool/storage/main/w/web.deb",
        "/repositories/pub/dists/daemon/InRelease",
        "/repositories/nothere/dists/daemon/InRelease",
        "/repositories/priv/dists//InRelease",
    ];
    for path in paths {
        let (status, _, body) = set.get(&d1, path).await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes()),
            "{path}"
        );
    }
    assert_eq!(set.fake.dials(), 0);
    let expected: Vec<String> = paths
        .iter()
        .map(|path| {
            format!(
                " WARN hostkeeper_coordinator::repository_serve: refused a read of a repository: it is not a private repository of the host's group node_id=d1 path={path}"
            )
        })
        .collect();
    assert_eq!(lines(&log), expected);

    // The private surface does not serve the route at all.
    let request = Request::builder()
        .uri("/repositories/priv/dists/daemon/InRelease")
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let (status, _, body) = raw(&set.served.coordinator, Surface::Control, &d1, request).await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes())
    );
}

/// Proves: a path that breaks the locator grammar, with `..`, `.`, an empty segment, a `%`, no
/// file, or a kind other than `dists` and `pool`, is the same 404, with no warning, and the origin
/// is not asked.
///
/// Catches: a path that leaves the repository.
#[tokio::test]
async fn a_path_that_leaves_the_grammar_is_a_404_the_origin_never_sees() {
    let set = set_up("serve-grammar", |fake| fake.dial(Arc::new(serving)));
    let log = Log::default();
    let _guard = log.install();
    let d1 = host("d1", "daemon");
    for path in [
        "/repositories/priv/dists/daemon/../../secret",
        "/repositories/priv/dists/daemon/.",
        "/repositories/priv/dists/daemon/main//Packages",
        "/repositories/priv/dists/daemon/a%2Fb",
        "/repositories/priv/dists/daemon",
        "/repositories/priv/dists/daemon/",
        "/repositories/priv/other/daemon/InRelease",
        "/repositories/Bad_Name/dists/daemon/InRelease",
        "/repositories/",
        "/repositories/priv",
    ] {
        let (status, _, body) = set.get(&d1, path).await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes()),
            "{path}"
        );
    }
    assert_eq!(set.fake.dials(), 0);
    assert_eq!(lines(&log), Vec::<String>::new());
}

/// Proves: a method other than `GET` is 405 `method_not_allowed`, a request with no certificate or
/// with one that names no host is 403 `forbidden`, and a host an operator denied, retiring or
/// retired, is 403 `denied`; none reaches the origin.
///
/// Catches: a repository read by a caller that is no admitted host.
#[tokio::test]
async fn only_an_admitted_host_may_get() {
    let set = set_up("serve-callers", |fake| fake.dial(Arc::new(serving)));
    let d1 = host("d1", "daemon");
    let path = "/repositories/priv/dists/daemon/InRelease";
    for method in [Method::POST, Method::HEAD, Method::PUT, Method::DELETE] {
        let (status, _, body) = set.send(&d1, method, path).await;
        assert_eq!(
            (status, &body[..]),
            (
                StatusCode::METHOD_NOT_ALLOWED,
                br#"{"error":"method_not_allowed"}"#.as_slice()
            )
        );
    }
    let stranger = peer(&["dns:d1"]);
    for who in [None, stranger] {
        let (status, _, body) = set.get(&who, path).await;
        assert_eq!(
            (status, &body[..]),
            (
                StatusCode::FORBIDDEN,
                br#"{"error":"forbidden"}"#.as_slice()
            )
        );
    }
    assert_eq!(set.fake.dials(), 0);
}

/// Proves: a host an operator denied is 403 `denied` while it retires and once it retired, and
/// an admitted host of the same coordinator is still served.
///
/// Catches: a denied host that keeps reading a private repository.
#[tokio::test]
async fn a_denied_host_is_refused_denied() {
    let fake = FakeOrigin::new();
    let dir = scratch("serve-denied");
    let file = credential_in(dir.path(), "priv-credential", TOKEN);
    let client = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(fake.dial(Arc::new(serving)));
    let mut enrollment = None;
    let served = served_built("serve-denied", |scratch, clock| {
        let held = enrollment_in(scratch);
        enrollment = Some(Arc::clone(&held));
        Rig::start(scratch, clock)
            .with_enrollment(held)
            .with_repository_origin(client)
    });
    let enrollment = enrollment.expect("enrollment");
    served
        .coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(private("priv", URI, &["daemon"], 1, &file)),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
    let get = |who: Peer| {
        let coordinator = Arc::clone(&served.coordinator);
        async move {
            let request = Request::builder()
                .uri("/repositories/priv/dists/daemon/InRelease")
                .body(Full::new(Bytes::new()))
                .expect("a request");
            let (status, _, body) = raw(&coordinator, Surface::Heartbeat, &who, request).await;
            (status, String::from_utf8(body.to_vec()).expect("text"))
        }
    };
    let (d1, d2) = (host("d1", "daemon"), host("d2", "daemon"));
    assert_eq!(get(d1.clone()).await.0, StatusCode::OK);
    let log = Log::default();
    let _guard = log.install();
    enrollment.store().deny(&host_id("d1")).expect("denied");
    assert_eq!(
        get(d1.clone()).await,
        (StatusCode::FORBIDDEN, r#"{"error":"denied"}"#.to_owned())
    );
    enrollment.store().retire(&host_id("d1")).expect("retired");
    assert_eq!(
        get(d1).await,
        (StatusCode::FORBIDDEN, r#"{"error":"denied"}"#.to_owned())
    );
    assert_eq!(get(d2).await.0, StatusCode::OK);
    assert_eq!(
        lines(&log)[0],
        " WARN hostkeeper_coordinator::repository_serve: refused a read of a repository: the host is denied node_id=d1"
    );
}

/// Proves: the origin's 404 is the host's 404 with the body of a path that does not exist, and no
/// warning.
///
/// Catches: a file the origin lacks reported as a failure of the coordinator.
#[tokio::test]
async fn the_origins_404_is_the_hosts_404() {
    let set = set_up("serve-origin-404", |fake| fake.dial(Arc::new(serving)));
    let log = Log::default();
    let _guard = log.install();
    let (status, _, body) = set
        .get(
            &host("d1", "daemon"),
            "/repositories/priv/dists/daemon/gone",
        )
        .await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes())
    );
    assert_eq!(lines(&log), Vec::<String>::new());
}

/// Proves: an origin that answers 500 or redirects, one that cannot be connected to, one that gives
/// no `Content-Length`, one that never answers, a credential file that is unusable and a
/// coordinator with no origin client are each 502 `origin_failed`, with one warn line that names
/// the repository, the path under the origin and the kind of failure, by exact equality, and the
/// credential's bytes are in no line.
///
/// Catches: a failure the host cannot tell from a missing file, or a credential in a log line.
#[tokio::test(start_paused = true)]
async fn an_origin_that_fails_is_a_502_and_a_logged_kind() {
    type Dialer = fn(&FakeOrigin) -> hostkeeper_oidc::Dial;
    let cases: [(&str, Dialer, &str); 5] = [
        (
            "serve-500",
            |fake| fake.dial_all(Then::Write(response("500 Oops", b""))),
            "the origin answered 500",
        ),
        (
            "serve-redirect",
            |fake| fake.dial_all(Then::Write(response("302 Found", b""))),
            "the origin answered 302",
        ),
        (
            "serve-refused",
            |_| refusing_dial(),
            "the connection failed",
        ),
        (
            "serve-no-length",
            |fake| {
                fake.dial_all(Then::Write(
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc".to_vec(),
                ))
            },
            "the origin gave no Content-Length",
        ),
        ("serve-hang", |_| hanging_dial(), "the connection timed out"),
    ];
    for (name, dial, why) in cases {
        let set = set_up(name, dial);
        let log = Log::default();
        let _guard = log.install();
        let (status, _, body) = set
            .get(
                &host("d1", "daemon"),
                "/repositories/priv/dists/daemon/InRelease",
            )
            .await;
        assert_eq!(
            (status, &body[..]),
            (StatusCode::BAD_GATEWAY, ORIGIN_FAILED.as_bytes()),
            "{name}"
        );
        assert_eq!(
            lines(&log),
            [format!(
                " WARN hostkeeper_coordinator::repository_serve: the origin of a private repository failed repository=priv path=dists/daemon/InRelease why={why}"
            )],
            "{name}"
        );
        assert!(!log.text().contains("s3cret"));
    }
}

/// Proves: a credential file that cannot be used when the host asks (gone, readable by others,
/// two lines) and a coordinator with no origin client are 502 `origin_failed`, each with the one
/// warn line that names its kind, by exact equality, and no byte of the file; the origin is not
/// asked.
///
/// Catches: a request sent to the origin with a credential nobody vetted, or a credential in a log.
#[tokio::test]
async fn an_unusable_credential_or_no_origin_client_is_a_502() {
    use std::os::unix::fs::PermissionsExt as _;
    let set = set_up("serve-credential", |fake| fake.dial(Arc::new(serving)));
    let path = "/repositories/priv/dists/daemon/InRelease";
    let d1 = host("d1", "daemon");
    let log = Log::default();
    let _guard = log.install();
    let credential = set.dir.path().join("priv-credential");
    let failed = (StatusCode::BAD_GATEWAY, ORIGIN_FAILED.to_owned());
    let ask = || async {
        let (status, _, body) = set.get(&d1, path).await;
        (status, String::from_utf8(body.to_vec()).expect("text"))
    };
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o644)).expect("mode");
    assert_eq!(ask().await, failed);
    std::fs::remove_file(&credential).expect("removed");
    assert_eq!(ask().await, failed);
    std::fs::write(&credential, "Bearer a\nBearer b\n").expect("two lines");
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).expect("mode");
    assert_eq!(ask().await, failed);
    let why = [
        "credential_file: its mode must be 0600 or 0400",
        "credential_file: entity not found",
        "the credential file must hold one line of 1 to 4096 printable ASCII characters",
    ];
    assert_eq!(
        lines(&log),
        why.map(|why| format!(
            " WARN hostkeeper_coordinator::repository_serve: the origin of a private repository failed repository=priv path=dists/daemon/InRelease why={why}"
        ))
    );
    assert_eq!(set.fake.dials(), 0);
    assert!(!log.text().contains("s3cret"));

    // No origin client at all.
    credential_in(set.dir.path(), "priv-credential", TOKEN);
    let bare = served_built("serve-no-origin", Rig::start);
    let _again = log.install();
    bare.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(private("priv", URI, &["daemon"], 1, &set.file)),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
    let request = Request::builder()
        .uri(path)
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let (status, _, body) = raw(&bare.coordinator, Surface::Heartbeat, &d1, request).await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::BAD_GATEWAY, ORIGIN_FAILED.as_bytes())
    );
    assert_eq!(
        lines(&log).last().map(String::as_str),
        Some(
            " WARN hostkeeper_coordinator::repository_serve: the origin of a private repository failed repository=priv path=dists/daemon/InRelease why=the coordinator has no origin client"
        )
    );
}

/// Proves: the group is looked up at each request: a host that presents a certificate naming
/// another role is served that group's paths and no longer the old group's, with the old group's
/// path answered as any other group's.
///
/// Catches: a host that keeps reading the group it left.
#[tokio::test]
async fn a_host_that_changes_group_is_served_the_new_groups_paths_only() {
    let set = set_up("serve-moved", |fake| fake.dial(Arc::new(serving)));
    let old = "/repositories/priv/dists/daemon/InRelease";
    let new = "/repositories/priv/dists/storage/InRelease";
    let before = host("d1", "daemon");
    assert_eq!(set.get(&before, old).await.0, StatusCode::OK);
    assert_eq!(set.get(&before, new).await.0, StatusCode::NOT_FOUND);
    let after = host("d1", "storage");
    assert_eq!(set.get(&after, old).await.0, StatusCode::NOT_FOUND);
    let (status, _, _) = set.get(&after, new).await;
    // The origin has no such file, and says so with a 500 in this fake: the group was served.
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(set.fake.heard().len(), 2);
    assert!(set.fake.heard()[1].starts_with("GET /apt/dists/storage/InRelease "));
}

/// Proves: a host whose group the repository does not list (`frontend`) is refused as another
/// group's reader is, though it asks for the path of its own group.
///
/// Catches: a repository served to a group it was not put for.
#[tokio::test]
async fn a_group_the_repository_does_not_list_is_not_served() {
    let set = set_up("serve-unlisted", |fake| fake.dial(Arc::new(serving)));
    let (status, _, body) = set
        .get(
            &host("f1", "frontend"),
            "/repositories/priv/dists/frontend/InRelease",
        )
        .await;
    assert_eq!(
        (status, &body[..]),
        (StatusCode::NOT_FOUND, NOT_FOUND.as_bytes())
    );
    assert_eq!(set.fake.dials(), 0);
}

/// Proves: once the store holds a host group, a host is served the paths of the group its
/// enrollment record holds, not the role its certificate names: `d1`, whose certificate names
/// `daemon` and whose record is moved into `storage`, is served `storage`'s path and refused
/// `daemon`'s; `d2`, enrolled and in no group, is refused both; before any group exists the
/// certificate's role still decides.
///
/// Catches: a group-mode host that is refused the suite its own repository set names, because
/// the route read the role of its certificate.
#[tokio::test]
async fn in_group_mode_a_host_is_served_the_group_its_record_holds() {
    let fake = FakeOrigin::new();
    let dir = scratch("serve-group-mode");
    let file = credential_in(dir.path(), "priv-credential", TOKEN);
    let client = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(fake.dial(Arc::new(serving)));
    let mut enrollment = None;
    let served = served_built("serve-group-mode", |scratch, clock| {
        let held = enrollment_in(scratch);
        enrollment = Some(Arc::clone(&held));
        Rig::start(scratch, clock)
            .with_enrollment(held)
            .with_repository_origin(client)
    });
    let enrollment = enrollment.expect("enrollment");
    served
        .coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(private("priv", URI, &["daemon", "storage"], 1, &file)),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
    enroll_host(&enrollment, "d1");
    enroll_host(&enrollment, "d2");
    let status = |who: &Peer, group: &'static str| {
        let coordinator = Arc::clone(&served.coordinator);
        let who = who.clone();
        async move {
            let request = Request::builder()
                .uri(format!("/repositories/priv/dists/{group}/InRelease"))
                .body(Full::new(Bytes::new()))
                .expect("a request");
            raw(&coordinator, Surface::Heartbeat, &who, request).await.0
        }
    };
    let (d1, d2) = (host("d1", "daemon"), host("d2", "daemon"));
    assert_eq!(status(&d1, "daemon").await, StatusCode::OK);

    served
        .coordinator
        .host_groups_put(&operator(), put_of(body("storage", &[SELF], None), None))
        .expect("put group");
    served
        .coordinator
        .host_groups_move(&operator(), move_of("d1", "storage"))
        .expect("moved");

    assert_eq!(status(&d1, "daemon").await, StatusCode::NOT_FOUND);
    // The origin has no such file, and says so with a 500 in this fake: the group was served.
    assert_eq!(status(&d1, "storage").await, StatusCode::BAD_GATEWAY);
    assert!(fake.heard()[1].starts_with("GET /apt/dists/storage/InRelease "));
    assert_eq!(status(&d2, "daemon").await, StatusCode::NOT_FOUND);
    assert_eq!(status(&d2, "storage").await, StatusCode::NOT_FOUND);
    assert_eq!(fake.heard().len(), 2);
}
