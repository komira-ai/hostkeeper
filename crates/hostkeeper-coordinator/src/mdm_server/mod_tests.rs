//! The routes of the Macs' listener (`docs/design/coordinator-mdm.md`, section 4.1): which paths it
//! serves, which family needs what, what each route reads of the client certificate, and how the
//! listener serves a connection when the certificate of the name is stored or renewed.

use std::sync::Arc;
use std::time::Duration;

use hyper::StatusCode;
use rustls::SignatureScheme;
use rustls::client::ResolvesClientCert;
use rustls::pki_types::ServerName;
use rustls::sign::CertifiedKey;
use tokio::io::{AsyncReadExt as _, DuplexStream};

use super::support::{
    Ca, Rig, SERIAL, Session, UDID, authenticate, connector, dial_resolving, result, served_leaf,
};
use super::{
    ClientCert, ConnectionTried, Family, MAX_BODY_BYTES, Refusal, client_cert, family_of,
    sweep_every,
};
use crate::mdm_server::auth::Refused;
use crate::store::mdm::{CommandState, MdmError, MdmStore};

/// Proves: the table of routes: the MDM family is the four Mac routes, the proxy's is the two
/// prefixes, nothing else belongs to either; `/enroll` and `/scep` read no client certificate and
/// `/checkin` and `/mdm` need one.
///
/// Catches: `/enroll` made to need a certificate a Mac at Setup Assistant does not have.
#[test]
fn the_route_table_puts_each_path_in_a_family_with_its_certificate_rule() {
    for path in ["/enroll", "/scep", "/checkin", "/mdm"] {
        assert_eq!(family_of(path), Some(Family::Mdm), "{path}");
    }
    for path in ["/apt/main/dists/x/Release", "/pkg/ab.pkg"] {
        assert_eq!(family_of(path), Some(Family::Proxy), "{path}");
    }
    for path in [
        "/",
        "/v1/status",
        "/ui/",
        "/checkin/",
        "/MDM",
        "/apt",
        "/pkg",
    ] {
        assert_eq!(family_of(path), None, "{path}");
        assert_eq!(client_cert(path), None, "{path}");
    }
    assert_eq!(client_cert("/enroll"), Some(ClientCert::Ignored));
    assert_eq!(client_cert("/scep"), Some(ClientCert::Ignored));
    assert_eq!(client_cert("/checkin"), Some(ClientCert::Required));
    assert_eq!(client_cert("/mdm"), Some(ClientCert::Required));
    assert_eq!(client_cert("/apt/x"), None);
}

/// Proves: every request for a path outside the table, for any method and whatever credential it
/// carries (an operator token, a session cookie, a client certificate), is `404 not_found`, and so
/// are the routes whose task has not landed (`/enroll`, `/scep`, the proxy's).
///
/// Catches: the setup rpcs, `/v1/*`, the sign-in routes or the operator web app reachable from
/// the Macs' network.
#[tokio::test]
async fn every_path_outside_the_table_is_not_found_whatever_the_credential() {
    let rig = Rig::new("mod-not-found");
    let mac = rig.mac_ca.issue(1, SERIAL);
    rig.issue(&mac, SERIAL, UDID, false);
    let mut session = Session::dial(&rig, Some(&mac)).await.expect("connected");
    let credentials = [
        ("authorization", "Bearer operator-token".to_owned()),
        ("cookie", "__Host-session=abc".to_owned()),
    ];
    for path in [
        "/",
        "/v1/status",
        "/v1/operator",
        "/v1/desired",
        "/v1/heartbeat",
        "/v1/login",
        "/v1/session",
        "/v1/version",
        "/ui/",
        "/ui/index.html",
        "/hostkeeper.operator.v1.Mdm/PutSecret",
        "/hostkeeper.operator.v1.Mdm/ListDevices",
        "/repositories/x/dists/y/Release",
        "/enroll",
        "/scep",
        "/apt/main/dists/stable/Release",
        "/pkg/ab.pkg",
        "/checkin/",
        "/MDM",
    ] {
        for method in ["GET", "POST", "PUT"] {
            let (status, body) = session.request(method, path, &credentials, b"{}").await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(body, br#"{"error":"not_found"}"#, "{method} {path}");
        }
    }
}

/// Proves: the route families are decided for each request: with the certificate of the name alone
/// the package routes are served (`404` while none is mounted) and the MDM routes answer `503`; once
/// a push identity is stored the MDM routes answer; with no certificate at all neither family is
/// served and no handshake is made.
///
/// Catches: a farm of Linux hosts that never uploads an Apple credential losing its package proxy
/// (package-proxy.md section 3).
#[tokio::test]
async fn the_mdm_routes_answer_503_while_the_package_routes_are_served() {
    let rig = Rig::bare("mod-families");
    assert!(!rig.server.serves(Family::Proxy) && !rig.server.serves(Family::Mdm));
    assert!(
        Session::dial(&rig, None).await.is_none(),
        "no handshake without a certificate"
    );
    rig.store_tls();
    assert!(rig.server.serves(Family::Proxy) && !rig.server.serves(Family::Mdm));
    let mut session = Session::dial(&rig, None).await.expect("connected");
    for path in ["/enroll", "/scep", "/checkin", "/mdm"] {
        let (status, body) = session.put(path, &[], b"x").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(body, br#"{"error":"unavailable"}"#);
    }
    for path in ["/apt/main/dists/stable/Release", "/pkg/ab.pkg"] {
        assert_eq!(
            session.put(path, &[], b"x").await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    rig.store_push();
    assert!(rig.server.serves(Family::Mdm));
    assert_eq!(
        session.put("/checkin", &[], b"x").await.0,
        StatusCode::UNAUTHORIZED
    );
}

/// Proves: a method other than `PUT` on `/checkin` and `/mdm` is `405`, before any credential is
/// read; a body over the limit is a bad request before any credential is read.
///
/// Catches: a `GET` reaching the handlers, and a body of any size read into memory.
#[tokio::test]
async fn only_put_is_taken_and_a_body_has_a_limit() {
    let rig = Rig::new("mod-method-limit");
    let mut session = Session::dial(&rig, None).await.expect("connected");
    for path in ["/checkin", "/mdm"] {
        for method in ["GET", "POST", "DELETE"] {
            let (status, _) = session.request(method, path, &[], b"x").await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        }
        let big = vec![b'a'; MAX_BODY_BYTES + 1];
        assert_eq!(
            session.put(path, &[], &big).await.0,
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
}

/// Proves: each refusal is answered with its status and its code, and the refusals of the
/// credentials and of the store map to them.
///
/// Catches: a `410`, which a Mac may read as the end of its management, answered for an unknown
/// enrollment.
#[test]
fn each_refusal_is_answered_with_its_status() {
    for (refusal, status, code) in [
        (Refusal::Unauthorized, 401, "unauthorized"),
        (Refusal::Forbidden, 403, "forbidden"),
        (Refusal::BadRequest, 400, "bad_request"),
        (Refusal::Unavailable, 503, "unavailable"),
        (Refusal::Internal, 500, "internal"),
    ] {
        assert_eq!(refusal.answer().status().as_u16(), status, "{code}");
    }
    for refused in [
        Refused::Unauthenticated,
        Refused::UnknownIdentity,
        Refused::NotLive,
    ] {
        assert_eq!(Refusal::from(refused), Refusal::Unauthorized);
    }
    assert_eq!(Refusal::from(Refused::OtherDevice), Refusal::Forbidden);
    let store = MdmError::Store("x".to_owned());
    assert_eq!(
        Refusal::from(Refused::Store(store.clone())),
        Refusal::Internal
    );
    assert_eq!(Refusal::from(store), Refusal::Internal);
}

/// Proves: a Mac that is unknown to the table, or that was replaced, is answered `401` and not
/// `410`, and the fake Mac, which removes its profile only on `410`, keeps it.
///
/// Catches: an answer that tells the client to drop management (section 6, lesson 3).
#[tokio::test]
async fn an_unknown_enrollment_keeps_the_fake_macs_state() {
    let rig = Rig::new("mod-unknown");
    let unknown = rig.mac_ca.issue(1, SERIAL);
    let mut profile_installed = true;
    let mut session = Session::dial(&rig, Some(&unknown))
        .await
        .expect("connected");
    for (path, body) in [
        ("/mdm", result("Idle", None)),
        ("/checkin", authenticate(SERIAL)),
    ] {
        let (status, _) = session.put(path, &[], &body).await;
        // What a client does with a status: only 410 removes the profile.
        if status == StatusCode::GONE {
            profile_installed = false;
        }
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
    assert!(profile_installed);
}

/// A client-certificate resolver that records the issuers the server named and offers none.
#[derive(Debug, Default)]
struct Hints(std::sync::Mutex<Option<Vec<Vec<u8>>>>);

impl ResolvesClientCert for Hints {
    fn resolve(
        &self,
        root_hint_subjects: &[&[u8]],
        _: &[SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        *self.0.lock().expect("lock") =
            Some(root_hint_subjects.iter().map(|s| s.to_vec()).collect());
        None
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// Proves: the listener names the issuers of the Macs' identities as the root hints of its
/// certificate request, completes the handshake of a client that offers no certificate, and refuses
/// a client whose certificate does not chain to a trusted issuer.
///
/// Catches: a macOS client that offers no identity because no hint names its issuer (section 12
/// item 10), and a listener that takes a certificate of any issuer.
#[tokio::test]
async fn the_listener_names_the_issuers_as_hints_and_takes_a_client_without_a_certificate() {
    let rig = Rig::new("mod-hints");
    let hints = Arc::new(Hints::default());
    let tls = dial_resolving(&rig, hints.clone())
        .await
        .expect("a handshake without a certificate");
    drop(tls);
    let named = hints
        .0
        .lock()
        .expect("lock")
        .clone()
        .expect("the server asked for a certificate");
    let ca_subject = x509_parser::parse_x509_certificate(&rig.mac_ca.der())
        .map(|(_, c)| c.tbs_certificate.subject.as_raw().to_vec())
        .expect("a certificate");
    assert_eq!(named, vec![ca_subject]);
    // A certificate of an issuer the listener does not trust.
    let stranger = Ca::new().issue(1, SERIAL);
    let mut session = Session::dial(&rig, Some(&stranger))
        .await
        .expect("the handshake completes");
    let refused = session.try_request("PUT", "/checkin", &[], b"x").await;
    assert!(
        refused.is_err(),
        "the server ended the connection: {refused:?}"
    );
}

/// Proves: a certificate stored after the listener started is served from the next handshake, a
/// renewal replaces it for the next handshake while an open connection keeps what it was accepted
/// under, and a connection that arrives while there is none is closed.
///
/// Catches: a renewed certificate that needs a restart, or an expired one served after the
/// operator uploaded a new one.
#[tokio::test]
async fn a_certificate_stored_or_renewed_is_served_from_the_next_handshake() {
    let rig = Rig::bare("mod-hot-swap");
    assert!(rig.server.tls().config().is_none());
    rig.store_tls();
    rig.store_push();
    let first = served_leaf(&rig).await;
    let mut open = Session::dial(&rig, None).await.expect("connected");
    rig.store_tls();
    let second = served_leaf(&rig).await;
    assert_ne!(first, second, "the renewed certificate is the one served");
    assert_eq!(
        open.put("/checkin", &[], b"x").await.0,
        StatusCode::UNAUTHORIZED,
        "the open connection goes on"
    );
}

/// Proves: `serve_mdm` closes a connection that arrives while no certificate is stored and serves
/// the next one once it is, on the surface of the Macs.
///
/// Catches: a listener that answers a handshake it cannot finish, or that stays shut after the
/// certificate is stored.
#[tokio::test]
async fn the_listener_serves_nothing_until_the_certificate_is_stored() {
    let rig = Rig::bare("mod-serve");
    let (connections, accepted) = tokio::sync::mpsc::unbounded_channel::<DuplexStream>();
    let accepted = Arc::new(tokio::sync::Mutex::new(accepted));
    let accept = move || {
        let accepted = Arc::clone(&accepted);
        async move {
            let next = accepted.lock().await.recv().await;
            next.ok_or_else(|| std::io::Error::other("no more connections"))
        }
    };
    tokio::spawn(crate::listen::serve_mdm(
        accept,
        Arc::clone(&rig.coordinator),
        Duration::from_secs(10),
        8,
    ));
    let (mut client, server) = tokio::io::duplex(1024);
    connections.send(server).expect("accepted");
    let mut sink = Vec::new();
    client
        .read_to_end(&mut sink)
        .await
        .expect("closed by the server");
    assert!(sink.is_empty());
    rig.store_tls();
    let (client, server) = tokio::io::duplex(256 * 1024);
    connections.send(server).expect("accepted");
    let tls = connector(&rig, None)
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    drop(tls);
}

/// Proves: the sweep the coordinator runs returns a timed-out command to the queue at each interval:
/// after a command has waited past the timeout, one tick of the interval queues it again.
///
/// Catches: a sweep that is built and never run.
#[tokio::test(start_paused = true)]
async fn the_sweep_runs_at_its_interval() {
    let rig = Rig::new("mod-sweep");
    rig.live("e1", SERIAL, true);
    rig.store
        .enqueue(&super::queue_new(1, "e1"))
        .expect("queued");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.time.advance(86_400);
    tokio::spawn(sweep_every(Arc::clone(&rig.server)));
    // Let the task reach its first wait (the first tick is at once), then move the virtual
    // clock to the next one.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(61)).await;
    let queued = |rig: &Rig| {
        rig.store
            .command(&Rig::command_id(1))
            .expect("read")
            .expect("kept")
            .state
    };
    // Progress events, not time: give the sweep task turns until it has returned the command.
    for _ in 0..1000 {
        if queued(&rig) == CommandState::Queued {
            break;
        }
        tokio::task::yield_now().await;
    }
    let state = rig
        .store
        .command(&Rig::command_id(1))
        .expect("read")
        .expect("kept")
        .state;
    assert_eq!(state, CommandState::Queued);
    // A store that fails is logged and the sweep goes on.
    rig.store.fail(Some("requeue_timed_out"));
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;
}

/// Proves: the commands tried on a connection are listed per enrollment and start empty.
///
/// Catches: one Mac's tried list skipping another Mac's commands.
#[test]
fn the_tried_list_is_kept_for_each_enrollment() {
    let tried = ConnectionTried::default();
    assert!(tried.of("e1").is_empty());
    tried.add("e1", "a");
    tried.add("e1", "b");
    tried.add("e2", "c");
    assert_eq!(tried.of("e1"), ["a", "b"]);
    assert_eq!(tried.of("e2"), ["c"]);
}
