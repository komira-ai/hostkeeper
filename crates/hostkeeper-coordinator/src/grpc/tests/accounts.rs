//! `Users` and `ApiTokens` through the JSON gateway, the way the page and a script call them: a
//! session's call and a bearer's, a stored token's among them, the refusals as the gateway
//! words them, and the audit under the name of whoever called.

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::{Value, json};

use crate::login_support::{PASSWORD, World};
use crate::testutil::{OPERATOR, ci_token};

const USERS: &str = "/hostkeeper.operator.v1.Users";
const TOKENS: &str = "/hostkeeper.operator.v1.ApiTokens";

fn with_bearer(mut request: Request<Full<Bytes>>, token: &str) -> Request<Full<Bytes>> {
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("a value"),
    );
    request
}

/// A bearer's call: the same request a page makes, with the token in place of the cookie.
fn bearer_post(token: &str, path: &str, body: &str) -> Request<Full<Bytes>> {
    with_bearer(crate::login_support::page_post(path, None, body), token)
}

fn refusal(code: &str, reason: &str) -> Value {
    json!({ "code": code, "reason": reason })
}

/// Proves: a signed-in user makes users and tokens through the gateway under their own name, and
/// the answers carry the one-time password and token, the user and the token as the page lists
/// them; the audit holds the user's name as `by` and the user or the token as `subject`.
#[tokio::test]
async fn a_session_makes_users_and_tokens_under_its_own_name() {
    let world = World::new("gateway-accounts-session");
    let alice = world.sign_in("alice", PASSWORD).await;
    let made = world
        .send(alice.post(&format!("{USERS}/Create"), r#"{"name":"carol"}"#))
        .await;
    assert_eq!(made.status, StatusCode::OK, "{made:?}");
    let body = made.json();
    assert_eq!(body["user"]["name"], json!("carol"));
    assert_eq!(body["user"]["mustChange"], json!(true));
    assert_eq!(body["password"].as_str().map(str::len), Some(24));
    let token = world
        .send(alice.post(&format!("{TOKENS}/Create"), r#"{"name":"robot"}"#))
        .await;
    assert_eq!(token.status, StatusCode::OK, "{token:?}");
    let secret = token.json()["secret"]
        .as_str()
        .expect("a secret")
        .to_owned();
    assert!(secret.starts_with("hkt_"));
    let listed = world
        .send(alice.post(&format!("{USERS}/List"), "{}"))
        .await
        .json();
    let names: Vec<&str> = listed["users"]
        .as_array()
        .expect("users")
        .iter()
        .filter_map(|user| user["name"].as_str())
        .collect();
    assert_eq!(names, ["alice", "bob", "carol", "dora", "late", "newbie"]);
    let said: Vec<(String, String, Option<String>)> = world
        .said()
        .into_iter()
        .map(|(by, action, subject, _)| (by, action, subject))
        .collect();
    assert_eq!(
        said[1..],
        [
            (
                "alice".to_owned(),
                "user_create".to_owned(),
                Some("carol".to_owned())
            ),
            (
                "alice".to_owned(),
                "api_token_create".to_owned(),
                Some("robot".to_owned())
            ),
        ]
    );
}

/// Proves: a stored token is a bearer of the gateway under its name: it calls `Users` and
/// `ApiTokens` as a token of the tokens file does, the audit names it, a revoked token is
/// refused 401 `unauthorized`, and the tokens file's token still calls.
#[tokio::test]
async fn a_stored_token_calls_the_gateway_until_it_is_revoked() {
    let world = World::new("gateway-accounts-bearer");
    let made = world
        .send(bearer_post(
            OPERATOR,
            &format!("{TOKENS}/Create"),
            r#"{"name":"robot"}"#,
        ))
        .await
        .json();
    let secret = made["secret"].as_str().expect("a secret").to_owned();
    let created = world
        .send(bearer_post(
            &secret,
            &format!("{USERS}/Create"),
            r#"{"name":"carol"}"#,
        ))
        .await;
    assert_eq!(created.status, StatusCode::OK, "{created:?}");
    let revoked = world
        .send(bearer_post(
            &secret,
            &format!("{TOKENS}/Revoke"),
            r#"{"name":"robot"}"#,
        ))
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{revoked:?}");
    let after = world
        .send(bearer_post(&secret, &format!("{USERS}/List"), "{}"))
        .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(after.json(), refusal("UNAUTHENTICATED", "unauthorized"));
    let still = world
        .send(bearer_post(OPERATOR, &format!("{TOKENS}/List"), "{}"))
        .await;
    assert_eq!(still.status, StatusCode::OK);
    assert_eq!(
        still.json()["tokens"][0]["name"],
        json!("operator"),
        "the tokens file's token is listed and still calls"
    );
    let by: Vec<(String, String)> = world
        .said()
        .into_iter()
        .map(|(by, action, _, _)| (by, action))
        .collect();
    assert_eq!(
        by,
        [
            ("operator".to_owned(), "api_token_create".to_owned()),
            ("robot".to_owned(), "user_create".to_owned()),
            ("robot".to_owned(), "api_token_revoke".to_owned()),
        ]
    );
}

/// Proves: the gateway words each refusal as its token and its gRPC code with the HTTP status of
/// the table: a name that is taken is 409 `name_taken`, one that is no user's 404 `unknown_user`,
/// a CI token 403 `forbidden`, and a reset of one's own password 400 `own_password`.
#[tokio::test]
async fn the_gateway_words_each_refusal_as_its_token() {
    let world = World::new("gateway-accounts-refusals");
    let taken = world
        .send(bearer_post(
            OPERATOR,
            &format!("{USERS}/Create"),
            r#"{"name":"alice"}"#,
        ))
        .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.json(), refusal("ALREADY_EXISTS", "name_taken"));
    let unknown = world
        .send(bearer_post(
            OPERATOR,
            &format!("{USERS}/Delete"),
            r#"{"name":"ghost"}"#,
        ))
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.json(), refusal("NOT_FOUND", "unknown_user"));
    let ci = world
        .send(bearer_post(&ci_token(), &format!("{USERS}/List"), "{}"))
        .await;
    assert_eq!(ci.status, StatusCode::FORBIDDEN);
    assert_eq!(ci.json(), refusal("PERMISSION_DENIED", "forbidden"));
    let alice = world.sign_in("alice", PASSWORD).await;
    let own = world
        .send(alice.post(&format!("{USERS}/ResetPassword"), r#"{"name":"alice"}"#))
        .await;
    assert_eq!(own.status, StatusCode::BAD_REQUEST);
    assert_eq!(own.json(), refusal("FAILED_PRECONDITION", "own_password"));
    assert_eq!(world.said().len(), 1, "only the login is on the audit");
}

/// Proves: a user who must still choose a password calls neither service, as no other operator
/// method: 403 `password_change_required`, and nothing is made.
#[tokio::test]
async fn a_session_that_must_change_its_password_makes_no_user_and_no_token() {
    let world = World::new("gateway-accounts-must-change");
    let newbie = world.sign_in("newbie", PASSWORD).await;
    for (path, body) in [
        (format!("{USERS}/Create"), r#"{"name":"carol"}"#),
        (format!("{TOKENS}/Create"), r#"{"name":"robot"}"#),
    ] {
        let sent = world.send(newbie.post(&path, body)).await;
        assert_eq!(sent.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            sent.json(),
            refusal("PERMISSION_DENIED", "password_change_required")
        );
    }
    assert_eq!(world.said().len(), 1, "only the login is on the audit");
}

/// Proves: a signed-in user disables and enables another through the gateway, the answers tell
/// the user as they are now, and a session of the disabled user ends at once.
#[tokio::test]
async fn a_session_disables_and_enables_another_user() {
    let world = World::new("gateway-accounts-disable");
    let alice = world.sign_in("alice", PASSWORD).await;
    let bob = world.sign_in("bob", PASSWORD).await;
    let off = world
        .send(alice.post(&format!("{USERS}/Disable"), r#"{"name":"bob"}"#))
        .await;
    assert_eq!(off.status, StatusCode::OK, "{off:?}");
    assert_eq!(off.json()["user"]["name"], json!("bob"));
    assert_eq!(
        off.json()["user"]["enabled"],
        Value::Null,
        "false is the default of a bool and the gateway leaves it out"
    );
    let gone = world.send(bob.get("/v1/session")).await;
    assert_eq!(gone.status, StatusCode::UNAUTHORIZED);
    let on = world
        .send(alice.post(&format!("{USERS}/Enable"), r#"{"name":"bob"}"#))
        .await;
    assert_eq!(on.status, StatusCode::OK, "{on:?}");
    assert_eq!(on.json()["user"]["enabled"], json!(true));
    let said: Vec<(String, String, Option<String>)> = world
        .said()
        .into_iter()
        .map(|(by, action, subject, _)| (by, action, subject))
        .filter(|(_, action, _)| action.starts_with("user_"))
        .collect();
    assert_eq!(
        said,
        [
            (
                "alice".to_owned(),
                "user_disable".to_owned(),
                Some("bob".to_owned())
            ),
            (
                "alice".to_owned(),
                "user_enable".to_owned(),
                Some("bob".to_owned())
            ),
        ]
    );
}
