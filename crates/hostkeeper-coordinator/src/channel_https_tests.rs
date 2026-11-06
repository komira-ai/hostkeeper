//! The HTTPS fetch from the release channel ([`crate::channel_https::HttpsChannel`]) against a
//! channel served in process: the dial answers with the client end of an in-memory stream and
//! serves the other end with TLS (a certificate of the test's own) and one canned HTTP answer.
//! No socket is opened. The deadlines run on tokio's paused clock, which moves only when every
//! task waits, so a deadline is reached without any time passing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt as _;
use hostkeeper_oidc::{Dial, Roots, Stream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio_rustls::TlsAcceptor;

use crate::channel::{ChannelFetcher, FetchError, Fetched, MAX_INDEX_BYTES};
use crate::channel_https::{ChannelTlsError, ChannelUrl, HttpsChannel};

const HOST: &str = "channel.test";
const PATH: &str = "kbf/2.0/index.json";

/// What the channel does once a request has been read.
#[derive(Clone)]
enum Then {
    /// Writes these bytes and closes.
    Write(Vec<u8>),
    /// Never answers.
    Hang,
}

/// A channel at [`HOST`] with a certificate of its own, and what it heard.
struct Channel {
    acceptor: TlsAcceptor,
    root: CertificateDer<'static>,
    /// The head of every request read, in order.
    heard: Arc<Mutex<Vec<String>>>,
    /// How many connections were dialled.
    dials: Arc<AtomicUsize>,
}

impl Channel {
    fn new() -> Self {
        let certified =
            rcgen::generate_simple_self_signed(vec![HOST.to_owned()]).expect("a certificate");
        let root = certified.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(vec![root.clone()], key)
            .expect("a server config");
        Self {
            acceptor: TlsAcceptor::from(Arc::new(config)),
            root,
            heard: Arc::default(),
            dials: Arc::default(),
        }
    }

    fn roots(&self) -> Roots {
        Roots::Given(vec![self.root.clone()])
    }

    /// A dial whose every connection is served by this channel, answering `then`.
    fn dial(&self, then: Then) -> Dial {
        let (acceptor, heard, dials) = (
            self.acceptor.clone(),
            Arc::clone(&self.heard),
            Arc::clone(&self.dials),
        );
        Arc::new(move |_host: String, _port: u16| {
            dials.fetch_add(1, Ordering::SeqCst);
            let (client, server) = tokio::io::duplex(256 * 1024);
            tokio::spawn(serve(
                acceptor.clone(),
                server,
                then.clone(),
                Arc::clone(&heard),
            ));
            async move { Ok::<Stream, std::io::Error>(Box::new(client)) }.boxed()
        })
    }

    /// The fetcher of `url` trusting this channel, dialling it to answer `then`.
    fn fetcher(&self, url: &str, then: Then) -> HttpsChannel {
        fetcher_with(url, self.roots(), self.dial(then))
    }

    fn heard(&self) -> Vec<String> {
        self.heard.lock().expect("the heads").clone()
    }
}

fn fetcher_with(url: &str, roots: Roots, dial: Dial) -> HttpsChannel {
    HttpsChannel::new(
        ChannelUrl::parse(url).expect("a channel URL"),
        roots,
        Duration::from_secs(10),
        Duration::from_secs(30),
    )
    .expect("a fetcher")
    .with_dial(dial)
}

/// Serves one connection: the handshake, the request's head, then `then`.
async fn serve(
    acceptor: TlsAcceptor,
    server: DuplexStream,
    then: Then,
    heard: Arc<Mutex<Vec<String>>>,
) {
    let Ok(mut tls) = acceptor.accept(server).await else {
        return;
    };
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    heard
        .lock()
        .expect("the heads")
        .push(String::from_utf8_lossy(&head).into_owned());
    match then {
        Then::Write(bytes) => {
            let _ = tls.write_all(&bytes).await;
            let _ = tls.shutdown().await;
        }
        Then::Hang => futures::future::pending::<()>().await,
    }
}

/// An HTTP/1.1 answer of `status` whose `Content-Length` is `length` and whose body is `body`.
fn answer(status: &str, length: usize, body: &[u8]) -> Then {
    let mut bytes =
        format!("HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")
            .into_bytes();
    bytes.extend_from_slice(body);
    Then::Write(bytes)
}

fn whole(status: &str, body: &[u8]) -> Then {
    answer(status, body.len(), body)
}

/// Catches: the index read from anywhere but `<base>/<service>/<version>/index.json` on the
/// channel's host, a Host header without the port the URL names, a body cut or changed, and a
/// body of exactly the cap refused.
#[tokio::test]
async fn a_200_is_the_index_read_from_the_path_under_the_base() {
    let channel = Channel::new();
    let index = br#"{"packages":[],"schema":1,"service":"kbf","version":"2.0"}"#;
    let fetcher = channel.fetcher(
        "https://channel.test:8443/releases/",
        whole("200 OK", index),
    );
    assert_eq!(
        fetcher.fetch(PATH).await,
        Ok(Fetched::Found(index.to_vec()))
    );
    let heard = channel.heard();
    assert_eq!(heard.len(), 1);
    assert!(
        heard[0].starts_with("GET /releases/kbf/2.0/index.json HTTP/1.1\r\n"),
        "{heard:?}"
    );
    let head = heard[0].to_ascii_lowercase();
    assert!(head.contains("\r\nhost: channel.test:8443\r\n"), "{head}");
    assert!(
        head.contains("\r\nuser-agent: hostkeeper-coordinator\r\n"),
        "{head}"
    );

    let full = vec![b'x'; MAX_INDEX_BYTES];
    let at_root = channel.fetcher("https://channel.test", whole("200 OK", &full));
    assert_eq!(at_root.fetch(PATH).await, Ok(Fetched::Found(full)));
    assert!(
        channel.heard()[1].starts_with("GET /kbf/2.0/index.json HTTP/1.1\r\n"),
        "{:?}",
        channel.heard()
    );
    assert!(format!("{at_root:?}").contains("https://channel.test"));
}

/// Catches: a 404 read as an error (it is the one answer that says the channel holds nothing
/// there), and any other status, a redirect among them, read as the index or followed: one
/// connection is dialled for each fetch, whatever the answer.
#[tokio::test]
async fn a_404_is_not_found_and_any_other_status_is_an_error() {
    let channel = Channel::new();
    let url = format!("https://{HOST}/{PATH}");
    let not_found = channel.fetcher(&format!("https://{HOST}"), whole("404 Not Found", b"no"));
    assert_eq!(not_found.fetch(PATH).await, Ok(Fetched::NotFound));
    for (status, code) in [
        ("301 Moved Permanently", 301),
        ("302 Found", 302),
        ("307 Temporary Redirect", 307),
        ("403 Forbidden", 403),
        ("500 Internal Server Error", 500),
        ("503 Service Unavailable", 503),
    ] {
        let bytes = format!(
            "HTTP/1.1 {status}\r\nLocation: https://elsewhere.test/{PATH}\r\nContent-Length: 0\r\n\r\n"
        )
        .into_bytes();
        let fetcher = channel.fetcher(&format!("https://{HOST}"), Then::Write(bytes));
        assert_eq!(
            fetcher.fetch(PATH).await,
            Err(FetchError::Status {
                url: url.clone(),
                status: code,
            }),
            "{status}"
        );
    }
    assert_eq!(channel.dials.load(Ordering::SeqCst), 7);
}

/// Catches: an answer longer than the cap read whole (by its length or by its bytes), and an
/// answer cut short or no HTTP at all taken for an index.
#[tokio::test]
async fn an_answer_over_the_cap_or_broken_is_an_error() {
    let channel = Channel::new();
    let url = format!("https://{HOST}/{PATH}");
    let base = format!("https://{HOST}");
    let over = vec![b'x'; MAX_INDEX_BYTES + 1];
    let too_large = channel.fetcher(&base, whole("200 OK", &over));
    assert_eq!(
        too_large.fetch(PATH).await,
        Err(FetchError::TooLarge { url: url.clone() })
    );
    let cut = channel.fetcher(&base, answer("200 OK", 100, b"{\"packages\""));
    assert!(
        matches!(cut.fetch(PATH).await, Err(FetchError::Http { url: at, .. }) if at == url),
        "a cut body"
    );
    let not_http = channel.fetcher(&base, Then::Write(b"not http at all\r\n\r\n".to_vec()));
    assert!(
        matches!(not_http.fetch(PATH).await, Err(FetchError::Http { url: at, .. }) if at == url),
        "no HTTP"
    );
}

/// Catches: a channel that cannot be reached, whose handshake breaks, or whose certificate
/// does not chain to the roots given, read as anything but a failed connection.
#[tokio::test]
async fn a_connection_that_fails_is_a_connect_error() {
    let channel = Channel::new();
    let base = format!("https://{HOST}");
    let url = format!("{base}/{PATH}");
    let refused: Dial = Arc::new(|_host: String, _port: u16| {
        async { Err::<Stream, _>(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)) }
            .boxed()
    });
    assert_eq!(
        fetcher_with(&base, channel.roots(), refused)
            .fetch(PATH)
            .await,
        Err(FetchError::Connect {
            url: url.clone(),
            why: "connection refused".to_owned(),
        })
    );
    let dead: Dial = Arc::new(|_host: String, _port: u16| {
        let (client, _server) = tokio::io::duplex(1024);
        async move { Ok::<Stream, std::io::Error>(Box::new(client)) }.boxed()
    });
    assert!(matches!(
        fetcher_with(&base, channel.roots(), dead).fetch(PATH).await,
        Err(FetchError::Connect { url: at, .. }) if at == url
    ));
    let stranger = Channel::new();
    let untrusted = fetcher_with(
        &base,
        stranger.roots(),
        channel.dial(whole("200 OK", b"{}")),
    );
    let why = untrusted.fetch(PATH).await.expect_err("untrusted");
    assert!(
        matches!(&why, FetchError::Connect { url: at, why } if *at == url && why.contains("certificate")),
        "{why:?}"
    );
    assert_eq!(channel.heard(), Vec::<String>::new());
}

/// Catches: a connection that never completes, or a channel that never answers, waited on for
/// ever, or the two deadlines confused: each ends the fetch with its own error.
#[tokio::test(start_paused = true)]
async fn each_deadline_ends_the_fetch_with_its_own_error() {
    let channel = Channel::new();
    let base = format!("https://{HOST}");
    let url = format!("{base}/{PATH}");
    let never: Dial = Arc::new(|_host: String, _port: u16| {
        futures::future::pending::<std::io::Result<Stream>>().boxed()
    });
    assert_eq!(
        fetcher_with(&base, channel.roots(), never)
            .fetch(PATH)
            .await,
        Err(FetchError::ConnectTimedOut {
            url: url.clone(),
            secs: 10,
        })
    );
    let silent = channel.fetcher(&base, Then::Hang);
    assert_eq!(
        silent.fetch(PATH).await,
        Err(FetchError::TimedOut {
            url: url.clone(),
            secs: 30,
        })
    );
    assert_eq!(channel.heard().len(), 1);
}

/// Catches: a fetcher made with no root it can trust (every fetch would then fail at the
/// handshake, which a start should say instead).
#[test]
fn a_fetcher_with_no_usable_root_is_refused() {
    let url = ChannelUrl::parse("https://channel.test").expect("a channel URL");
    for roots in [
        Roots::Given(Vec::new()),
        Roots::Given(vec![CertificateDer::from(b"not a certificate".to_vec())]),
    ] {
        let why = HttpsChannel::new(
            url.clone(),
            roots,
            Duration::from_secs(10),
            Duration::from_secs(30),
        )
        .expect_err("no roots");
        assert!(matches!(why, ChannelTlsError::NoRoots));
        assert_eq!(
            why.to_string(),
            "no trusted root certificates for fetching from the release channel"
        );
    }
}
