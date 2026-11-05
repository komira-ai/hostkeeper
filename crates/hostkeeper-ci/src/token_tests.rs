//! The OIDC token request against a plain-HTTP server on a free loopback port of the test's
//! own: it records the request head and answers once.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

use crate::token::{OidcTokens, TokenError};

/// A server that answers one request with `status` and `body`; joining it gives the request head.
fn serve(status: &str, body: &str) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let url = format!(
        "http://{}/token",
        listener.local_addr().expect("its address")
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("a request head");
            head.push(byte[0]);
        }
        stream.write_all(response.as_bytes()).expect("the reply");
        String::from_utf8(head).expect("an ASCII head")
    });
    (url, handle)
}

fn source(url: &str) -> OidcTokens {
    OidcTokens::new(
        Some((url.to_owned(), "request-secret".to_owned())),
        &[],
        false,
    )
}

/// Catches a request that leaves out the audience, sends the bearer in another form, or
/// returns something other than the reply's token.
#[test]
fn the_token_request_carries_the_audience_and_the_bearer() {
    let (url, server) = serve("200 OK", r#"{"count":1,"value":"the-token"}"#);
    let token = source(&url).token("hostkeeper-ci").expect("a token");
    assert_eq!(token, "the-token");
    let head = server.join().expect("the server");
    assert_eq!(
        head.lines().next(),
        Some("GET /token?audience=hostkeeper-ci HTTP/1.1")
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("\r\nauthorization: bearer request-secret\r\n"),
        "{head}"
    );
}

/// Catches an error that shows the reply's body or the bearer, and a refused request taken for
/// a token.
#[test]
fn a_refused_token_request_names_only_its_status() {
    let (url, server) = serve("403 Forbidden", r#"{"value":"must-not-be-used"}"#);
    let error = source(&url).token("hostkeeper-ci").expect_err("a refusal");
    server.join().expect("the server");
    assert!(matches!(error, TokenError::Status(403)));
    assert_eq!(
        error.to_string(),
        "the OIDC token request was answered with status 403"
    );
}

/// Catches a 200 reply that is taken for a token without holding one.
#[test]
fn a_reply_without_a_token_is_no_token() {
    for body in [
        r#"{"count":1}"#,
        r#"{"value":""}"#,
        r#"{"value":7}"#,
        "not json",
        "",
    ] {
        let (url, server) = serve("200 OK", body);
        let error = source(&url).token("a").expect_err("no token");
        server.join().expect("the server");
        assert_eq!(
            error.to_string(),
            "the OIDC token reply holds no token",
            "{body}"
        );
    }
}

/// Catches a job without the platform's variables that makes a request anyway.
#[test]
fn without_the_request_variables_there_is_no_request() {
    let error = OidcTokens::new(None, &[], true)
        .token("a")
        .expect_err("no variables");
    assert!(matches!(error, TokenError::NoVariables));
    assert_eq!(
        error.to_string(),
        "no OIDC token request variables: the calling job must grant id-token: write"
    );
}

/// Catches a token request over plain HTTP in production, and a transport error that repeats
/// the bearer.
#[test]
fn a_plain_http_url_is_refused_when_https_only() {
    let error = OidcTokens::new(
        Some((
            "http://127.0.0.1:1/token".to_owned(),
            "request-secret".to_owned(),
        )),
        &[],
        true,
    )
    .token("a")
    .expect_err("https only");
    let text = error.to_string();
    assert!(
        text.starts_with("the OIDC token request failed: "),
        "{text}"
    );
    assert!(!text.contains("request-secret"), "{text}");
}
