use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::identity::PushIdentity;
use crate::request::pkcs1_pem;
use crate::testkit::{fixture_key, push_certificate_pem};
use crate::tlsfixture::{Auth, Reply, Server};

const TOPIC: &str = "com.apple.mgmt.External.00000000-0000-0000-0000-000000000001";
const MAGIC: &str = "6C1F0E9A-1111-2222-3333-444455556666";

fn identity() -> PushIdentity {
    let key = fixture_key(0);
    let cert = push_certificate_pem(key, TOPIC, 1_000_000, 4_000_000_000);
    PushIdentity::parse(cert.as_bytes(), pkcs1_pem(key).unwrap().expose()).unwrap()
}

fn sender(server: &Server, timeout: Duration) -> Http2Sender {
    let roots = tls::roots_of(std::slice::from_ref(&server.ca)).unwrap();
    let endpoint = Endpoint::new("localhost", 443).dialing(&server.addr.to_string());
    Http2Sender::new(endpoint, roots, &identity(), timeout).unwrap()
}

fn push(priority: u8, expiration_unix: u64) -> Push {
    Push {
        token: DeviceToken::from_raw(&[0xAB; 32]).unwrap(),
        magic: MAGIC.to_owned(),
        topic: TOPIC.to_owned(),
        priority,
        expiration_unix,
    }
}

const LONG: Duration = Duration::from_secs(60);

#[tokio::test]
async fn a_push_posts_the_hex_token_url_with_the_mdm_headers_and_the_magic_body() {
    let server = Server::start(true, Auth::Any).await;
    let sender = sender(&server, LONG);
    let delivered = sender.send(&push(10, 1_700_086_400)).await.unwrap();
    assert_eq!(
        delivered.apns_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].path, format!("/3/device/{}", "ab".repeat(32)));
    assert_eq!(seen[0].header("apns-topic"), Some(TOPIC));
    assert_eq!(seen[0].header("apns-push-type"), Some("mdm"));
    assert_eq!(seen[0].header("apns-priority"), Some("10"));
    assert_eq!(seen[0].header("apns-expiration"), Some("1700086400"));
    assert_eq!(seen[0].body, format!(r#"{{"mdm":"{MAGIC}"}}"#).into_bytes());
    // The push certificate is what the client presented, so that APNs knows the topic.
    let presented = server.client_certs.lock().unwrap().clone();
    assert_eq!(presented, vec![identity().chain[0].as_ref().to_vec()]);
}

#[tokio::test]
async fn the_priority_and_the_expiration_of_the_push_are_the_headers_sent() {
    let server = Server::start(true, Auth::Any).await;
    let sender = sender(&server, LONG);
    sender.send(&push(5, 0)).await.unwrap();
    let seen = server.requests();
    assert_eq!(seen[0].header("apns-priority"), Some("5"));
    assert_eq!(seen[0].header("apns-expiration"), Some("0"));
}

#[tokio::test]
async fn sequential_pushes_reuse_one_connection() {
    let server = Server::start(true, Auth::Any).await;
    let sender = sender(&server, LONG);
    for _ in 0..3 {
        sender.send(&push(10, 1)).await.unwrap();
    }
    assert_eq!(server.requests().len(), 3);
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_push_after_a_goaway_connects_again_and_is_delivered() {
    let server = Server::start(true, Auth::Any).await;
    let mut first = Reply::ok();
    first.then_goaway = true;
    server.answer_next(first);
    let sender = sender(&server, LONG);
    sender.send(&push(10, 1)).await.unwrap();
    // Whether the GOAWAY has reached the client or not by now, the second push is delivered, on a
    // second connection: the reused one is closed, and a request that failed on it is repeated.
    sender.send(&push(10, 1)).await.unwrap();
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn bad_device_token_is_terminal_for_that_token_and_the_setup_is_not_blamed() {
    let server = Server::start(true, Auth::Any).await;
    server.answer_next(Reply::status(400, r#"{"reason":"BadDeviceToken"}"#));
    let sender = sender(&server, LONG);
    let error = sender.send(&push(10, 1)).await.unwrap_err();
    assert_eq!(
        error,
        PushError::BadDeviceToken("BadDeviceToken".to_owned())
    );
    assert!(error.token_is_dead());
    assert!(!error.is_setup_mistake());
    assert!(!error.is_certificate_alert());
    assert!(!error.is_transient());
}

#[tokio::test]
async fn topic_disallowed_is_a_setup_mistake() {
    let server = Server::start(true, Auth::Any).await;
    server.answer_next(Reply::status(400, r#"{"reason":"TopicDisallowed"}"#));
    let error = sender(&server, LONG).send(&push(10, 1)).await.unwrap_err();
    assert_eq!(error, PushError::TopicDisallowed);
    assert!(error.is_setup_mistake());
    assert!(!error.token_is_dead());
}

#[test]
fn every_status_of_apns_maps_to_the_error_the_design_names() {
    let reason = |text: &str| format!(r#"{{"reason":"{text}"}}"#).into_bytes();
    assert_eq!(
        map_answer(400, &reason("DeviceTokenNotForTopic")),
        PushError::BadDeviceToken("DeviceTokenNotForTopic".to_owned())
    );
    // A 403 with no client certificate is not the expiry signal.
    assert_eq!(
        map_answer(403, &reason("MissingProviderToken")),
        PushError::NoClientCertificate
    );
    assert!(PushError::NoClientCertificate.is_setup_mistake());
    assert!(!PushError::NoClientCertificate.is_certificate_alert());
    // Another 403 is taken as a certificate APNs refuses (unverified).
    let refused = map_answer(403, &reason("Forbidden"));
    assert_eq!(
        refused,
        PushError::CertificateRejected("Forbidden".to_owned())
    );
    assert!(refused.is_certificate_alert());
    assert_eq!(
        map_answer(410, &reason("Unregistered")),
        PushError::Unregistered
    );
    assert!(PushError::Unregistered.token_is_dead());
    assert_eq!(map_answer(429, b""), PushError::Throttled);
    assert_eq!(map_answer(503, b""), PushError::Unavailable(503));
    assert_eq!(map_answer(500, b"not json"), PushError::Unavailable(500));
    for transient in [PushError::Throttled, PushError::Unavailable(503)] {
        assert!(transient.is_transient());
        assert!(!transient.token_is_dead());
    }
    assert_eq!(
        map_answer(400, &reason("BadPriority")),
        PushError::Rejected {
            status: 400,
            reason: "BadPriority".to_owned()
        }
    );
    assert_eq!(
        map_answer(413, b""),
        PushError::Rejected {
            status: 413,
            reason: String::new()
        }
    );
}

#[test]
fn the_error_texts_say_what_happened_without_a_token_or_a_magic() {
    assert_eq!(
        PushError::TopicDisallowed.to_string(),
        "APNs refuses the topic: the certificate and the topic do not belong together"
    );
    assert_eq!(
        PushError::Rejected {
            status: 400,
            reason: "BadPriority".to_owned()
        }
        .to_string(),
        "APNs answered 400: BadPriority"
    );
    assert_eq!(
        PushError::Unavailable(503).to_string(),
        "APNs is unavailable (503)"
    );
}

#[test]
fn a_certificate_alert_and_a_goaway_payload_in_an_error_are_a_certificate_alert() {
    let alert = std::io::Error::other("received fatal alert: CertificateExpired");
    assert_eq!(
        connection_error(&alert),
        PushError::CertificateAlert("CertificateExpired".to_owned())
    );
    // The text h2 makes of a GOAWAY whose payload is JSON, the bytes shown escaped.
    let goaway = std::io::Error::other(
        r#"connection error received: not a result of an error (b"{\"reason\":\"BadCertificateEnvironment\"}")"#,
    );
    assert_eq!(
        connection_error(&goaway),
        PushError::CertificateAlert("BadCertificateEnvironment".to_owned())
    );
    assert!(connection_error(&goaway).is_certificate_alert());
    // An alert that is not about the certificate, and a GOAWAY of another reason, are plain
    // connection errors.
    let other = std::io::Error::other("received fatal alert: HandshakeFailure");
    assert_eq!(
        connection_error(&other),
        PushError::Connection("received fatal alert: HandshakeFailure".to_owned())
    );
    let busy = std::io::Error::other(r#"(b"{\"reason\":\"ServiceUnavailable\"}")"#);
    assert!(matches!(connection_error(&busy), PushError::Connection(_)));
    assert!(connection_error(&busy).is_transient());
    let plain = std::io::Error::other("connection reset");
    assert_eq!(
        connection_error(&plain),
        PushError::Connection("connection reset".to_owned())
    );
    let wrapped = std::io::Error::other(plain);
    assert_eq!(
        connection_error(&wrapped),
        PushError::Connection("connection reset".to_owned())
    );
}

#[tokio::test]
async fn a_client_certificate_the_server_refuses_as_expired_is_a_certificate_alert() {
    let server = Server::start(true, Auth::RefuseExpired).await;
    let error = sender(&server, LONG).send(&push(10, 1)).await.unwrap_err();
    assert!(error.is_certificate_alert(), "{error}");
}

#[tokio::test]
async fn a_server_that_is_not_there_is_a_connection_error() {
    let server = Server::start(true, Auth::Any).await;
    let sender = sender(&server, LONG);
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    let roots = tls::roots_of(std::slice::from_ref(&server.ca)).unwrap();
    let endpoint = Endpoint::new("localhost", 443).dialing(&dead.to_string());
    let gone = Http2Sender::new(endpoint, roots, &identity(), LONG).unwrap();
    let error = gone.send(&push(10, 1)).await.unwrap_err();
    assert!(matches!(error, PushError::Connection(_)), "{error}");
    assert!(error.is_transient());
    drop(sender);
}

#[tokio::test]
async fn an_answer_that_never_comes_ends_in_a_connection_error() {
    let server = Server::start(true, Auth::Any).await;
    let mut never = Reply::ok();
    never.hang = true;
    server.answer_next(never);
    let error = sender(&server, Duration::from_millis(200))
        .send(&push(10, 1))
        .await
        .unwrap_err();
    assert!(matches!(error, PushError::Connection(_)), "{error}");
}

#[test]
fn the_debug_of_a_sender_and_an_identity_name_no_key() {
    let server_less = Http2Sender::production(&identity(), LONG).unwrap();
    let text = format!("{server_less:?}");
    assert!(text.contains("api.push.apple.com"));
    let identity_text = format!("{:?}", identity());
    assert!(identity_text.contains(TOPIC));
    assert!(!identity_text.contains("BEGIN"));
}

#[test]
fn a_header_value_with_a_control_character_is_refused_before_anything_is_sent() {
    let mut bad = push(10, 1);
    bad.topic = "line\nbreak".to_owned();
    let endpoint = Endpoint::new("localhost", 443);
    assert!(matches!(
        request_of(&bad, &endpoint),
        Err(PushError::Connection(_))
    ));
}

#[tokio::test]
async fn a_goaway_whose_payload_names_the_certificate_environment_is_a_certificate_alert() {
    // What the spike saw from the sandbox host with a production certificate: no response, an
    // HTTP/2 GOAWAY with a JSON payload.
    let server = Server::start_goaway(r#"{"reason":"BadCertificateEnvironment"}"#).await;
    let error = sender(&server, LONG).send(&push(10, 1)).await.unwrap_err();
    assert_eq!(
        error,
        PushError::CertificateAlert("BadCertificateEnvironment".to_owned())
    );
}
