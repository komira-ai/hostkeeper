//! A TLS server on a loopback port for the tests of the two clients: HTTP/2 for APNs, HTTP/1.1
//! for mdmcert.download, with a CA of its own, a scripted answer per request, a record of what it
//! saw (request, headers, body, client certificate), and a count of connections.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rcgen::{CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;

/// What the server saw of one request.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub apns_id: Option<String>,
    /// Close this connection gracefully (HTTP/2 GOAWAY) once this answer is sent.
    pub then_goaway: bool,
    /// Never answer.
    pub hang: bool,
    /// Fail the request: the connection is torn down with no answer.
    pub abort: bool,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            status: 200,
            body: String::new(),
            apns_id: Some("11111111-2222-3333-4444-555555555555".to_owned()),
            then_goaway: false,
            hang: false,
            abort: false,
        }
    }

    pub fn aborted() -> Self {
        Self {
            abort: true,
            ..Self::ok()
        }
    }

    pub fn status(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
            apns_id: None,
            then_goaway: false,
            hang: false,
            abort: false,
        }
    }
}

/// What the server does at the handshake with a client certificate.
#[derive(Clone, Debug)]
pub enum Auth {
    /// Does not ask for one.
    None,
    /// Asks for one and takes the certificate given (as its own trust anchor) and no other.
    Trusting(CertificateDer<'static>),
}

/// A running server.
pub struct Server {
    pub addr: SocketAddr,
    pub ca: CertificateDer<'static>,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub client_certs: Arc<Mutex<Vec<Vec<u8>>>>,
    pub connections: Arc<AtomicUsize>,
    script: Arc<Mutex<VecDeque<Reply>>>,
}

impl Server {
    /// Starts a server speaking HTTP/2 (`h2`) or HTTP/1.1 and asking for a client certificate as
    /// `auth` says.
    fn tls_config(h2: bool, auth: Auth) -> (ServerConfig, CertificateDer<'static>) {
        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("params");
        ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).expect("ca");
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf_key = KeyPair::generate().expect("leaf key");
        let leaf = CertificateParams::new(vec!["localhost".to_owned()])
            .expect("params")
            .signed_by(&leaf_key, &issuer)
            .expect("leaf");
        let builder =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("versions");
        let builder = match auth {
            Auth::None => builder.with_no_client_auth(),
            Auth::Trusting(certificate) => {
                let mut roots = RootCertStore::empty();
                roots.add(certificate).expect("a trust anchor");
                builder.with_client_cert_verifier(
                    WebPkiClientVerifier::builder_with_provider(
                        Arc::new(roots),
                        Arc::new(rustls::crypto::ring::default_provider()),
                    )
                    .build()
                    .expect("a verifier"),
                )
            }
        };
        let mut config = builder
            .with_single_cert(
                vec![leaf.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
            )
            .expect("server cert");
        config.alpn_protocols = vec![if h2 {
            b"h2".to_vec()
        } else {
            b"http/1.1".to_vec()
        }];
        (config, ca.der().clone())
    }

    /// Starts a server speaking HTTP/2 (`h2`) or HTTP/1.1 and asking for a client certificate as
    /// `auth` says.
    pub async fn start(h2: bool, auth: Auth) -> Self {
        let (config, ca) = Self::tls_config(h2, auth);
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = Self {
            addr,
            ca,
            seen: Arc::default(),
            client_certs: Arc::default(),
            connections: Arc::default(),
            script: Arc::default(),
        };
        let (seen, certs, connections, script) = (
            server.seen.clone(),
            server.client_certs.clone(),
            server.connections.clone(),
            server.script.clone(),
        );
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                connections.fetch_add(1, Ordering::SeqCst);
                let (acceptor, seen, certs, script) = (
                    acceptor.clone(),
                    seen.clone(),
                    certs.clone(),
                    script.clone(),
                );
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    if let Some(chain) = tls.get_ref().1.peer_certificates() {
                        certs
                            .lock()
                            .unwrap()
                            .extend(chain.iter().map(|der| der.as_ref().to_vec()));
                    }
                    serve(tls, h2, seen, script).await;
                });
            }
        });
        server
    }

    /// Starts an HTTP/2 server that answers every connection with a GOAWAY carrying `payload`
    /// and no response.
    pub async fn start_goaway(payload: &str) -> Self {
        let (config, ca) = Self::tls_config(true, Auth::None);
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let mut frames = vec![0, 0, 0, 4, 0, 0, 0, 0, 0];
        let mut goaway = vec![0, 0, 0, 0];
        goaway.extend(0_u32.to_be_bytes());
        goaway.extend(payload.as_bytes());
        let length = u32::try_from(goaway.len()).expect("short");
        frames.extend(&length.to_be_bytes()[1..]);
        frames.extend([7, 0, 0, 0, 0, 0]);
        frames.extend(goaway);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, frames) = (acceptor.clone(), frames.clone());
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut preface = [0_u8; 64];
                    drop(tls.read(&mut preface).await);
                    drop(tls.write_all(&frames).await);
                    let mut rest = Vec::new();
                    drop(tls.read_to_end(&mut rest).await);
                });
            }
        });
        Self {
            addr,
            ca,
            seen: Arc::default(),
            client_certs: Arc::default(),
            connections: Arc::default(),
            script: Arc::default(),
        }
    }

    /// Queues the answer of the next request.
    pub fn answer_next(&self, reply: Reply) {
        self.script.lock().unwrap().push_back(reply);
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn serve(
    tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    h2: bool,
    seen: Arc<Mutex<Vec<Seen>>>,
    script: Arc<Mutex<VecDeque<Reply>>>,
) {
    let goaway = Arc::new(Notify::new());
    let signal = goaway.clone();
    let service = service_fn(move |request: Request<Incoming>| {
        let (seen, script, signal) = (seen.clone(), script.clone(), signal.clone());
        async move {
            let (parts, body) = request.into_parts();
            let body = body
                .collect()
                .await
                .map(|b| b.to_bytes().to_vec())
                .unwrap_or_default();
            seen.lock().unwrap().push(Seen {
                method: parts.method.to_string(),
                path: parts.uri.path().to_owned(),
                headers: parts
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_owned()))
                    .collect(),
                body,
            });
            let reply = script.lock().unwrap().pop_front().unwrap_or_else(Reply::ok);
            if reply.hang {
                std::future::pending::<()>().await;
            }
            if reply.abort {
                return Err(std::io::Error::other("the server aborts the request"));
            }
            if reply.then_goaway {
                signal.notify_one();
            }
            let mut answer = Response::builder().status(reply.status);
            if let Some(id) = &reply.apns_id {
                answer = answer.header("apns-id", id);
            }
            Ok::<_, std::io::Error>(
                answer
                    .body(Full::new(Bytes::from(reply.body)))
                    .expect("response"),
            )
        }
    });
    if h2 {
        let conn = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(TokioIo::new(tls), service);
        tokio::pin!(conn);
        tokio::select! {
            _ = conn.as_mut() => {}
            () = goaway.notified() => {
                conn.as_mut().graceful_shutdown();
                drop(conn.await);
            }
        }
    } else {
        drop(
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tls), service)
                .await,
        );
    }
}
