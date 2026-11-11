//! A private repository's origin served in process, for the tests of [`crate::repository_origin`]
//! and [`crate::repository_serve`]: the dial answers with the client end of an in-memory stream
//! and serves the other end with TLS (a certificate of the test's own for [`HOST`]), reads one
//! request's head, and answers as the test says by the request's path. No socket is opened.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::FutureExt as _;
use hostkeeper_oidc::{Dial, Roots, Stream};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio_rustls::TlsAcceptor;

/// The host name the origin's certificate is for.
pub(crate) const HOST: &str = "origin.test";

/// What the origin does once it has read a request's head.
#[derive(Clone)]
pub(crate) enum Then {
    /// Writes these bytes and closes.
    Write(Vec<u8>),
    /// Writes these bytes and then never writes or closes.
    Stall(Vec<u8>),
    /// Never answers.
    Hang,
}

/// What the origin does for a request path.
pub(crate) type Answer = Arc<dyn Fn(&str) -> Then + Send + Sync>;

/// What the origin does for a request's head (its request line and headers, as read).
pub(crate) type AnswerHead = Arc<dyn Fn(&str) -> Then + Send + Sync>;

/// A whole HTTP/1.1 answer: `status` (`200 OK`), a `Content-Length` of `body`'s length, `body`.
pub(crate) fn response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// An answer of `status` with `headers`, a `Content-Length` of the body's length, and `body`.
pub(crate) fn reply(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Then {
    let mut text = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = text.into_bytes();
    bytes.extend_from_slice(body);
    Then::Write(bytes)
}

/// An answer with exactly these `lines` as its head and `body` after it.
pub(crate) fn bare(lines: &[&str], body: &[u8]) -> Then {
    let mut bytes = format!("{}\r\n\r\n", lines.join("\r\n")).into_bytes();
    bytes.extend_from_slice(body);
    Then::Write(bytes)
}

/// An origin at [`HOST`] with a certificate of its own, and what it heard.
pub(crate) struct FakeOrigin {
    acceptor: TlsAcceptor,
    root: CertificateDer<'static>,
    heard: Arc<Mutex<Vec<String>>>,
    dialed: Arc<Mutex<Vec<(String, u16)>>>,
    dials: Arc<AtomicUsize>,
}

impl FakeOrigin {
    pub(crate) fn new() -> Self {
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
            dialed: Arc::default(),
            dials: Arc::default(),
        }
    }

    /// The roots that trust this origin.
    pub(crate) fn roots(&self) -> Roots {
        Roots::Given(vec![self.root.clone()])
    }

    /// A dial whose every connection is served by this origin, answering `answer` by path.
    pub(crate) fn dial(&self, answer: Answer) -> Dial {
        self.dial_heads(Arc::new(move |head: &str| {
            answer(head.split_whitespace().nth(1).unwrap_or_default())
        }))
    }

    /// A dial whose every connection is served by this origin, answering `answer` by the head of
    /// the request: its method, path and headers.
    pub(crate) fn dial_heads(&self, answer: AnswerHead) -> Dial {
        let (acceptor, heard, dialed, dials) = (
            self.acceptor.clone(),
            Arc::clone(&self.heard),
            Arc::clone(&self.dialed),
            Arc::clone(&self.dials),
        );
        Arc::new(move |host: String, port: u16| {
            dials.fetch_add(1, Ordering::SeqCst);
            dialed.lock().expect("the dials").push((host, port));
            let (client, server) = tokio::io::duplex(256 * 1024);
            tokio::spawn(serve(
                acceptor.clone(),
                server,
                Arc::clone(&answer),
                Arc::clone(&heard),
            ));
            async move { Ok::<Stream, std::io::Error>(Box::new(client)) }.boxed()
        })
    }

    /// A dial answering `then` to every path.
    pub(crate) fn dial_all(&self, then: Then) -> Dial {
        self.dial(Arc::new(move |_| then.clone()))
    }

    /// The head of every request read, in order.
    pub(crate) fn heard(&self) -> Vec<String> {
        self.heard.lock().expect("the heads").clone()
    }

    /// The host and port of every dial, in order.
    pub(crate) fn dialed(&self) -> Vec<(String, u16)> {
        self.dialed.lock().expect("the dials").clone()
    }

    /// How many connections were dialled.
    pub(crate) fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }
}

/// A dial whose connection is refused.
pub(crate) fn refusing_dial() -> Dial {
    Arc::new(|_: String, _: u16| {
        async { Err::<Stream, _>(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)) }
            .boxed()
    })
}

/// A dial that never connects.
pub(crate) fn hanging_dial() -> Dial {
    Arc::new(|_: String, _: u16| futures::future::pending().boxed())
}

/// Serves one connection: the handshake, the request's head, then what `answer` says of its path.
async fn serve(
    acceptor: TlsAcceptor,
    server: DuplexStream,
    answer: AnswerHead,
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
    let head = String::from_utf8_lossy(&head).into_owned();
    heard.lock().expect("the heads").push(head.clone());
    match answer(&head) {
        Then::Write(bytes) => {
            let _ = tls.write_all(&bytes).await;
            let _ = tls.shutdown().await;
        }
        Then::Stall(bytes) => {
            let _ = tls.write_all(&bytes).await;
            let _ = tls.flush().await;
            futures::future::pending::<()>().await;
        }
        Then::Hang => futures::future::pending::<()>().await,
    }
}
