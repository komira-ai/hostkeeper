//! `serve` with the sign-in (`docs/design/operator-login.md`, section 5): the flags that set it,
//! what they take, and the first admin's path through a coordinator started from them: the file,
//! the first login, the change of the password, and the file's deletion. Time here is the
//! system's, so nothing below waits for a deadline.

mod support;

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::accounts::User;
use hostkeeper_coordinator::config::{Cli, StartError};
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_coordinator::password::{PasswordSettings, Passwords};
use hostkeeper_coordinator::service::Coordinator;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response};
use serde_json::{Value, json};
use support::serve::{Files, build_given, flags_of, refusal, with, without};

const PASSWORD: &str = "k7Pq9xWm3Rt5Vb2NcH8dZs4F";
const NEW_PASSWORD: &str = "a much newer password";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs()
}

/// The arguments with the cheapest password hash the algorithm takes, and `more`.
fn cheap(files: &Files, more: &[(&str, &str)]) -> Vec<OsString> {
    let mut args = files.args();
    for (flag, value) in [
        ("--password-memory-kib", "8"),
        ("--password-iterations", "1"),
    ]
    .into_iter()
    .chain(more.iter().copied())
    {
        args.extend([flag.into(), value.into()]);
    }
    args
}

/// A user `name` whose password is [`PASSWORD`] under the cheap parameters.
fn add_user(coordinator: &Coordinator, name: &str) {
    let settings = PasswordSettings {
        memory_kib: 8,
        iterations: 1,
        ..PasswordSettings::default()
    };
    let hash = Passwords::new(settings)
        .expect("passwords")
        .hash(PASSWORD)
        .expect("a hash");
    let user = User {
        name: name.to_owned(),
        hash: hash.as_str().to_owned(),
        enabled: true,
        created_by: "coordinator".to_owned(),
        created_at: 1,
        ..User::default()
    };
    coordinator
        .accounts()
        .change(|held| held.with_user(user))
        .expect("a user");
}

/// `method path` with `body`, as a page of the listener's own origin sends it, with the cookie and
/// CSRF header of a session when `session` is given.
fn page(
    method: &str,
    path: &str,
    session: Option<(&str, &str)>,
    body: Value,
) -> Request<Full<Bytes>> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "hk.test")
        .header("origin", "https://hk.test")
        .header("content-type", "application/json");
    if let Some((id, csrf)) = session {
        builder = builder
            .header("cookie", format!("__Host-hk_session={id}"))
            .header("x-hk-csrf", csrf);
    }
    builder
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("a request")
}

async fn send(
    coordinator: &Arc<Coordinator>,
    request: Request<Full<Bytes>>,
) -> (u16, Response<()>, Value) {
    let response = route(
        coordinator,
        Surface::Control,
        &None,
        request,
        Duration::from_secs(30),
    )
    .await;
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.expect("a body").to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (parts.status.as_u16(), Response::from_parts(parts, ()), json)
}

/// The session a login answered: the cookie's value and the CSRF token.
fn session_of(answer: &(u16, Response<()>, Value)) -> (String, String) {
    let cookie = answer.1.headers()["set-cookie"].to_str().expect("text");
    let id = cookie
        .strip_prefix("__Host-hk_session=")
        .and_then(|rest| rest.split(';').next())
        .expect("the cookie's value");
    (
        id.to_owned(),
        answer.2["csrf"].as_str().expect("a token").to_owned(),
    )
}

async fn login(
    coordinator: &Arc<Coordinator>,
    name: &str,
    password: &str,
) -> (u16, Response<()>, Value) {
    let body = json!({ "username": name, "password": password });
    send(coordinator, page("POST", "/v1/login", None, body)).await
}

/// Proves: a coordinator started from the flags serves the sign-in, and the flags' numbers are the
/// ones it runs by: `--login-max-failures 2` locks a name at its second failure (429 for the
/// right password after it), `--session-max-count 1` ends the first session when a second signs
/// in, and a user who never failed signs in as before.
///
/// Catches: a flag that is read and not used, or a default that is not the design's.
#[tokio::test]
async fn the_flags_set_the_limits_the_sign_in_runs_by() {
    let files = Files::new("login-flags");
    let args = cheap(
        &files,
        &[
            ("--login-max-failures", "2"),
            ("--session-max-count", "1"),
            ("--login-max-tracked", "8"),
            ("--login-max-hashes", "1"),
            ("--session-idle-secs", "600"),
            ("--session-max-secs", "6000"),
            ("--login-lockout-secs", "60"),
        ],
    );
    let parts = build_given(&flags_of(args)).expect("starts");
    let coordinator = &parts.coordinator;
    assert!(coordinator.login().is_some());
    for name in ["alice", "bob", "carol"] {
        add_user(coordinator, name);
    }
    let first = login(coordinator, "alice", PASSWORD).await;
    assert_eq!(first.0, 200);
    let (id, csrf) = session_of(&first);
    let second = login(coordinator, "bob", PASSWORD).await;
    assert_eq!(second.0, 200);
    let gone = send(
        coordinator,
        page("GET", "/v1/session", Some((&id, &csrf)), json!({})),
    )
    .await;
    assert_eq!(gone.0, 401, "the cap of one session ended the first");

    assert_eq!(login(coordinator, "carol", "not the password").await.0, 401);
    assert_eq!(login(coordinator, "carol", "not the password").await.0, 401);
    assert_eq!(login(coordinator, "carol", PASSWORD).await.0, 429);
}

/// Proves: each number of the sign-in is a flag that takes 1 or more and nothing less, and hash
/// parameters the algorithm refuses stop the start with its own error even with no initial-admin
/// file.
///
/// Catches: a flag that takes 0 (a lock that never locks, a session that never lives), or a
/// start that finds out about bad hash parameters at the first login.
#[test]
fn the_flags_take_one_or_more_and_hash_parameters_the_algorithm_runs() {
    let files = Files::new("login-flags-range");
    for flag in [
        "--session-idle-secs",
        "--session-max-secs",
        "--session-max-count",
        "--login-max-failures",
        "--login-lockout-secs",
        "--login-max-hashes",
        "--login-max-tracked",
    ] {
        let mut args = files.args();
        args.extend([flag.into(), "0".into()]);
        let refused = Cli::try_parse_from(args).expect_err(flag);
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{flag}");
        let mut args = files.args();
        args.extend([flag.into(), "1".into()]);
        Cli::try_parse_from(args).expect(flag);
    }
    let weak = with(&cheap(&files, &[]), "--password-memory-kib", "1");
    let why = refusal(weak);
    assert!(matches!(why, StartError::Password(_)), "{why}");
}

/// Proves: the first admin's path, from the file to the first password: the coordinator makes the
/// user `admin` from the file, who signs in with its password, is told to change it and can call
/// nothing else, changes it, and the file is deleted by the change; the old password no longer
/// signs in, the new one does, and the session's `must_change` is gone.
///
/// Catches: a password that stays good after its change, a file that outlives the change, or a
/// session that is still shut out of the API after it chose its password.
#[tokio::test]
async fn the_first_admin_signs_in_changes_the_password_and_the_file_goes() {
    let files = Files::new("login-first-admin");
    let file = format!(
        "id = \"id-1\"\nuser = \"admin\"\npassword = \"{PASSWORD}\"\nvalid_until = {}\n",
        now() + 600
    );
    files.put("initial-admin.toml", &file, 0o400);
    let mut args = without(&cheap(&files, &[]), "--operator-token-file");
    args.extend([
        "--initial-admin-file".into(),
        files.path("initial-admin.toml").into(),
    ]);
    let parts = build_given(&flags_of(args)).expect("starts");
    let coordinator = &parts.coordinator;

    let answered = login(coordinator, "admin", PASSWORD).await;
    assert_eq!(answered.0, 200);
    assert_eq!(answered.2["must_change"], json!(true));
    let (id, csrf) = session_of(&answered);
    let call = page(
        "POST",
        "/hostkeeper.operator.v1.Hosts/List",
        Some((&id, &csrf)),
        json!({}),
    );
    let refused = send(coordinator, call).await;
    assert_eq!(refused.0, 403);
    assert_eq!(
        refused.2,
        json!({ "code": "PERMISSION_DENIED", "reason": "password_change_required" })
    );

    let change = page(
        "POST",
        "/v1/password",
        Some((&id, &csrf)),
        json!({ "current": PASSWORD, "new": NEW_PASSWORD }),
    );
    let changed = send(coordinator, change).await;
    assert_eq!(changed.0, 200);
    assert_eq!(changed.2["must_change"], json!(false));
    assert!(!files.path("initial-admin.toml").exists());
    let call = page(
        "POST",
        "/hostkeeper.operator.v1.Hosts/List",
        Some((&id, &csrf)),
        json!({}),
    );
    assert_eq!(
        send(coordinator, call).await.0,
        200,
        "the call is served now"
    );
    assert_eq!(login(coordinator, "admin", PASSWORD).await.0, 401);
    assert_eq!(login(coordinator, "admin", NEW_PASSWORD).await.0, 200);
}

/// Proves: a coordinator started from the flags lets a signed-in user make a user through the
/// operator API with a temporary password good for `--initial-password-secs` and no longer, and
/// the new user signs in with it and is told to change it; a token made through the API calls the
/// API under its name.
///
/// Catches: a flag that is read for the first admin's file and not for the passwords an operator
/// makes, and a stored token that the bearer lookup of a started coordinator does not see.
#[tokio::test]
async fn a_started_coordinator_makes_users_and_tokens_for_the_operator() {
    let files = Files::new("login-accounts");
    let args = cheap(&files, &[("--initial-password-secs", "600")]);
    let parts = build_given(&flags_of(args)).expect("starts");
    let coordinator = &parts.coordinator;
    add_user(coordinator, "alice");
    let (id, csrf) = session_of(&login(coordinator, "alice", PASSWORD).await);
    let made = send(
        coordinator,
        page(
            "POST",
            "/hostkeeper.operator.v1.Users/Create",
            Some((&id, &csrf)),
            json!({ "name": "carol" }),
        ),
    )
    .await;
    assert_eq!(made.0, 200, "{:?}", made.2);
    let seconds = |field: &str| -> u64 {
        made.2["user"][field]
            .as_str()
            .and_then(|text| text.parse().ok())
            .unwrap_or_else(|| panic!("{field}: {:?}", made.2))
    };
    assert_eq!(
        seconds("passwordValidUntilUnix") - seconds("createdUnix"),
        600
    );
    let password = made.2["password"].as_str().expect("a password").to_owned();
    let signed = login(coordinator, "carol", &password).await;
    assert_eq!(signed.0, 200);
    assert_eq!(signed.2["must_change"], json!(true));

    let token = send(
        coordinator,
        page(
            "POST",
            "/hostkeeper.operator.v1.ApiTokens/Create",
            Some((&id, &csrf)),
            json!({ "name": "robot" }),
        ),
    )
    .await;
    assert_eq!(token.0, 200, "{:?}", token.2);
    let secret = token.2["secret"].as_str().expect("a secret");
    let mut listed = page(
        "POST",
        "/hostkeeper.operator.v1.Users/List",
        None,
        json!({}),
    );
    listed.headers_mut().insert(
        "authorization",
        format!("Bearer {secret}").parse().expect("a value"),
    );
    let answer = send(coordinator, listed).await;
    assert_eq!(answer.0, 200, "{:?}", answer.2);
    assert_eq!(answer.2["users"].as_array().map(Vec::len), Some(2));
}
