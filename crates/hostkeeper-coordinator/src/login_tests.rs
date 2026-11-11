//! The rules by which a request is a session's, and the sign-in's own state: the cookie among
//! other cookies, where a bearer's request never reads one, the origin and the content type, and
//! the checks a password change makes off the runtime's threads.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hyper::{HeaderMap, Uri};

use crate::login::{
    COOKIE_NAME, Change, Login, LoginSettings, authority, held, same_origin, says_json,
    session_request_ok,
};
use crate::login_support::{
    HOST, NEW_PASSWORD, PASSWORD, START, Seconds, cheap, counting, settings, user,
};
use crate::password::{PasswordError, PasswordSettings, Passwords, PolicyError, Verdict};

/// The headers of a request as name and value pairs.
type Pairs<'a> = &'a [(&'a str, &'a str)];

fn headers(pairs: Pairs<'_>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.append(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).expect("a name"),
            value.parse().expect("a value"),
        );
    }
    headers
}

fn login() -> Login {
    let clock = Arc::new(AtomicU64::new(START));
    Login::new(settings(), Arc::new(Seconds(clock)), counting()).expect("a sign-in")
}

/// Proves: the session cookie is found among other cookies and among several `Cookie` headers,
/// the first of its name wins, and a cookie of another name, with the name as a suffix or a
/// prefix, is none.
#[test]
fn the_session_cookie_is_found_by_its_whole_name() {
    let login = login();
    let (id, _) = login
        .sessions
        .create("alice", false, None)
        .expect("a session");
    let cookie = |value: String| headers(&[("cookie", &value)]);
    let id = id.as_str();
    for found in [
        cookie(format!("{COOKIE_NAME}={id}")),
        cookie(format!("theme=dark; {COOKIE_NAME}={id}; other=1")),
        headers(&[
            ("cookie", "a=b"),
            ("cookie", &format!("{COOKIE_NAME}={id}")),
        ]),
        cookie(format!("{COOKIE_NAME}={id}; {COOKIE_NAME}=second")),
    ] {
        assert_eq!(
            held(&login, &found).map(|held| held.id),
            Some(id.to_owned())
        );
    }
    for missing in [
        headers(&[]),
        cookie(format!("hk_session={id}")),
        cookie(format!("x{COOKIE_NAME}={id}")),
        cookie(format!("{COOKIE_NAME}x={id}")),
        cookie(format!("{COOKIE_NAME}=")),
        cookie(COOKIE_NAME.to_owned()),
        cookie(format!("{COOKIE_NAME}={}", "a".repeat(129))),
        cookie(format!("{COOKIE_NAME}=second; {COOKIE_NAME}={id}")),
    ] {
        assert!(held(&login, &missing).is_none(), "{missing:?}");
    }
}

/// Proves: a request with an `Authorization` header, of any scheme, never has its cookie read, even
/// when the cookie names a live session.
#[test]
fn a_cookie_beside_an_authorization_header_is_not_read() {
    let login = login();
    let (id, _) = login
        .sessions
        .create("alice", false, None)
        .expect("a session");
    let cookie = format!("{COOKIE_NAME}={}", id.as_str());
    assert!(held(&login, &headers(&[("cookie", &cookie)])).is_some());
    for scheme in ["Bearer token", "Basic YTpi", "nonsense"] {
        let both = headers(&[("cookie", &cookie), ("authorization", scheme)]);
        assert!(held(&login, &both).is_none(), "{scheme}");
    }
}

/// Proves: the authority asked is the `Host` header, or, with none (an HTTP/2 request), the
/// authority of the target.
#[test]
fn the_authority_is_the_host_header_or_the_targets() {
    let uri: Uri = "https://target.test/v1/login".parse().expect("a uri");
    let path: Uri = "/v1/login".parse().expect("a uri");
    assert_eq!(authority(&headers(&[("host", HOST)]), &uri), Some(HOST));
    assert_eq!(authority(&headers(&[]), &uri), Some("target.test"));
    assert_eq!(authority(&headers(&[]), &path), None);
}

/// Proves: an `Origin` is the listener's own only when it is `https://` and the authority asked
/// (in any case); no `Origin` passes; with no authority an `Origin` does not; `Sec-Fetch-Site`
/// passes only as `same-origin` or not at all.
#[test]
fn an_origin_is_the_listeners_own_or_absent() {
    let own = format!("https://{HOST}");
    let cases: [(Pairs, Option<&str>, bool); 12] = [
        (&[], Some(HOST), true),
        (&[("origin", &own)], Some(HOST), true),
        (&[("origin", "HTTPS://HK.TEST")], Some(HOST), true),
        (&[("origin", "http://hk.test")], Some(HOST), false),
        (&[("origin", "https://hk.test:8443")], Some(HOST), false),
        (&[("origin", "https://evil.test")], Some(HOST), false),
        (&[("origin", "null")], Some(HOST), false),
        (&[("origin", &own)], None, false),
        (&[("sec-fetch-site", "same-origin")], Some(HOST), true),
        (&[("sec-fetch-site", "same-site")], Some(HOST), false),
        (&[("sec-fetch-site", "cross-site")], Some(HOST), false),
        (&[("sec-fetch-site", "none")], Some(HOST), false),
    ];
    for (pairs, authority, want) in cases {
        assert_eq!(same_origin(&headers(pairs), authority), want, "{pairs:?}");
    }
    let mut not_text = HeaderMap::new();
    not_text.insert(
        "origin",
        hyper::header::HeaderValue::from_bytes(&[0xff]).expect("a value"),
    );
    assert!(!same_origin(&not_text, Some(HOST)));
}

/// Proves: the body says it is JSON only as the media type `application/json`, in any case, with
/// any parameters; nothing else, and no header at all, does.
#[test]
fn only_the_json_media_type_says_json() {
    for json in [
        "application/json",
        "Application/JSON",
        "application/json; charset=utf-8",
        " application/json ;charset=utf-8",
    ] {
        assert!(says_json(&headers(&[("content-type", json)])), "{json}");
    }
    for other in [
        "text/plain",
        "application/x-www-form-urlencoded",
        "application/jsonx",
        "multipart/form-data; boundary=json",
        "",
    ] {
        assert!(!says_json(&headers(&[("content-type", other)])), "{other}");
    }
    assert!(!says_json(&headers(&[])));
}

/// Proves: a session's `POST` passes only with the session's CSRF token, compared whole, and the
/// origin and the content type right: a token that is a prefix or an extension of it, none, and
/// each wrong other check fail.
#[test]
fn a_session_post_needs_the_token_the_origin_and_json() {
    let login = login();
    let (_, view) = login
        .sessions
        .create("alice", false, None)
        .expect("a session");
    let token = view.csrf().to_owned();
    let page = |csrf: &str, origin: &str, kind: &str| {
        headers(&[
            ("x-hk-csrf", csrf),
            ("origin", origin),
            ("content-type", kind),
        ])
    };
    let own = format!("https://{HOST}");
    assert!(session_request_ok(
        &view,
        &page(&token, &own, "application/json"),
        Some(HOST)
    ));
    for refused in [
        page(&token[1..], &own, "application/json"),
        page(&format!("{token}x"), &own, "application/json"),
        page("", &own, "application/json"),
        page(&token, "https://evil.test", "application/json"),
        page(&token, &own, "text/plain"),
        headers(&[("origin", &own), ("content-type", "application/json")]),
    ] {
        assert!(
            !session_request_ok(&view, &refused, Some(HOST)),
            "{refused:?}"
        );
    }
}

/// Proves: the settings reach the parts: a sign-in over hash parameters Argon2id refuses is not
/// made, and a cap of no hash at once is taken as one.
#[test]
fn a_sign_in_takes_settings_argon2_allows_and_at_least_one_hash_slot() {
    let clock = Arc::new(Seconds(Arc::new(AtomicU64::new(START))));
    let refused = Login::new(
        LoginSettings {
            passwords: PasswordSettings {
                memory_kib: 1,
                ..cheap()
            },
            ..settings()
        },
        clock.clone(),
        counting(),
    )
    .expect_err("refused");
    assert!(matches!(refused, PasswordError::Params(_)), "{refused}");
    let none = Login::new(
        LoginSettings {
            max_hashes: 0,
            ..settings()
        },
        clock,
        counting(),
    )
    .expect("a sign-in");
    let first = none.limiter.admit("alice").expect("one slot");
    assert!(none.limiter.admit("bob").is_err(), "and only one");
    drop(first);
    assert!(none.limiter.admit("bob").is_ok());
}

/// Proves: the checks of a password change run in order: a wrong current password is `Wrong`
/// before the policy is looked at; a right one with a new password under the policy is that
/// policy's refusal; a right one with a good new password is its hash, which verifies it.
#[tokio::test]
async fn the_checks_of_a_change_run_in_order_and_hash_last() {
    let login = login();
    let stored = user("alice").hash;
    let ask = |current: &str, new: &str| {
        login.change(
            "alice".to_owned(),
            current.to_owned(),
            new.to_owned(),
            stored.clone(),
        )
    };
    assert_eq!(ask("not the password", "short").await, Change::Wrong);
    assert_eq!(
        ask(PASSWORD, "short").await,
        Change::Policy(PolicyError::TooShort { min: 12 })
    );
    let Change::Hash(hash) = ask(PASSWORD, NEW_PASSWORD).await else {
        panic!("a hash");
    };
    assert_eq!(
        Passwords::new(cheap())
            .expect("passwords")
            .verify(NEW_PASSWORD, &hash.into()),
        Verdict::Right { rehash: None }
    );
}

/// Proves: a verify of no stored hash spends one verify and says no; of a stored one it says what
/// the hash says.
#[tokio::test]
async fn a_verify_of_nobody_spends_one_verify_and_says_no() {
    let login = login();
    let before = login.passwords.verifies();
    assert_eq!(
        login.verify(PASSWORD.to_owned(), None).await,
        Verdict::Wrong
    );
    assert_eq!(login.passwords.verifies() - before, 1);
    assert_eq!(
        login
            .verify(PASSWORD.to_owned(), Some(user("alice").hash))
            .await,
        Verdict::Right { rehash: None }
    );
}

/// Proves: what a sign-in prints says nothing of its sessions, its counts or its hashes.
#[test]
fn a_sign_in_prints_no_secret() {
    let login = login();
    let (id, view) = login
        .sessions
        .create("alice", false, None)
        .expect("a session");
    let text = format!("{login:?}");
    assert_eq!(text, "Login { .. }");
    assert!(!text.contains(id.as_str()) && !text.contains(view.csrf()));
}
