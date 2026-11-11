//! The MDM setup through the router: the writes are a signed-in user's alone, the reads are the
//! token's too, no answer holds a secret, and the MDM listener answers `404` to every path outside
//! the table of its routes (`docs/design/coordinator-mdm.md`, sections 4.1, 4.9 and 4.11).

use base64::Engine as _;
use bytes::Bytes;
use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::Full;
use hyper::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tonic::Code;

use super::credentials::operator_paths;
use super::support::{call, grpc_request};
use crate::grpc::{SESSION_ONLY, operator_limit};
use crate::http::Surface;
use crate::login_support::{PASSWORD, World, counting, settings};
use crate::mdm_secrets::MasterKey;
use crate::mdm_setup::MdmSetup;
use crate::testutil::{OPERATOR, ci_token};
use crate::ui::UiDir;

const PUT_SECRET: &str = "/hostkeeper.operator.v1.Mdm/PutSecret";
const PUT_SETTING: &str = "/hostkeeper.operator.v1.Mdm/PutSetting";
const LIST_SECRETS: &str = "/hostkeeper.operator.v1.Mdm/ListSecrets";
const LIST_SETTINGS: &str = "/hostkeeper.operator.v1.Mdm/ListSettings";
const LIST_DEVICES: &str = "/hostkeeper.operator.v1.Mdm/ListDevices";

const MATERIAL: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAgatewaymarker\n-----END RSA PRIVATE KEY-----\n";

/// A world with a master key and a web app directory.
fn world(name: &str) -> World {
    World::build(name, settings(), counting(), |coordinator, rig| {
        std::fs::write(rig.dir.path().join("index.html"), "<html></html>").expect("an index");
        coordinator
            .with_mdm_setup(
                MdmSetup::volatile().with_key(MasterKey::from_bytes(&[5; 32]).expect("a key")),
            )
            .with_ui(Some(UiDir::new(rig.dir.path()).expect("a ui directory")))
    })
}

fn secret_body() -> String {
    json!({
        "kind": "push_key",
        "material": base64::engine::general_purpose::STANDARD.encode(MATERIAL),
        "notAfterUnix": "2000000000",
    })
    .to_string()
}

fn bearer_post(path: &str, body: &str, token: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request")
}

/// The body of the call of `path` that a write of the setup takes.
fn write_body(path: &str) -> String {
    if path == PUT_SECRET {
        secret_body()
    } else {
        json!({ "name": "tls_renew_days", "value": "12" }).to_string()
    }
}

/// The paths of the `Mdm` service, split by whether the method reads: its name starts with `List`.
fn mdm_paths() -> (Vec<&'static str>, Vec<&'static str>) {
    let all = COMMANDS
        .iter()
        .filter(|command| command.service == "Mdm")
        .map(|command| (command.method.starts_with("List"), command.path));
    let reads = all
        .clone()
        .filter(|(read, _)| *read)
        .map(|(_, p)| p)
        .collect();
    let writes = all.filter(|(read, _)| !*read).map(|(_, p)| p).collect();
    (reads, writes)
}

/// Proves: the session-only list is exactly the writes of the `Mdm` service (every method that does
/// not list), so that a setup write added later and not put on the list fails here, and no read is
/// on it.
///
/// Catches: a setup write an API token can call.
#[test]
fn every_write_of_the_mdm_service_is_session_only_and_no_read_is() {
    let (reads, writes) = mdm_paths();
    assert_eq!(reads, [LIST_DEVICES, LIST_SECRETS, LIST_SETTINGS]);
    assert_eq!(writes, [PUT_SECRET, PUT_SETTING]);
    for path in &writes {
        assert!(SESSION_ONLY.contains(path), "{path}");
    }
    for path in &reads {
        assert!(!SESSION_ONLY.contains(path), "{path}");
    }
}

/// Proves: a token gets `PERMISSION_DENIED` on every write of the setup (`session_required` for the
/// operator's token, by the JSON gateway and by gRPC; `forbidden` for CI's, which the `Mdm` service
/// never takes), and nothing is stored, audited or changed; every read of the service is answered
/// to the operator's token on both routes.
///
/// Catches: a setup write that an API token a script holds can make, which could erase every Mac.
#[tokio::test]
async fn a_token_is_refused_every_setup_write_and_allowed_every_read() {
    let world = world("mdm-setup-token");
    let (reads, writes) = mdm_paths();
    for path in &writes {
        let sent = world
            .send(bearer_post(path, &write_body(path), OPERATOR))
            .await;
        assert_eq!(sent.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            sent.json(),
            json!({ "code": "PERMISSION_DENIED", "reason": "session_required" }),
            "{path}"
        );
        let message = if *path == PUT_SECRET {
            grpc_request(
                path,
                proto::PutSecretRequest {
                    kind: "push_key".to_owned(),
                    material: MATERIAL.to_vec(),
                    not_after_unix: None,
                },
                Some(OPERATOR),
            )
        } else {
            grpc_request(
                path,
                proto::PutSettingRequest {
                    name: "tls_renew_days".to_owned(),
                    value: "12".to_owned(),
                },
                Some(OPERATOR),
            )
        };
        let answer = call(&world.coordinator, Surface::Control, &None, message).await;
        assert_eq!(
            answer,
            (Code::PermissionDenied, "session_required".to_owned(), None),
            "{path}"
        );
    }
    // CI's token is good once: it is refused, as the `Mdm` service never takes it.
    let sent = world
        .send(bearer_post(PUT_SECRET, &secret_body(), &ci_token()))
        .await;
    assert_eq!(
        (sent.status, sent.json()),
        (
            StatusCode::FORBIDDEN,
            json!({ "code": "PERMISSION_DENIED", "reason": "forbidden" })
        )
    );
    assert!(world.audit().is_empty(), "a refused call is not a change");
    assert!(
        world
            .coordinator
            .mdm_setup()
            .list_secrets()
            .expect("listed")
            .is_empty()
    );
    assert!(
        world
            .coordinator
            .mdm_setup()
            .settings()
            .expect("set")
            .is_empty()
    );

    for path in &reads {
        let sent = world.send(bearer_post(path, "{}", OPERATOR)).await;
        assert_eq!(sent.status, StatusCode::OK, "{path}: {sent:?}");
        let message = match *path {
            LIST_DEVICES => {
                grpc_request(path, proto::ListDevicesRequest::default(), Some(OPERATOR))
            }
            LIST_SECRETS => grpc_request(path, proto::ListSecretsRequest {}, Some(OPERATOR)),
            _ => grpc_request(path, proto::ListSettingsRequest {}, Some(OPERATOR)),
        };
        let (code, said, _) = call(&world.coordinator, Surface::Control, &None, message).await;
        assert_eq!((code, said.as_str()), (Code::Ok, ""), "{path}");
    }
}

/// Proves: a signed-in user makes every setup write through the JSON gateway as themselves, the
/// answers carry the kind, version, fingerprint and times and nothing of the value, the audit holds
/// the user's name, and the user's reads show the same; a user whose session fails the CSRF check
/// changes nothing.
///
/// Catches: a setup write only a script could make, a secret returned in an answer, and an audit
/// entry under the wrong name.
#[tokio::test]
async fn a_session_makes_the_setup_writes_and_no_answer_shows_the_secret() {
    let world = world("mdm-setup-session");
    let alice = world.sign_in("alice", PASSWORD).await;
    let put = world.send(alice.post(PUT_SECRET, &secret_body())).await;
    assert_eq!(put.status, StatusCode::OK, "{put:?}");
    let answer = put.json();
    let secret = answer["secret"].clone();
    assert_eq!(secret["kind"], "push_key");
    assert_eq!(secret["version"], 1);
    assert_eq!(secret["notAfterUnix"], "2000000000");
    assert_eq!(answer.as_object().map(|o| o.len()), Some(1));
    let fingerprint = secret["fingerprintSha256"].as_str().expect("a fingerprint");
    assert_eq!(fingerprint.len(), 64);

    let set = world
        .send(alice.post(PUT_SETTING, &write_body(PUT_SETTING)))
        .await;
    assert_eq!(
        set.json(),
        json!({ "name": "tls_renew_days", "value": "12" })
    );

    let listed = world.send(alice.post(LIST_SECRETS, "{}")).await;
    let by_token = world.send(bearer_post(LIST_SECRETS, "{}", OPERATOR)).await;
    assert_eq!(listed.json(), by_token.json());
    assert_eq!(listed.json()["secrets"], json!([secret]));
    assert_eq!(listed.json()["sealing"], true);

    let base64 = base64::engine::general_purpose::STANDARD.encode(MATERIAL);
    for text in [&put.body, &set.body, &listed.body, &by_token.body] {
        let text = String::from_utf8_lossy(text);
        for needle in [
            base64.as_str(),
            "gatewaymarker",
            "RSA PRIVATE",
            &hex::encode(MATERIAL),
        ] {
            assert!(!text.contains(needle), "{needle} is in {text}");
        }
    }
    let said: Vec<(String, String)> = world
        .said()
        .into_iter()
        .map(|(by, action, _, _)| (by, action))
        .collect();
    assert_eq!(
        said,
        [
            ("alice".to_owned(), "login".to_owned()),
            ("alice".to_owned(), "mdm_push_cert_put".to_owned()),
            ("alice".to_owned(), "mdm_setting_set".to_owned()),
        ]
    );

    let mut forged = alice.post(PUT_SECRET, &secret_body());
    forged.headers_mut().remove("x-hk-csrf");
    let sent = world.send(forged).await;
    assert_eq!(
        sent.json(),
        json!({ "code": "PERMISSION_DENIED", "reason": "csrf" })
    );
    assert_eq!(world.said().len(), 3);
    assert_eq!(
        world
            .coordinator
            .mdm_setup()
            .list_secrets()
            .expect("listed")
            .len(),
        1
    );
}

/// Proves: a path that is not a route of the MDM listener's table answers `404 not_found` on that
/// listener whatever the method and whatever credential comes with it: every method of the
/// operator API (the setup writes and reads among them) by gRPC and by the JSON gateway, `/v1/*`,
/// the sign-in routes, the heartbeat, `/ui/`, the health path and the root; the same paths on the
/// private listener are answered, so the 404 is the listener's.
///
/// Catches: an operator API, a sign-in or a web app on the listener the Macs reach.
#[tokio::test]
async fn the_mdm_listener_answers_404_to_everything_outside_its_table() {
    let world = world("mdm-setup-listener");
    let alice = world.sign_in("alice", PASSWORD).await;
    let not_found = json!({ "error": "not_found" });
    let plain = [
        "/v1/heartbeat",
        "/v1/desired",
        "/v1/operator",
        "/v1/status",
        "/v1/convergence",
        "/v1/version",
        "/v1/login",
        "/v1/logout",
        "/v1/session",
        "/v1/password",
        "/ui/",
        "/ui/index.html",
        "/healthz",
        "/",
        "/repositories/a/dists/g/x",
    ];
    for method in [Method::GET, Method::POST, Method::PUT, Method::OPTIONS] {
        for path in plain {
            for credential in ["none", "token", "session"] {
                let mut request = Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header("host", crate::login_support::HOST)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from_static(b"{}")))
                    .expect("a request");
                match credential {
                    "token" => {
                        request.headers_mut().insert(
                            "authorization",
                            format!("Bearer {OPERATOR}").parse().expect("a value"),
                        );
                    }
                    "session" => {
                        let from_page = alice.post(path, "{}");
                        for name in ["cookie", "x-hk-csrf", "origin", "sec-fetch-site"] {
                            request
                                .headers_mut()
                                .insert(name, from_page.headers()[name].clone());
                        }
                    }
                    _ => {}
                }
                let sent = world.send_on(Surface::Mdm, request).await;
                assert_eq!(
                    sent.status,
                    StatusCode::NOT_FOUND,
                    "{method} {path} {credential}"
                );
                assert_eq!(sent.json(), not_found, "{method} {path} {credential}");
            }
        }
    }
    let mut every = operator_paths();
    every.extend([PUT_SECRET, PUT_SETTING, LIST_SECRETS, LIST_SETTINGS]);
    for path in every {
        let json_call = bearer_post(path, "{}", OPERATOR);
        let sent = world.send_on(Surface::Mdm, json_call).await;
        assert_eq!(sent.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(sent.json(), not_found, "{path}");
        let session_call = alice.post(path, &write_body(path));
        let sent = world.send_on(Surface::Mdm, session_call).await;
        assert_eq!(sent.status, StatusCode::NOT_FOUND, "{path} with a session");
        let grpc = grpc_request(path, (), Some(OPERATOR));
        let answered = world.send_on(Surface::Mdm, grpc).await;
        assert_eq!(answered.status, StatusCode::NOT_FOUND, "{path} by gRPC");
        assert_eq!(answered.json(), not_found, "{path} by gRPC");
    }
    assert_eq!(
        world.said().len(),
        1,
        "nothing was done on the MDM listener"
    );
    assert!(
        world
            .coordinator
            .mdm_setup()
            .list_secrets()
            .expect("listed")
            .is_empty()
    );

    let ui = world
        .send_on(
            Surface::Control,
            Request::builder()
                .uri("/ui/")
                .body(Full::new(Bytes::new()))
                .expect("a request"),
        )
        .await;
    assert_eq!(
        ui.status,
        StatusCode::OK,
        "the private listener serves the web app"
    );
    let version = world
        .send_on(
            Surface::Control,
            Request::builder()
                .uri("/v1/version")
                .body(Full::new(Bytes::new()))
                .expect("a request"),
        )
        .await;
    assert_eq!(version.status, StatusCode::OK);
    let setup: Value = world.send(alice.post(LIST_SETTINGS, "{}")).await.json();
    assert_eq!(setup["settings"].as_array().map(Vec::len), Some(5));
}

/// Proves: a signed-in user's `PutSecret` through the gateway reads a value of 32 KiB (the most
/// a secret may be) and stores it, a value one byte over is refused `invalid_request` with nothing
/// stored, a body past 64 KiB is refused `invalid_request`, and the operator limit of `PutSecret`
/// is 64 KiB while every other method of the service reads 4 KiB.
///
/// Catches: a service that decodes with the 4 KiB limit of the other methods, which would refuse a
/// TLS chain, a 4096-bit key with its certificate or an ABM token before the audit or the store.
#[tokio::test]
async fn a_session_uploads_a_secret_of_32_kib_and_no_more() {
    assert_eq!(operator_limit(PUT_SECRET), 65_536);
    for path in [PUT_SETTING, LIST_SECRETS, LIST_SETTINGS, LIST_DEVICES] {
        assert_eq!(operator_limit(path), 4096, "{path}");
    }
    let world = world("mdm-setup-big");
    let alice = world.sign_in("alice", PASSWORD).await;
    let of = |size: usize| {
        json!({
            "kind": "tls_cert",
            "material": base64::engine::general_purpose::STANDARD.encode(vec![b'a'; size]),
        })
        .to_string()
    };
    let at_most = world.send(alice.post(PUT_SECRET, &of(32 * 1024))).await;
    assert_eq!(at_most.status, StatusCode::OK, "{at_most:?}");
    assert_eq!(at_most.json()["secret"]["kind"], "tls_cert");
    assert_eq!(at_most.json()["secret"]["version"], 1);

    let over = world.send(alice.post(PUT_SECRET, &of(32 * 1024 + 1))).await;
    assert_eq!(over.json()["reason"], "invalid_request", "{over:?}");
    let too_long = world.send(alice.post(PUT_SECRET, &of(64 * 1024))).await;
    assert_eq!(too_long.json()["reason"], "invalid_request", "{too_long:?}");
    assert_eq!(
        world
            .coordinator
            .mdm_setup()
            .list_secrets()
            .expect("listed")
            .len(),
        1
    );
}
