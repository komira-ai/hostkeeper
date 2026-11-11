//! The ABM client over the scripted transport ([`crate::abm::client`], [`crate::abm::fake`]): the session,
//! the faults Apple names, the retry of a `429`, and each call.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyper::Method;
use serde_json::json;

use crate::abm::client::{AbmClient, AbmError, PAGE_LIMIT, Pacing};
use crate::abm::fake::{
    FakeAbm, Handler, NOW, Reply, Sim, body_of, client_over, clock_at, counting_random, dry_random,
    header, json_reply, pacing, recording_sleep, sample_token, text_reply,
};
use crate::abm::transport::{AbmRequest, AbmResponse, TransportError};
use crate::mdm_secrets::AbmToken;

/// A client over a simulated ABM with one device.
fn simulated() -> (Sim, Arc<FakeAbm>, AbmClient) {
    let sim = Sim::default();
    let fake = FakeAbm::new(sim.handler());
    let (client, _) = client_over(Arc::clone(&fake) as _);
    (sim, fake, client)
}

/// Proves: the first call opens a session with the signed header and every later call carries the
/// session it was given, so that two calls make one `GET /session`; every call names protocol
/// version 3, a page asks for 1000 devices and, the first time, no cursor; and the `Debug` of a
/// request shows the method and the path and no header.
///
/// Catches: a session opened for every call (288 more requests a day than the design counts), a
/// call that goes without the session header, and a request that prints its credentials.
#[tokio::test]
async fn the_session_is_opened_once_and_carried_by_every_call() {
    let (sim, fake, client) = simulated();
    sim.device("SERIAL1", "empty", None);
    let token = sample_token();
    let api = client.api(&token, pacing());
    assert_eq!(api.account().await.expect("account").server_name, "js-mdm");
    let page = api.fetch(None).await.expect("a page");
    assert_eq!(page.devices.len(), 1);
    assert_eq!(
        fake.calls(),
        ["GET /session", "GET /account", "POST /server/devices"]
    );
    let seen = fake.seen();
    let opening = header(&seen[0], "Authorization").expect("the signed header");
    assert!(opening.starts_with("OAuth realm=\"ADM\", oauth_consumer_key=\"CK_abc\""));
    assert!(opening.contains("oauth_nonce=\"000102030405060708090a0b0c0d0e0f\""));
    assert!(opening.contains(&format!("oauth_timestamp=\"{NOW}\"")));
    for request in &seen {
        assert_eq!(header(request, "X-Server-Protocol-Version"), Some("3"));
    }
    for request in &seen[1..] {
        assert_eq!(header(request, "X-ADM-Auth-Session"), Some("session-1"));
    }
    assert_eq!(body_of(&seen[2]), json!({ "limit": PAGE_LIMIT }));
    let answer = AbmResponse {
        status: 200,
        retry_after: Some(3),
        body: b"session-1 secret".to_vec(),
    };
    assert_eq!(
        format!("{answer:?}"),
        "AbmResponse { status: 200, retry_after: Some(3), .. }"
    );
    let shown = format!("{:?}", seen[1]);
    assert!(shown.contains("GET") && shown.contains("/account"));
    assert!(!shown.contains("session-1"), "{shown}");
}

/// Proves: a session is made for one token: a call with another token opens its own, and a call
/// with the first one again goes on with the session that is held for the newest token only.
///
/// Catches: a session made for one credential used for another after the operator uploads a
/// new token.
#[tokio::test]
async fn a_session_belongs_to_the_token_it_was_made_for() {
    let (sim, _fake, client) = simulated();
    let first = sample_token();
    let second = AbmToken {
        access_token: "other".to_owned(),
        ..sample_token()
    };
    client.api(&first, pacing()).account().await.expect("first");
    client
        .api(&second, pacing())
        .account()
        .await
        .expect("second");
    assert_eq!(sim.state().sessions, 2);
    client
        .api(&second, pacing())
        .account()
        .await
        .expect("second again");
    assert_eq!(sim.state().sessions, 2);
}

/// Proves: when ABM answers a call with 403 (not 401) because the session ran out, the client
/// opens a new session once and repeats the call, which then answers; and a 200 whose body is not
/// JSON is taken as the same fault.
///
/// Catches: a sync that stays down after the session's lifetime, and one that waits for a 401
/// Apple does not send.
#[tokio::test]
async fn a_403_or_a_body_that_is_not_json_refreshes_the_session_once() {
    let (sim, fake, client) = simulated();
    let token = sample_token();
    let api = client.api(&token, pacing());
    api.account().await.expect("account");
    sim.state().session = None;
    api.account()
        .await
        .expect("account after the session ran out");
    assert_eq!(sim.state().sessions, 2);
    assert_eq!(
        fake.calls(),
        [
            "GET /session",
            "GET /account",
            "GET /account",
            "GET /session",
            "GET /account"
        ]
    );

    let answers: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(vec!["<html>", "{}"]));
    let script = Arc::clone(&answers);
    let handler: Handler = Box::new(move |request: &AbmRequest| {
        if request.path == "/session" {
            return json_reply(200, &json!({ "auth_session_token": "s" }));
        }
        let next = script.lock().expect("script").remove(0);
        text_reply(200, next)
    });
    let (client, _) = client_over(FakeAbm::new(handler));
    let body = client
        .api(&token, pacing())
        .define_profile(&json!({}))
        .await;
    assert_eq!(body, Err(AbmError::BadAnswer("POST /profile")));
}

/// Proves: a session refused twice running is [`AbmError::SessionRefused`] and no third attempt
/// is made.
///
/// Catches: a loop that opens sessions for ever against a token Apple keeps refusing.
#[tokio::test]
async fn a_second_refusal_of_the_session_is_surfaced() {
    let handler: Handler = Box::new(|request: &AbmRequest| {
        if request.path == "/session" {
            json_reply(200, &json!({ "auth_session_token": "s" }))
        } else {
            text_reply(403, "Forbidden")
        }
    });
    let fake = FakeAbm::new(handler);
    let (client, _) = client_over(Arc::clone(&fake) as _);
    let token = sample_token();
    assert_eq!(
        client.api(&token, pacing()).account().await,
        Err(AbmError::SessionRefused)
    );
    assert_eq!(
        fake.calls(),
        [
            "GET /session",
            "GET /account",
            "GET /session",
            "GET /account"
        ]
    );
    // A 200 that is not JSON, twice, is the same.
    let handler: Handler = Box::new(|request: &AbmRequest| {
        if request.path == "/session" {
            json_reply(200, &json!({ "auth_session_token": "s" }))
        } else {
            text_reply(200, "not json")
        }
    });
    let (client, _) = client_over(FakeAbm::new(handler));
    assert_eq!(
        client.api(&token, pacing()).account().await,
        Err(AbmError::SessionRefused)
    );
}

/// Runs `GET /account` against a `/session` that answers `status` and `body`.
async fn account_after_session(
    status: u16,
    body: &str,
) -> Result<crate::abm::wire::Account, AbmError> {
    let body = body.to_owned();
    let handler: Handler = Box::new(move |_: &AbmRequest| text_reply(status, &body));
    let (client, _) = client_over(FakeAbm::new(handler));
    client.api(&sample_token(), pacing()).account().await
}

/// Proves: the faults Apple names on `/session` are told apart: `signature_invalid` (403, or 401)
/// is a fault of the credential, `timestamp_refused` a fault of the clock, any other 403 a refused
/// session, any other status is its status, and a 200 that has no `auth_session_token` is a
/// bad answer.
///
/// Catches: a wrong clock reported as a revoked token (and the operator sent to ABM to make a new
/// one), and a revoked token reported as a clock.
#[tokio::test]
async fn the_faults_of_the_session_are_told_apart() {
    assert_eq!(
        account_after_session(403, "signature_invalid").await,
        Err(AbmError::CredentialRefused)
    );
    assert_eq!(
        account_after_session(401, "signature_invalid").await,
        Err(AbmError::CredentialRefused)
    );
    assert_eq!(
        account_after_session(403, "timestamp_refused").await,
        Err(AbmError::ClockRefused)
    );
    assert_eq!(
        account_after_session(403, "oauth_problem=other").await,
        Err(AbmError::SessionRefused)
    );
    assert_eq!(
        account_after_session(502, "").await,
        Err(AbmError::Status(502))
    );
    assert_eq!(
        account_after_session(200, "{\"other\":1}").await,
        Err(AbmError::BadAnswer("GET /session"))
    );
    assert_eq!(
        account_after_session(200, "nope").await,
        Err(AbmError::BadAnswer("GET /session"))
    );
}

/// Proves: with no random bytes for the nonce no request is made and the error says so; and a
/// transport that gives no answer is its own error, carrying the transport's words.
///
/// Catches: a signature made with a nonce of zeros, and a lost connection reported as a refusal.
#[tokio::test]
async fn no_nonce_and_no_answer_are_their_own_errors() {
    let fake = FakeAbm::new(Sim::default().handler());
    let (sleep, _) = recording_sleep();
    let client = AbmClient::new(Arc::clone(&fake) as _, clock_at(NOW), dry_random(), sleep);
    assert_eq!(
        client.api(&sample_token(), pacing()).account().await,
        Err(AbmError::Random)
    );
    assert!(fake.seen().is_empty());

    let down: Handler =
        Box::new(|_: &AbmRequest| Err(TransportError("ABM: the connection failed".into())));
    let (client, _) = client_over(FakeAbm::new(down));
    let failed = client.api(&sample_token(), pacing()).account().await;
    assert_eq!(
        failed,
        Err(AbmError::Transport(TransportError(
            "ABM: the connection failed".to_owned()
        )))
    );
    assert_eq!(
        failed.expect_err("an error").to_string(),
        "ABM: the connection failed"
    );
}

/// An answer of `status` that names `Retry-After` as `retry_after`.
fn limited(retry_after: Option<u64>) -> Reply {
    Ok(AbmResponse {
        status: 429,
        retry_after,
        body: Vec::new(),
    })
}

/// Proves: a 429 that names `Retry-After` is repeated after that many seconds (the sleeper is
/// asked, nothing waits), a wait longer than the sync interval is cut to it, a 429 after the last
/// retry is [`AbmError::RateLimited`] with the seconds Apple named, one that names none is
/// that at once with no wait, and the same holds for the request that opens the session.
///
/// Catches: a client that hammers a server that asked for fewer requests, one that sleeps an
/// hour inside a five-minute pass, and one that retries for ever.
#[tokio::test]
async fn a_429_is_repeated_after_its_retry_after_and_no_more_than_the_retries() {
    let token = sample_token();
    // One 429 of 2 seconds on the call, then the answer.
    let asked: Arc<Mutex<u32>> = Arc::default();
    let count = Arc::clone(&asked);
    let handler: Handler = Box::new(move |request: &AbmRequest| {
        if request.path == "/session" {
            return json_reply(200, &json!({ "auth_session_token": "s" }));
        }
        let mut count = count.lock().expect("count");
        *count += 1;
        if *count == 1 {
            limited(Some(2))
        } else {
            json_reply(200, &json!({ "server_name": "js-mdm" }))
        }
    });
    let (client, waits) = client_over(FakeAbm::new(handler));
    client
        .api(&token, pacing())
        .account()
        .await
        .expect("after the wait");
    assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(2)]);

    // A wait of an hour is cut to the pacing's maximum, and the retries are used up.
    let always: Handler = Box::new(|request: &AbmRequest| {
        if request.path == "/session" {
            json_reply(200, &json!({ "auth_session_token": "s" }))
        } else {
            limited(Some(3600))
        }
    });
    let fake = FakeAbm::new(always);
    let (client, waits) = client_over(Arc::clone(&fake) as _);
    assert_eq!(
        client.api(&token, pacing()).account().await,
        Err(AbmError::RateLimited {
            retry_after: Some(3600)
        })
    );
    assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(300); 3]);
    assert_eq!(fake.calls().len(), 1 + 4);

    // No Retry-After: no wait, no retry.
    let none: Handler = Box::new(|_: &AbmRequest| limited(None));
    let fake = FakeAbm::new(none);
    let (client, waits) = client_over(Arc::clone(&fake) as _);
    assert_eq!(
        client.api(&token, pacing()).account().await,
        Err(AbmError::RateLimited { retry_after: None })
    );
    assert!(waits.lock().expect("waits").is_empty());
    assert_eq!(fake.calls(), ["GET /session"]);

    // A pacing with no retries gives up at the first 429 even with a Retry-After.
    let once: Handler = Box::new(|_: &AbmRequest| limited(Some(1)));
    let (client, waits) = client_over(FakeAbm::new(once));
    let no_retries = Pacing {
        retries: 0,
        ..pacing()
    };
    assert_eq!(
        client.api(&token, no_retries).account().await,
        Err(AbmError::RateLimited {
            retry_after: Some(1)
        })
    );
    assert!(waits.lock().expect("waits").is_empty());
}

/// Proves: the pages: a full fetch asks `POST /server/devices` with the limit and the cursor of
/// the page before, a sync asks `POST /devices/sync` with its cursor, the records keep the fields
/// the sync reads (serial, model, description, profile status and uuid, `op_type`) and ignore
/// the rest, a 400 `INVALID_CURSOR` is [`AbmError::InvalidCursor`] and a 400 with another body is
/// a status.
///
/// Catches: a cursor that is not sent, a record that fails for a field of a later release, and an
/// invalid cursor that is reported as a failure of the sync and not as the signal for a full fetch.
#[tokio::test]
async fn pages_carry_their_cursors_and_an_invalid_cursor_is_its_own_error() {
    let (sim, fake, client) = simulated();
    sim.device("S1", "empty", None);
    sim.device("S2", "pushed", Some("UUID-1"));
    sim.state().page = 1;
    let token = sample_token();
    let api = client.api(&token, pacing());
    let first = api.fetch(None).await.expect("first page");
    assert!(first.more_to_follow);
    assert_eq!(first.cursor, "full-1");
    let second = api.fetch(Some(&first.cursor)).await.expect("second page");
    assert!(!second.more_to_follow);
    assert_eq!(second.devices[0].serial_number, "S2");
    assert_eq!(second.devices[0].profile_status.as_deref(), Some("pushed"));
    assert_eq!(second.devices[0].profile_uuid.as_deref(), Some("UUID-1"));
    assert_eq!(second.devices[0].model.as_deref(), Some("Mac Studio"));
    assert_eq!(second.devices[0].op_type, None);
    let bodies: Vec<_> = fake.seen().iter().skip(1).map(body_of).collect();
    assert_eq!(
        bodies,
        [
            json!({ "limit": 1000 }),
            json!({ "limit": 1000, "cursor": "full-1" })
        ]
    );

    sim.state()
        .log
        .push(("S1".to_owned(), "modified".to_owned()));
    let changes = api.changes("c0").await.expect("changes");
    assert_eq!(changes.devices[0].op_type.as_deref(), Some("modified"));
    assert_eq!(changes.cursor, "c1");
    assert_eq!(
        api.changes("bogus").await.map(drop),
        Err(AbmError::InvalidCursor)
    );
    assert_eq!(
        api.changes("c9").await.map(drop),
        Err(AbmError::InvalidCursor)
    );

    let other: Handler = Box::new(|request: &AbmRequest| {
        if request.path == "/session" {
            json_reply(200, &json!({ "auth_session_token": "s" }))
        } else if request.path == "/devices/sync" {
            text_reply(400, "BAD_REQUEST")
        } else {
            text_reply(200, "{\"cursor\": 5}")
        }
    });
    let (client, _) = client_over(FakeAbm::new(other));
    assert_eq!(
        client.api(&token, pacing()).changes("c0").await.map(drop),
        Err(AbmError::Status(400))
    );
    assert_eq!(
        client.api(&token, pacing()).fetch(None).await.map(drop),
        Err(AbmError::BadAnswer("a device page"))
    );
}

/// Proves: a profile is defined with `POST /profile`, which answers its uuid, and an answer that
/// has no uuid is a bad answer.
///
/// Catches: a sync that stores an empty uuid and then assigns nothing for ever.
#[tokio::test]
async fn a_profile_is_defined_and_its_uuid_returned() {
    let (sim, fake, client) = simulated();
    let token = sample_token();
    let uuid = client
        .api(&token, pacing())
        .define_profile(&json!({ "profile_name": "P" }))
        .await
        .expect("defined");
    assert_eq!(uuid, format!("PROFILE{:028}", 1));
    assert_eq!(sim.state().profiles[&uuid], json!({ "profile_name": "P" }));
    assert_eq!(body_of(&fake.seen()[1]), json!({ "profile_name": "P" }));

    let bad: Handler = Box::new(|request: &AbmRequest| {
        if request.path == "/session" {
            json_reply(200, &json!({ "auth_session_token": "s" }))
        } else {
            json_reply(200, &json!({ "devices": {} }))
        }
    });
    let (client, _) = client_over(FakeAbm::new(bad));
    assert_eq!(
        client
            .api(&token, pacing())
            .define_profile(&json!({}))
            .await,
        Err(AbmError::BadAnswer("POST /profile"))
    );
}

/// Proves: an assignment is `POST /profile/devices` with the profile and the serials, and the
/// outcome of each serial comes back; when ABM answers the POST with a 4xx it is made again as
/// `PUT`; when both are refused the status is the PUT's; a 5xx is not retried as `PUT`; and an
/// answer that is not JSON is a bad answer.
///
/// Catches: an assignment that never works against a server that only takes `PUT` (the
/// simulator NanoDEP's source names), and one that repeats a write after a server error.
#[tokio::test]
async fn an_assignment_falls_back_to_put_when_post_is_refused() {
    let (sim, fake, client) = simulated();
    sim.device("S1", "empty", None);
    sim.device("S2", "empty", None);
    sim.state().inaccessible = vec!["S2".to_owned()];
    let token = sample_token();
    let serials = vec!["S1".to_owned(), "S2".to_owned()];
    let answer = client
        .api(&token, pacing())
        .assign("UUID", &serials)
        .await
        .expect("assigned");
    assert_eq!(answer.devices["S1"], "SUCCESS");
    assert_eq!(answer.devices["S2"], "NOT_ACCESSIBLE");
    assert_eq!(
        body_of(&fake.seen()[1]),
        json!({ "profile_uuid": "UUID", "devices": ["S1", "S2"] })
    );
    assert_eq!(
        fake.calls().last().map(String::as_str),
        Some("POST /profile/devices")
    );

    sim.state().refuse_assign = vec![Method::POST];
    client
        .api(&token, pacing())
        .assign("UUID", &serials)
        .await
        .expect("assigned by PUT");
    let calls = fake.calls();
    assert_eq!(
        calls[calls.len() - 2..],
        ["POST /profile/devices", "PUT /profile/devices"]
    );

    sim.state().refuse_assign = vec![Method::POST, Method::PUT];
    assert_eq!(
        client
            .api(&token, pacing())
            .assign("UUID", &serials)
            .await
            .map(drop),
        Err(AbmError::Status(404))
    );

    let server_error: Handler = Box::new(|request: &AbmRequest| match request.path.as_str() {
        "/session" => json_reply(200, &json!({ "auth_session_token": "s" })),
        _ if request.method == Method::POST => text_reply(500, "boom"),
        _ => json_reply(200, &json!({})),
    });
    let fake = FakeAbm::new(server_error);
    let (client, _) = client_over(Arc::clone(&fake) as _);
    assert_eq!(
        client
            .api(&token, pacing())
            .assign("UUID", &serials)
            .await
            .map(drop),
        Err(AbmError::Status(500))
    );
    assert_eq!(fake.calls(), ["GET /session", "POST /profile/devices"]);

    let not_json: Handler = Box::new(|request: &AbmRequest| match request.path.as_str() {
        "/session" => json_reply(200, &json!({ "auth_session_token": "s" })),
        _ => json_reply(200, &json!([1, 2])),
    });
    let (client, _) = client_over(FakeAbm::new(not_json));
    assert_eq!(
        client
            .api(&token, pacing())
            .assign("UUID", &serials)
            .await
            .map(drop),
        Err(AbmError::BadAnswer("an assignment"))
    );
    let _ = (counting_random(),);
}
