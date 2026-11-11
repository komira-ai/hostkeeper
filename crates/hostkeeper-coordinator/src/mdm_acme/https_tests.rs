use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::{HttpRequest, Https, MAX_REPLY_BYTES, RealHttps};
use crate::mdm_acme::testkit::tls_server;

/// What the loopback server heard of the last request.
#[derive(Clone, Debug, Default)]
struct Heard {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A TLS server for `localhost` on a port of its own, answering `status` with `body` and the
/// header `x-answer`, and the client that trusts it.
async fn server(status: u16, body: Vec<u8>) -> (RealHttps, u16, Arc<Mutex<Heard>>) {
    let (roots, config) = tls_server("localhost");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bound");
    let port = listener.local_addr().expect("an address").port();
    let heard = Arc::new(Mutex::new(Heard::default()));
    let seen = Arc::clone(&heard);
    tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(config);
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let seen = Arc::clone(&seen);
            let body = body.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                    let seen = Arc::clone(&seen);
                    let body = body.clone();
                    async move {
                        let (parts, incoming) = request.into_parts();
                        let bytes = incoming.collect().await.expect("a body").to_bytes();
                        *seen.lock().expect("lock") = Heard {
                            method: parts.method.to_string(),
                            target: parts.uri.to_string(),
                            headers: parts
                                .headers
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
                                .collect(),
                            body: bytes.to_vec(),
                        };
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(status)
                                .header("x-answer", "yes")
                                .body(Full::new(Bytes::from(body)))
                                .expect("a response"),
                        )
                    }
                });
                drop(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service)
                        .await,
                );
            });
        }
    });
    let client = RealHttps::trusting(roots, &format!("127.0.0.1:{port}"), Duration::from_secs(20));
    (client, port, heard)
}

fn request(method: &str, url: &str, body: &[u8]) -> HttpRequest {
    HttpRequest {
        method: method.to_owned(),
        url: url.to_owned(),
        headers: vec![("x-ask".to_owned(), "please".to_owned())],
        body: body.to_vec(),
    }
}

/// Proves: a request is sent to the URL's path and query with the method, the body, the caller's
/// headers, the host header and the client's user agent, over TLS to the server's name, and the
/// reply carries its status, its headers and its body; a 404 is a reply and not an error.
///
/// Catches: a client that drops the query, sends no host, or turns a 4xx into a failure the
/// DNS provider's 401 would hide behind.
#[tokio::test]
async fn a_request_is_sent_whole_and_any_status_is_a_reply() {
    let (client, _, heard) = server(404, b"nothing here".to_vec()).await;
    let reply = client
        .send(request("POST", "https://localhost:1/a/b?c=d", b"payload"))
        .await
        .expect("a reply");
    assert_eq!(reply.status, 404);
    assert_eq!(reply.body, b"nothing here");
    assert!(
        reply
            .headers
            .contains(&("x-answer".to_owned(), "yes".to_owned()))
    );
    let heard = heard.lock().expect("lock").clone();
    assert_eq!(heard.method, "POST");
    assert_eq!(heard.target, "/a/b?c=d");
    assert_eq!(heard.body, b"payload");
    let header = |name: &str| {
        heard
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("x-ask"), Some("please"));
    assert_eq!(header("host"), Some("localhost"));
    assert_eq!(header("user-agent"), Some("hostkeeper-coordinator"));
}

/// Proves: a URL with no path asks for `/`.
#[tokio::test]
async fn a_url_with_no_path_asks_for_the_root() {
    let (client, _, heard) = server(200, Vec::new()).await;
    client
        .send(request("GET", "https://localhost", b""))
        .await
        .expect("a reply");
    assert_eq!(heard.lock().expect("lock").target, "/");
}

/// Proves: a request that cannot be made is a failure that says why and sends nothing: a URL that
/// does not parse, a URL that is not https, a URL with no host, a method or a header that is not
/// one; and a reply body over the limit is a failure, not a truncated body.
///
/// Catches: a reply of any size read into memory, and a plain-http URL sent a credential.
#[tokio::test]
async fn a_request_that_cannot_be_made_is_a_failure() {
    let (client, _, heard) = server(200, vec![b'x'; MAX_REPLY_BYTES + 1]).await;
    let failure = |request: HttpRequest| {
        let client = &client;
        async move { client.send(request).await.expect_err("a failure").0 }
    };
    assert_eq!(
        failure(request("GET", "not a url", b"")).await,
        "invalid uri character"
    );
    assert_eq!(
        failure(request("GET", "http://localhost/", b"")).await,
        "the URL is not an https URL"
    );
    assert_eq!(
        failure(request("GET", "https:///path", b"")).await,
        "invalid format"
    );
    assert_eq!(
        failure(request("BAD METHOD", "https://localhost/", b"")).await,
        "invalid HTTP method"
    );
    let mut bad = request("GET", "https://localhost/", b"");
    bad.headers = vec![("bad name".to_owned(), "x".to_owned())];
    assert_eq!(failure(bad).await, "invalid HTTP header name");
    let mut bad = request("GET", "https://localhost/", b"");
    bad.headers = vec![("x-ask".to_owned(), "line\nbreak".to_owned())];
    assert_eq!(failure(bad).await, "failed to parse header value");
    assert_eq!(heard.lock().expect("lock").method, "");
    assert_eq!(
        failure(request("GET", "https://localhost/", b"")).await,
        "length limit exceeded"
    );
}

/// Proves: a server whose certificate the client does not trust, and a port nothing listens on,
/// are failures.
#[tokio::test]
async fn an_untrusted_or_absent_server_is_a_failure() {
    let (_, port, _) = server(200, Vec::new()).await;
    let other = tls_server("localhost").0;
    let client = RealHttps::trusting(other, &format!("127.0.0.1:{port}"), Duration::from_secs(20));
    assert!(
        client
            .send(request("GET", "https://localhost/", b""))
            .await
            .is_err()
    );
    let closed = TcpListener::bind("127.0.0.1:0").await.expect("bound");
    let gone = closed.local_addr().expect("an address").port();
    drop(closed);
    let client = RealHttps::trusting(
        tls_server("localhost").0,
        &format!("127.0.0.1:{gone}"),
        Duration::from_secs(20),
    );
    assert!(
        client
            .send(request("GET", "https://localhost/", b""))
            .await
            .is_err()
    );
}

/// Proves: a request that is not answered within the timeout is a failure that says it timed out
/// (a zero timeout is over before the exchange can start), and the production client trusts the
/// public roots and has the timeout it was given.
#[tokio::test]
async fn a_request_not_answered_in_time_is_a_failure() {
    // A listener that accepts the connection and never speaks TLS.
    let silent = TcpListener::bind("127.0.0.1:0").await.expect("bound");
    let dial = silent.local_addr().expect("an address").to_string();
    let client = RealHttps::trusting(tls_server("localhost").0, &dial, Duration::ZERO);
    let failure = client
        .send(request("GET", "https://localhost/", b""))
        .await
        .expect_err("a failure");
    assert_eq!(failure.0, "the request timed out");
    let production = RealHttps::production(Duration::from_secs(5));
    assert_eq!(production.timeout, Duration::from_secs(5));
    assert!(!production.roots.is_empty());
    assert!(production.dial.is_none());
}
