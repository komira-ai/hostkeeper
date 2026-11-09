//! `GET /v1/version` (`version_route`): through the private surface with no token, and by its
//! answer for a coordinator of a given version.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use serde_json::{Value, json};

use crate::http::{Surface, VERSION_PATH, handle};
use crate::testutil::Rig;
use crate::version_route::answer_for;

const PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

async fn body_of(response: hyper::Response<Full<Bytes>>) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

async fn ask(rig: &Rig, surface: Surface, method: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Full::new(Bytes::new()))
        .expect("a request");
    body_of(handle(&rig.coordinator, surface, &None, request, PATIENCE).await).await
}

#[tokio::test]
async fn the_private_listener_says_its_version_and_commit_with_no_token() {
    let rig = Rig::new("version-plain");
    let (status, body) = ask(&rig, Surface::Control, "GET", VERSION_PATH).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"version": env!("CARGO_PKG_VERSION"), "commit": crate::stamp::BUILD_COMMIT})
    );
}

#[tokio::test]
async fn at_least_its_own_version_is_ok_and_a_newer_one_is_a_conflict_naming_it_older() {
    let rig = Rig::new("version-at-least");
    let own = env!("CARGO_PKG_VERSION");
    for asked in [
        own.to_owned(),
        "0.0.1".to_owned(),
        format!("{own}+0123456789ab"),
    ] {
        let (status, body) = ask(
            &rig,
            Surface::Control,
            "GET",
            &format!("{VERSION_PATH}?at_least={asked}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{asked}");
        assert_eq!(body["version"], own, "{asked}");
    }
    let (status, body) = ask(
        &rig,
        Surface::Control,
        "GET",
        &format!("{VERSION_PATH}?at_least=999.0.0"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({
            "error": "coordinator_older",
            "version": own,
            "commit": crate::stamp::BUILD_COMMIT,
        })
    );
}

#[tokio::test]
async fn another_method_is_not_allowed_and_the_heartbeat_surface_does_not_serve_it() {
    let rig = Rig::new("version-methods");
    for method in ["POST", "PUT", "DELETE"] {
        let (status, body) = ask(&rig, Surface::Control, method, VERSION_PATH).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(body, json!({"error": "method_not_allowed"}), "{method}");
    }
    let (status, body) = ask(&rig, Surface::Heartbeat, "GET", VERSION_PATH).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "not_found"}));
}

#[tokio::test]
async fn the_order_is_by_release_numbers_and_a_build_suffix_does_not_order() {
    let at = |version: &str, asked: &str| answer_for(version, "0123456789ab", Some(asked));
    for (version, asked, want) in [
        ("0.1.222", "at_least=0.1.221", StatusCode::OK),
        ("0.1.222", "at_least=0.1.222", StatusCode::OK),
        ("0.1.222", "at_least=0.1.223", StatusCode::CONFLICT),
        ("0.1.222", "at_least=0.2.0", StatusCode::CONFLICT),
        ("0.1.222", "at_least=0.1.30", StatusCode::OK),
        ("0.1.222", "at_least=0.1.222+ffffffffffff", StatusCode::OK),
        ("1.0.0", "at_least=0.9.999", StatusCode::OK),
        ("0.1.10", "at_least=0.1.9", StatusCode::OK),
    ] {
        let (status, body) = body_of(at(version, asked)).await;
        assert_eq!(status, want, "{version} {asked}");
        assert_eq!(body["version"], version, "{version} {asked}");
        assert_eq!(body["commit"], "0123456789ab", "{version} {asked}");
    }
}

#[tokio::test]
async fn a_query_that_is_not_one_version_is_a_bad_request() {
    for query in [
        "",
        "at_least=",
        "at_least=v0.1.2",
        "at_least=0.1.2&x=1",
        "since=0.1.2",
        "at_least=0.1.2 ",
    ] {
        let (status, body) = body_of(answer_for("0.1.222", "0123456789ab", Some(query))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query:?}");
        assert_eq!(body, json!({"error": "bad_request"}), "{query:?}");
    }
    // A build whose own version does not parse answers no comparison.
    let (status, body) = body_of(answer_for("", "0123456789ab", Some("at_least=0.1.0"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "bad_request"}));
}
