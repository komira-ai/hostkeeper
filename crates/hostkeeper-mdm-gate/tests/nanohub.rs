//! The client of NanoHUB's API: the URL and the key file it is configured with, the request it
//! sends, and each way a call ends.

mod support;

use std::os::unix::fs::symlink;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_mdm_gate::{
    ApiKey, ApiKeyError, ApiUrl, Command, CommandUuid, EnqueueError, KEY_FLAG, NanoHub, Queued,
    URL_FLAG,
};
use hostkeeper_wire::secret::SecretError;
use plist::Dictionary;
use serde_json::json;
use support::{Answer, FakeNanoHub, KEY, UDID, id, scratch, write_mode};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;

fn url_refusal(why: &str) -> String {
    format!(
        "{URL_FLAG}: {why}; it is http://<loopback address>:<port>, as http://127.0.0.1:9004, since NanoHUB's API is reached on loopback only"
    )
}

/// Catches: an API URL that is not plain HTTP to a loopback address and a port accepted (the
/// key would travel off the machine), or a refusal that names the wrong rule.
#[test]
fn an_api_url_is_http_to_a_loopback_address_and_a_port() {
    for (text, shown) in [
        (
            "http://127.0.0.1:9004",
            "ApiUrl { ip: 127.0.0.1, port: 9004 }",
        ),
        (
            "http://127.0.0.1:9004/",
            "ApiUrl { ip: 127.0.0.1, port: 9004 }",
        ),
        ("http://[::1]:9004", "ApiUrl { ip: ::1, port: 9004 }"),
        ("http://127.5.6.7:1", "ApiUrl { ip: 127.5.6.7, port: 1 }"),
        (
            "http://127.0.0.1:65535",
            "ApiUrl { ip: 127.0.0.1, port: 65535 }",
        ),
    ] {
        let parsed = ApiUrl::parse(text).unwrap_or_else(|why| panic!("{text}: {why}"));
        assert_eq!(format!("{parsed:?}"), shown, "{text}");
    }
    let refused = [
        ("https://127.0.0.1:9004", "it must start http://"),
        ("127.0.0.1:9004", "it must start http://"),
        ("", "it must start http://"),
        ("http://localhost:9004", "its host is not an IP address"),
        ("http://:9004", "its host is not an IP address"),
        ("http://[]:9004", "its host is not an IP address"),
        (
            "http://192.0.2.1:9004",
            "its address is not a loopback address",
        ),
        (
            "http://0.0.0.0:9004",
            "its address is not a loopback address",
        ),
        ("http://[::2]:1", "its address is not a loopback address"),
        ("http://127.0.0.1", "it names no port"),
        ("http://127.0.0.1:0", "its port is not 1 to 65535"),
        ("http://127.0.0.1:99999", "its port is not 1 to 65535"),
        ("http://127.0.0.1:", "its port is not 1 to 65535"),
        ("http://127.0.0.1:-1", "its port is not 1 to 65535"),
        (
            "http://127.0.0.1:9004/api",
            "it has a path, a query or user info",
        ),
        (
            "http://127.0.0.1:9004//",
            "it has a path, a query or user info",
        ),
        (
            "http://127.0.0.1:9004?x=1",
            "it has a path, a query or user info",
        ),
        (
            "http://user@127.0.0.1:9004",
            "it has a path, a query or user info",
        ),
        (
            "http://127.0.0.1:9004#top",
            "it has a path, a query or user info",
        ),
    ];
    for (text, why) in refused {
        let error = ApiUrl::parse(text).expect_err(text);
        assert_eq!(error.to_string(), url_refusal(why), "{text}");
    }
}

fn key_file(
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> (hostkeeper_testkit::Scratch, std::path::PathBuf) {
    let dir = scratch(name);
    let path = dir.path().join("api.key");
    write_mode(&path, bytes, mode);
    (dir, path)
}

fn key_refusal() -> String {
    format!("{KEY_FLAG}: it must hold one key of 1 to 4096 visible ASCII characters")
}

/// Catches: a key file that breaks the secret file rule (a mode others can read, a link, a
/// directory, a missing file) read; the path in the refusal.
#[test]
fn a_key_file_must_be_a_private_regular_file() {
    let (dir, path) = key_file("key-mode", KEY.as_bytes(), 0o644);
    let error = ApiKey::read(&path).expect_err("mode 0644");
    assert_eq!(
        error.to_string(),
        format!("{KEY_FLAG}: its mode must be 0600 or 0400")
    );
    assert!(!error.to_string().contains(&path.display().to_string()));
    for mode in [0o600, 0o400] {
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(mode))
            .expect("chmod");
        assert!(ApiKey::read(&path).is_ok(), "{mode:o}");
    }

    let link = dir.path().join("link.key");
    symlink(&path, &link).expect("a symlink");
    let error = ApiKey::read(&link).expect_err("a link");
    assert!(
        matches!(&error, ApiKeyError::File(SecretError::Io { flag, .. }) if flag == KEY_FLAG),
        "{error}"
    );
    assert!(error.to_string().starts_with(&format!("{KEY_FLAG}: ")));
    assert!(!error.to_string().contains(&link.display().to_string()));

    let error = ApiKey::read(dir.path()).expect_err("a directory");
    assert!(
        matches!(error, ApiKeyError::File(SecretError::Rule { .. })),
        "{error}"
    );
    let error = ApiKey::read(&dir.path().join("missing.key")).expect_err("missing");
    assert_eq!(error.to_string(), format!("{KEY_FLAG}: entity not found"));
}

/// Catches: an empty key, a blank one, a key with a space inside, a control character or a
/// non-ASCII character, or one over 4096 bytes, accepted; whitespace around a key kept.
#[test]
fn a_key_is_one_run_of_visible_ascii_of_at_most_4096_bytes() {
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("blank", b" \t\r\n".to_vec()),
        ("non-ascii", "kéy".as_bytes().to_vec()),
        ("not utf-8", vec![b'a', 0xff, b'b']),
        ("control", b"ab\x01cd".to_vec()),
        ("inner space", b"ab cd".to_vec()),
        ("inner newline", b"ab\ncd".to_vec()),
        ("4097 bytes", vec![b'a'; 4097]),
        (
            "4096 key and a newline",
            [vec![b'a'; 4096], b"\n".to_vec()].concat(),
        ),
    ];
    for (name, bytes) in bad {
        let (_dir, path) = key_file("key-bad", &bytes, 0o600);
        let error = ApiKey::read(&path).expect_err(name);
        assert_eq!(error, ApiKeyError::Key, "{name}");
        assert_eq!(error.to_string(), key_refusal(), "{name}");
    }
    let (_dir, path) = key_file("key-4096", &vec![b'a'; 4096], 0o600);
    assert!(ApiKey::read(&path).is_ok());
    let (_dir, path) = key_file(
        "key-4095",
        &[vec![b'a'; 4095], b"\n".to_vec()].concat(),
        0o600,
    );
    assert!(ApiKey::read(&path).is_ok());
    let (_dir, path) = key_file("key-one", b"k", 0o400);
    assert!(ApiKey::read(&path).is_ok());
}

/// Catches: the key shown by `Debug` (of the key or of the client that holds it), or surrounding
/// whitespace of the file sent to NanoHUB as part of the key.
#[tokio::test]
async fn a_key_is_trimmed_and_never_shown() {
    let (_dir, path) = key_file("key-trim", format!("\n  {KEY}\t \r\n").as_bytes(), 0o600);
    let key = ApiKey::read(&path).expect("the key");
    assert_eq!(format!("{key:?}"), "ApiKey(..)");
    assert_eq!(format!("{key:#?}"), "ApiKey(..)");
    let hub = FakeNanoHub::start().await;
    let client = NanoHub::new(hub.url(), key);
    assert!(!format!("{client:?}").contains(KEY));
    let uuid = CommandUuid::from_random([1; 16]);
    client
        .enqueue(&id(UDID), &Command::security_info(uuid))
        .await
        .expect("NanoHUB takes the trimmed key");
    assert_eq!(hub.heard().len(), 1);
}

async fn enqueue_to(hub: &FakeNanoHub) -> Result<Queued, EnqueueError> {
    let uuid = CommandUuid::from_random([9; 16]);
    hub.client()
        .enqueue(&id(UDID), &Command::device_information(uuid))
        .await
}

/// Catches: the request not what NanoHUB's enqueue API takes: the method, the path with the
/// enrollment id, the `Content-Type`, the basic authentication of the user `nanohub`, or the
/// command plist as the body.
#[tokio::test]
async fn the_request_is_a_put_of_the_command_plist_with_basic_authentication() {
    let hub = FakeNanoHub::start().await;
    assert_eq!(enqueue_to(&hub).await, Ok(Queued { push_error: None }));
    let heard = hub.heard();
    assert_eq!(heard.len(), 1);
    assert_eq!(heard[0].method, "PUT");
    assert_eq!(heard[0].path, format!("/api/v1/nanomdm/enqueue/{UDID}"));
    assert_eq!(heard[0].content_type.as_deref(), Some("application/xml"));
    assert_eq!(
        heard[0].authorization.as_deref(),
        Some(format!("Basic {}", STANDARD.encode(format!("nanohub:{KEY}"))).as_str())
    );
    let uuid = CommandUuid::from_random([9; 16]);
    assert_eq!(
        heard[0].body,
        Command::device_information(uuid.clone()).to_plist()
    );
    assert_eq!(
        heard[0].command(),
        ("DeviceInformation".to_owned(), uuid.to_string())
    );
}

/// Catches: a wrong key reported as anything but the refusal of the key, or the refusal's text
/// holding the key or its encoding.
#[tokio::test]
async fn a_wrong_key_is_unauthorized_and_the_text_holds_no_key() {
    let hub = FakeNanoHub::start().await;
    let (_dir, path) = key_file("key-wrong", b"another-key-entirely", 0o600);
    let client = NanoHub::new(hub.url(), ApiKey::read(&path).expect("a key"));
    let uuid = CommandUuid::from_random([9; 16]);
    let error = client
        .enqueue(&id(UDID), &Command::device_information(uuid))
        .await
        .expect_err("unauthorized");
    assert_eq!(error, EnqueueError::Unauthorized);
    assert_eq!(
        error.to_string(),
        "NanoHUB's API refused the key of --mdm-api-key-file"
    );
    // A 401 is the refusal whatever its body.
    hub.answer(Answer::Status(
        401,
        json!({"command_error": "x"}).to_string(),
    ));
    assert_eq!(enqueue_to(&hub).await, Err(EnqueueError::Unauthorized));
}

/// Catches: a command NanoHUB did not queue taken as sent; its reason, from the top of the
/// result or from the device's entry, lost or taken from the wrong status code.
#[tokio::test]
async fn a_command_error_is_not_queued_wherever_it_is_and_whatever_the_status() {
    let hub = FakeNanoHub::start().await;
    let cases = [
        (200, json!({"command_error": "bad plist"}), "bad plist"),
        (500, json!({"command_error": "bad plist"}), "bad plist"),
        (
            200,
            json!({"status": {UDID: {"command_error": "no enrollment"}}}),
            "no enrollment",
        ),
        (
            500,
            json!({"status": {UDID: {"command_error": "no enrollment"}}}),
            "no enrollment",
        ),
        (
            200,
            json!({"command_error": "top", "status": {UDID: {"command_error": "id"}}}),
            "top",
        ),
        (200, json!({"command_error": 5}), "an error"),
        (400, json!({"command_error": "rejected"}), "rejected"),
    ];
    for (status, body, why) in cases {
        hub.answer(Answer::Status(status, body.to_string()));
        let error = enqueue_to(&hub).await.expect_err(why);
        assert_eq!(error, EnqueueError::NotQueued(why.to_owned()), "{body}");
        assert_eq!(
            error.to_string(),
            format!("NanoHUB did not queue the command: {why}")
        );
    }
}

/// Catches: a queued command whose push failed treated as not queued, or its push error lost;
/// a 500 that says nothing taken as queued.
#[tokio::test]
async fn a_push_error_is_still_queued() {
    let hub = FakeNanoHub::start().await;
    let queued = |why: Option<&str>| {
        Ok(Queued {
            push_error: why.map(str::to_owned),
        })
    };
    for (status, body) in [
        (500, json!({"push_error": "apns down"})),
        (500, json!({"status": {UDID: {"push_error": "apns down"}}})),
        (200, json!({"push_error": "apns down"})),
        (200, json!({"status": {UDID: {"push_error": "apns down"}}})),
    ] {
        hub.answer(Answer::Status(status, body.to_string()));
        assert_eq!(enqueue_to(&hub).await, queued(Some("apns down")), "{body}");
    }
    hub.answer(Answer::Status(200, "{}".to_owned()));
    assert_eq!(enqueue_to(&hub).await, queued(None));
    hub.answer(Answer::Status(
        200,
        json!({"status": {UDID: {}}}).to_string(),
    ));
    assert_eq!(enqueue_to(&hub).await, queued(None));
}

/// Catches: an answer the client does not read taken as queued: a 500 with neither error, a
/// non-JSON body, another status, a body over 64 KiB; and the text of the refusal.
#[tokio::test]
async fn an_answer_that_is_not_nanohub_s_result_is_an_answer_error() {
    let hub = FakeNanoHub::start().await;
    let big = format!("{{\"push_error\": \"{}\"}}", "x".repeat(70 * 1024));
    let cases = [
        (500, "{}".to_owned(), 500),
        (500, json!({"status": {UDID: {}}}).to_string(), 500),
        (200, "queued, thanks".to_owned(), 200),
        (200, String::new(), 200),
        (200, "[1]".to_owned(), 200),
        (207, json!({"status": {}}).to_string(), 207),
        (404, "{}".to_owned(), 404),
        (302, "{}".to_owned(), 302),
        (200, big, 200),
    ];
    for (status, body, said) in cases {
        hub.answer(Answer::Status(status, body.clone()));
        let error = enqueue_to(&hub).await.expect_err(&format!("{status}"));
        assert_eq!(error, EnqueueError::Answer { status: said });
        assert_eq!(
            error.to_string(),
            format!("NanoHUB's API answered {said} with no result it reads")
        );
    }
}

/// Catches: a connection refused reported without the address, or with the OS error's text
/// that may vary; a port left behind by a listener that is gone.
#[tokio::test]
async fn a_refused_connection_names_the_address() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    let client = NanoHub::new(
        ApiUrl::parse(&format!("http://127.0.0.1:{port}")).expect("a URL"),
        support::key(),
    );
    let uuid = CommandUuid::from_random([9; 16]);
    let error = client
        .enqueue(&id(UDID), &Command::device_information(uuid.clone()))
        .await
        .expect_err("refused");
    assert_eq!(
        error,
        EnqueueError::Connect {
            url: format!("http://127.0.0.1:{port}"),
            why: "connection refused".to_owned()
        }
    );
    assert_eq!(
        error.to_string(),
        format!("NanoHUB's API at http://127.0.0.1:{port} cannot be reached: connection refused")
    );
    assert!(!error.to_string().contains(KEY));

    // The same port on the IPv6 loopback is named in brackets.
    let client = NanoHub::new(
        ApiUrl::parse(&format!("http://[::1]:{port}")).expect("a URL"),
        support::key(),
    );
    let error = client
        .enqueue(&id(UDID), &Command::device_information(uuid))
        .await
        .expect_err("nothing listens");
    assert!(
        matches!(&error, EnqueueError::Connect { url, .. } if *url == format!("http://[::1]:{port}")),
        "{error}"
    );
}

/// Catches: a server that never answers holding the gate's event for ever; the refusal's text.
/// The clock is paused and advances by itself, so no time is measured.
#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_times_out() {
    let hub = FakeNanoHub::start().await;
    hub.answer(Answer::Hang);
    let error = enqueue_to(&hub).await.expect_err("times out");
    assert_eq!(error, EnqueueError::TimedOut);
    assert_eq!(error.to_string(), "NanoHUB's API did not answer within 10s");
}

/// Catches: a server that closes the connection (or says what is not HTTP) taken as an answer;
/// the HTTP error's text holding more than hyper's reason.
#[tokio::test]
async fn a_connection_that_ends_without_an_answer_is_an_http_error() {
    for reply in [&b""[..], b"this is not http\r\n\r\n"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let port = listener.local_addr().expect("address").port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("a connection");
            let _ = stream.write_all(reply).await;
            let _ = stream.shutdown().await;
        });
        let client = NanoHub::new(
            ApiUrl::parse(&format!("http://127.0.0.1:{port}")).expect("a URL"),
            support::key(),
        );
        let uuid = CommandUuid::from_random([9; 16]);
        let error = client
            .enqueue(&id(UDID), &Command::device_information(uuid))
            .await
            .expect_err("no answer");
        assert!(matches!(&error, EnqueueError::Http(_)), "{error}");
        assert!(error.to_string().starts_with("NanoHUB's API: "), "{error}");
        assert!(!error.to_string().contains(KEY));
    }
}

/// Catches: a type off the list reaching NanoHUB; the type shown unfiltered in the refusal.
#[tokio::test]
async fn a_type_off_the_list_is_refused_before_nanohub_is_called() {
    let hub = FakeNanoHub::start().await;
    let uuid = CommandUuid::from_random([9; 16]);
    let error = hub
        .client()
        .enqueue(
            &id(UDID),
            &Command::new("EraseDevice", uuid.clone(), Dictionary::new()),
        )
        .await
        .expect_err("refused");
    assert_eq!(
        error.to_string(),
        "the gate does not send EraseDevice commands"
    );
    let error = hub
        .client()
        .enqueue(
            &id(UDID),
            &Command::new("Er\u{7}ase\u{e9}", uuid, Dictionary::new()),
        )
        .await
        .expect_err("refused");
    assert_eq!(error.to_string(), "the gate does not send Er?ase? commands");
    assert!(hub.heard().is_empty());
}

/// Catches: the text of an error holding the key or its base64 in the basic authentication,
/// for any of the errors a call ends with. The full text of each is compared.
#[tokio::test]
async fn no_error_text_holds_the_key() {
    let hub = FakeNanoHub::start().await;
    let encoded = STANDARD.encode(format!("nanohub:{KEY}"));
    let mut seen = Vec::new();
    for answer in [
        Answer::Status(401, String::new()),
        Answer::Status(200, "nope".to_owned()),
        Answer::Status(200, json!({"command_error": "refused"}).to_string()),
    ] {
        hub.answer(answer);
        seen.push(enqueue_to(&hub).await.expect_err("an error").to_string());
    }
    assert_eq!(
        seen,
        [
            "NanoHUB's API refused the key of --mdm-api-key-file",
            "NanoHUB's API answered 200 with no result it reads",
            "NanoHUB did not queue the command: refused",
        ]
    );
    for text in &seen {
        assert!(!text.contains(KEY) && !text.contains(&encoded));
    }
}
