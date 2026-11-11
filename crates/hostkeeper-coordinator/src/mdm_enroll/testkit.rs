//! What the enrollment tests share: a rig whose Apple is fake, a blob for a serial, and a way to
//! post it and read the profile that comes back.

use std::net::IpAddr;

use http_body_util::BodyExt as _;
use hyper::{Method, StatusCode};

use super::fake_apple::{self, Spec};
use super::{enroll, scep};
use crate::mdm_server::support::Rig;

/// The peer the tests post from.
pub(super) const PEER: &str = "192.0.2.10";

pub(super) const SERIAL: &str = crate::mdm_server::support::SERIAL;
pub(super) const UDID: &str = crate::mdm_server::support::UDID;

/// A rig that serves both families, trusts the fake Apple and names itself `localhost`, with
/// [`SERIAL`] listed in the ABM table.
pub(super) fn rig(name: &str) -> Rig {
    let rig = Rig::with_apple(name, fake_apple::roots());
    rig.list_in_abm(SERIAL);
    rig
}

/// A blob that verifies against the fake Apple, naming `serial` and `udid`.
pub(super) fn blob_for(serial: &str, udid: &str) -> Vec<u8> {
    fake_apple::blob(&Spec {
        content: fake_apple::plist(serial, udid),
        ..Spec::default()
    })
}

/// A blob of the same shape whose signature does not verify.
pub(super) fn forged_for(serial: &str, udid: &str) -> Vec<u8> {
    fake_apple::blob(&Spec {
        content: fake_apple::plist(serial, udid),
        break_signature: true,
        ..Spec::default()
    })
}

pub(super) fn peer() -> Option<IpAddr> {
    Some(PEER.parse().expect("an address"))
}

/// `POST /enroll` of `body`: the status and the body.
pub(super) async fn post(rig: &Rig, body: &[u8]) -> (StatusCode, Vec<u8>) {
    request(rig, &Method::POST, body).await
}

/// `method /enroll` with `body`.
pub(super) async fn request(rig: &Rig, method: &Method, body: &[u8]) -> (StatusCode, Vec<u8>) {
    let answer = enroll::answer(&rig.coordinator, &rig.server, peer(), method, body).await;
    let status = answer.status();
    let bytes = answer
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, bytes.to_vec())
}

/// `method /scep?query` with `body`: the status, the content type and the body.
pub(super) async fn scep_request(
    rig: &Rig,
    method: &Method,
    query: Option<&str>,
    body: &[u8],
) -> (StatusCode, Option<String>, Vec<u8>) {
    let answer = scep::answer(&rig.coordinator, &rig.server, peer(), method, query, body).await;
    let status = answer.status();
    let kind = answer
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = answer
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, kind, bytes.to_vec())
}

/// The error token of a refusal body, `{"error":"..."}`.
pub(super) fn error_of(body: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(body).expect("a JSON error");
    value["error"].as_str().expect("a token").to_owned()
}

/// The SCEP challenge of the SCEP payload of a profile.
pub(super) fn challenge_in(profile: &[u8]) -> String {
    let value = plist::Value::from_reader(std::io::Cursor::new(profile)).expect("a profile");
    let content = value.as_dictionary().expect("a dictionary")["PayloadContent"]
        .as_array()
        .expect("payloads");
    content
        .iter()
        .filter_map(|payload| payload.as_dictionary())
        .find(|payload| payload["PayloadType"].as_string() == Some("com.apple.security.scep"))
        .and_then(|payload| payload["PayloadContent"].as_dictionary())
        .and_then(|content| content["Challenge"].as_string())
        .expect("a challenge")
        .to_owned()
}
