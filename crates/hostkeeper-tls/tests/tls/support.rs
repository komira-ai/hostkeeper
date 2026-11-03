//! A throwaway CA and leaf certificates, an in-process TLS server that echoes over a
//! [`TlsSource`]'s configuration, and a TLS client that reports which certificate it was served.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_testkit::Scratch;
use hostkeeper_tls::{TlsFiles, TlsSource};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;
use tokio_rustls::client::TlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// The interval the sources under test are made with; nothing polls, so it is only a value.
pub const INTERVAL: Duration = Duration::from_secs(1);

pub struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
    pub pem: String,
}

pub struct Leaf {
    pub cert: String,
    pub key: String,
    pub der: Vec<u8>,
}

pub fn ca(name: &str) -> Ca {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(DnType::CommonName, name);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("CA key"))
        .expect("CA certificate");
    let pem = issuer.pem();
    Ca { issuer, pem }
}

/// A leaf of `ca` for `name`, valid now or, with `expired`, valid only for one day that
/// is long over.
pub fn leaf(ca: &Ca, name: &str, usage: ExtendedKeyUsagePurpose, expired: bool) -> Leaf {
    let mut params = CertificateParams::new(vec![name.to_owned()]).expect("leaf params");
    params.distinguished_name.push(DnType::CommonName, name);
    params.extended_key_usages = vec![usage];
    if expired {
        params.not_before = rcgen::date_time_ymd(2026, 9, 2);
        params.not_after = rcgen::date_time_ymd(2026, 9, 3);
    }
    let key = KeyPair::generate().expect("leaf key");
    let cert = params.signed_by(&key, &*ca.issuer).expect("sign leaf");
    Leaf {
        cert: cert.pem(),
        key: key.serialize_pem(),
        der: cert.der().to_vec(),
    }
}

pub fn server_leaf(ca: &Ca) -> Leaf {
    leaf(ca, "localhost", ExtendedKeyUsagePurpose::ServerAuth, false)
}

pub fn expired_leaf(ca: &Ca) -> Leaf {
    leaf(ca, "localhost", ExtendedKeyUsagePurpose::ServerAuth, true)
}

pub fn client_leaf(ca: &Ca, name: &str) -> Leaf {
    leaf(ca, name, ExtendedKeyUsagePurpose::ClientAuth, false)
}

/// A scratch directory of this test's own under the target directory, empty.
pub fn scratch(test: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), test).expect("a scratch directory")
}

/// Replaces `path` whole (write aside, rename), as a renewal tool does, so a look never
/// sees half a file.
pub fn put(path: &Path, contents: &str) {
    let aside = path.with_extension("aside");
    std::fs::write(&aside, contents).expect("write aside");
    std::fs::rename(&aside, path).expect("rename into place");
}

/// The server's files in a scratch directory.
pub struct Files {
    pub files: TlsFiles,
    _scratch: Scratch,
}

impl Files {
    /// The server's files in a directory of `test`'s own: `leaf` as the identity, and
    /// `ca_pem` as the client CA if there is one.
    pub fn write(test: &str, leaf: &Leaf, ca_pem: Option<&str>) -> Self {
        let scratch = scratch(test);
        let dir = scratch.path();
        let files = TlsFiles {
            cert: dir.join("server.pem"),
            key: dir.join("server.key"),
            client_ca: ca_pem.map(|_| dir.join("client-ca.pem")),
        };
        put(&files.cert, &leaf.cert);
        put(&files.key, &leaf.key);
        if let (Some(path), Some(pem)) = (&files.client_ca, ca_pem) {
            put(path, pem);
        }
        Self {
            files,
            _scratch: scratch,
        }
    }

    pub fn source(&self) -> TlsSource {
        TlsSource::files("test", self.files.clone(), INTERVAL).expect("a source")
    }

    pub fn install(&self, leaf: &Leaf) {
        put(&self.files.cert, &leaf.cert);
        put(&self.files.key, &leaf.key);
    }
}

/// One client connection and the server's view of it.
pub struct Conn {
    pub tls: TlsStream<DuplexStream>,
    /// The client certificate chain the server saw, sent once the server's handshake is over.
    pub seen: oneshot::Receiver<Option<Vec<Vec<u8>>>>,
}

/// An echo server for one connection: the handshake under `source`'s configuration as it is
/// now, then every byte read written back. The chain the client presented goes to `seen`.
fn serve(source: &TlsSource, stream: DuplexStream, seen: oneshot::Sender<Option<Vec<Vec<u8>>>>) {
    let acceptor = TlsAcceptor::from(source.config());
    tokio::spawn(async move {
        let Ok(mut tls) = acceptor.accept(stream).await else {
            return;
        };
        let chain = tls
            .get_ref()
            .1
            .peer_certificates()
            .map(|certs| certs.iter().map(|c| c.as_ref().to_vec()).collect());
        let _ = seen.send(chain);
        let mut buf = [0u8; 64];
        loop {
            let n = match tls.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if tls.write_all(&buf[..n]).await.is_err() || tls.flush().await.is_err() {
                break;
            }
        }
    });
}

/// A TLS connection to a server using `source`'s current configuration that trusts the CAs
/// in `trust_pem` and presents `identity` if given. With TLS 1.3 a refused client certificate
/// shows at the first read, not here.
pub async fn connect(
    source: &TlsSource,
    trust_pem: &str,
    identity: Option<&Leaf>,
) -> io::Result<Conn> {
    let mut roots = RootCertStore::empty();
    for der in CertificateDer::pem_slice_iter(trust_pem.as_bytes()) {
        roots.add(der.expect("CA PEM")).expect("add root");
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_root_certificates(roots);
    let config = match identity {
        Some(identity) => builder
            .with_client_auth_cert(
                CertificateDer::pem_slice_iter(identity.cert.as_bytes())
                    .collect::<Result<_, _>>()
                    .expect("client chain"),
                PrivateKeyDer::from_pem_slice(identity.key.as_bytes()).expect("client key"),
            )
            .expect("client identity"),
        None => builder.with_no_client_auth(),
    };
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (send, seen) = oneshot::channel();
    serve(source, server, send);
    let name = ServerName::try_from("localhost").expect("server name");
    let tls = TlsConnector::from(Arc::new(config))
        .connect(name, client)
        .await?;
    Ok(Conn { tls, seen })
}

/// One round trip: whether the server echoed.
pub async fn ping(conn: &mut Conn) -> io::Result<()> {
    conn.tls.write_all(b"ping").await?;
    conn.tls.flush().await?;
    let mut back = [0u8; 4];
    conn.tls.read_exact(&mut back).await?;
    assert_eq!(&back, b"ping");
    Ok(())
}

/// The DER of the leaf the server presented on `conn`.
pub fn served(conn: &Conn) -> Vec<u8> {
    conn.tls
        .get_ref()
        .1
        .peer_certificates()
        .expect("the server presented a certificate")[0]
        .as_ref()
        .to_vec()
}
