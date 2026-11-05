//! A fake GitHub Actions issuer for tests: RSA keys from fixtures, a TLS server (over
//! in-process streams) that serves a discovery document and a JWKS, and a token minter. Nothing is shared with
//! the code under test but its public interface.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures::FutureExt as _;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio_rustls::TlsAcceptor;

use super::{Clock, Dial, HttpsKeys, OidcConfig, Roots, SpentJtis, Stream, Verifier};

/// The fixed "now" of the tests, in unix seconds.
pub const NOW: u64 = 1_800_000_000;

pub const AUDIENCE: &str = "hostkeeper-test";
pub const WORKFLOW: &str =
    "example-org/example-repo/.github/workflows/ci-check.yml@refs/heads/main";

/// An RSA signing key and the JWK that publishes it.
pub struct TestKey {
    pub kid: String,
    pair: RsaKeyPair,
    n: Vec<u8>,
    e: Vec<u8>,
}

/// A big-endian integer without leading zero bytes, as a JWK writes it.
fn strip(mut bytes: Vec<u8>) -> Vec<u8> {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    bytes.drain(..zeros);
    bytes
}

impl TestKey {
    fn load(kid: &str, base64_der: &str) -> Self {
        let der = base64::engine::general_purpose::STANDARD
            .decode(base64_der.split_whitespace().collect::<String>())
            .expect("base64");
        let pair = RsaKeyPair::from_pkcs8(&der).expect("ring reads the key");
        let public = ring::signature::RsaPublicKeyComponents::<Vec<u8>>::from(pair.public());
        Self {
            kid: kid.to_owned(),
            n: strip(public.n),
            e: strip(public.e),
            pair,
        }
    }

    pub fn jwk(&self) -> Value {
        json!({"kty": "RSA", "kid": self.kid, "use": "sig", "alg": "RS256",
               "n": B64.encode(&self.n), "e": B64.encode(&self.e)})
    }

    pub fn n(&self) -> &[u8] {
        &self.n
    }

    fn sign(&self, input: &str) -> Vec<u8> {
        let mut out = vec![0; self.pair.public().modulus_len()];
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut out,
            )
            .expect("a signature");
        out
    }
}

/// Three keys: the issuer's, a second one for a forged signature, and one for a rotation.
/// They are fixtures (`testdata/k1.pk8.b64` and so on, base64 of 2048-bit PKCS#8 DER made once with
/// `openssl genpkey`), used by no deployment: a test that signs with them proves the verifier,
/// nothing else. Fixed keys also keep a test process from spending seconds on prime search.
pub fn keys() -> &'static [TestKey; 3] {
    static KEYS: OnceLock<[TestKey; 3]> = OnceLock::new();
    KEYS.get_or_init(|| {
        [
            TestKey::load("k1", include_str!("../testdata/k1.pk8.b64")),
            TestKey::load("k2", include_str!("../testdata/k2.pk8.b64")),
            TestKey::load("k3", include_str!("../testdata/k3.pk8.b64")),
        ]
    })
}

pub fn jwks_of(keys: &[&TestKey]) -> String {
    json!({"keys": keys.iter().map(|k| k.jwk()).collect::<Vec<_>>()}).to_string()
}

/// What the fake issuer serves and what it saw.
pub struct Served {
    pub jwks: String,
    /// Answer every request 503.
    pub fail: bool,
    /// The `jwks_uri` the discovery document names, if not the issuer's own.
    pub jwks_uri: Option<String>,
    /// How many times the JWKS was requested.
    pub jwks_requests: usize,
    /// The discovery document, if not the issuer's own.
    pub discovery: Option<String>,
    /// Read the request and never answer it.
    pub hang: bool,
}

/// The default name of a [`FakeIssuer`]: its URL is `https://issuer.test`.
pub const ISSUER_HOST: &str = "issuer.test";

/// A TLS server acting as an issuer, reached through [`FakeIssuer::dial`] over in-process
/// streams: no socket is opened, and the name in its URL is dialled by nothing else.
pub struct FakeIssuer {
    pub url: String,
    /// The certificate the issuer serves, which is the one root that trusts it.
    pub root: rustls::pki_types::CertificateDer<'static>,
    pub roots: Roots,
    pub served: Arc<Mutex<Served>>,
    acceptor: TlsAcceptor,
}

/// One connection of the fake issuer: the TLS handshake, one request, one answer.
async fn serve(
    acceptor: TlsAcceptor,
    state: Arc<Mutex<Served>>,
    issuer: String,
    stream: DuplexStream,
) {
    let Ok(mut tls) = acceptor.accept(stream).await else {
        return;
    };
    let mut head: Vec<u8> = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match tls.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    if state.lock().expect("state").hang {
        std::future::pending::<()>().await;
    }
    let head = String::from_utf8_lossy(&head);
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let (status, body) = {
        let mut s = state.lock().expect("state");
        if s.fail {
            ("503 Service Unavailable", String::new())
        } else if path == "/.well-known/openid-configuration" && s.discovery.is_some() {
            ("200 OK", s.discovery.clone().unwrap_or_default())
        } else if path == "/.well-known/openid-configuration" {
            let uri = s
                .jwks_uri
                .clone()
                .unwrap_or_else(|| format!("{issuer}/.well-known/jwks"));
            (
                "200 OK",
                json!({"issuer": issuer, "jwks_uri": uri}).to_string(),
            )
        } else if path == "/.well-known/jwks" {
            s.jwks_requests += 1;
            ("200 OK", s.jwks.clone())
        } else {
            ("404 Not Found", String::new())
        }
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tls.write_all(response.as_bytes()).await;
    let _ = tls.shutdown().await;
}

impl FakeIssuer {
    /// An issuer named [`ISSUER_HOST`] serving `jwks`.
    pub async fn start(jwks: String) -> Self {
        Self::named(ISSUER_HOST, jwks).await
    }

    /// An issuer at `https://<host>` serving `jwks`; `host` is a name under `.test` or
    /// `.example`. Its certificate is its own, so two issuers do not trust one another.
    pub async fn named(host: &str, jwks: String) -> Self {
        let certified =
            rcgen::generate_simple_self_signed(vec![host.to_owned()]).expect("a certificate");
        let der = certified.cert.der().clone();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der()),
        );
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_no_client_auth()
            .with_single_cert(vec![der.clone()], key)
            .expect("a server config");
        let served = Arc::new(Mutex::new(Served {
            jwks,
            fail: false,
            jwks_uri: None,
            jwks_requests: 0,
            discovery: None,
            hang: false,
        }));
        Self {
            url: format!("https://{host}"),
            root: der.clone(),
            roots: Roots::Given(vec![der]),
            served,
            acceptor: TlsAcceptor::from(Arc::new(config)),
        }
    }

    /// The issuer's URL, which is the `iss` of its tokens.
    pub fn issuer(&self) -> &str {
        &self.url
    }

    /// A dial that answers every connection with this issuer, whatever host and port it is
    /// asked for: the server end of an in-process stream is served at once.
    pub fn dial(&self) -> Dial {
        let (acceptor, state, issuer) = (
            self.acceptor.clone(),
            Arc::clone(&self.served),
            self.url.clone(),
        );
        Arc::new(move |_host: String, _port: u16| {
            let (acceptor, state, issuer) = (acceptor.clone(), Arc::clone(&state), issuer.clone());
            async move {
                let (client, server) = tokio::io::duplex(64 * 1024);
                tokio::spawn(serve(acceptor, state, issuer, server));
                Ok::<Stream, std::io::Error>(Box::new(client))
            }
            .boxed()
        })
    }

    /// A key source for this issuer: it trusts [`Self::roots`] and dials [`Self::dial`].
    pub fn source(&self, timeout: std::time::Duration) -> HttpsKeys {
        HttpsKeys::new(&self.url, self.roots.clone(), timeout)
            .expect("a source")
            .with_dial(self.dial())
    }

    pub fn set_jwks(&self, jwks: String) {
        self.served.lock().expect("state").jwks = jwks;
    }

    pub fn set_discovery(&self, document: &str) {
        self.served.lock().expect("state").discovery = Some(document.to_owned());
    }

    pub fn set_hang(&self) {
        self.served.lock().expect("state").hang = true;
    }

    pub fn set_fail(&self, fail: bool) {
        self.served.lock().expect("state").fail = fail;
    }

    pub fn jwks_requests(&self) -> usize {
        self.served.lock().expect("state").jwks_requests
    }
}

/// The allow-list text for `issuer`.
pub fn config_text(issuer: &str) -> String {
    format!(
        r#"
[remote_operator]
issuer = "{issuer}"
audience = "{AUDIENCE}"
repository_id = "4242"
repository = "example-org/example-repo"
job_workflow_ref = ["{WORKFLOW}"]
refs = ["refs/heads/main"]
environments = ["canary"]
"#
    )
}

/// A settable clock.
pub fn clock() -> (Clock, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(NOW));
    let shared = Arc::clone(&now);
    (Arc::new(move || shared.load(Ordering::SeqCst)), now)
}

/// Spent token ids kept in memory: a test's stand-in for the caller's file. Two verifiers given
/// the same one stand for a caller before and after a restart. While [`Self::fail`] is set,
/// recording fails.
#[derive(Default)]
pub struct SpentInMemory {
    entries: Mutex<Vec<(String, u64)>>,
    failing: std::sync::atomic::AtomicBool,
}

impl SpentInMemory {
    /// One holding `entries` already.
    #[must_use]
    pub fn holding(entries: Vec<(String, u64)>) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(entries),
            failing: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Makes recording fail (`true`) or work again (`false`).
    pub fn fail(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// Every id recorded, in order, with its expiry.
    #[must_use]
    pub fn entries(&self) -> Vec<(String, u64)> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl SpentJtis for SpentInMemory {
    fn load(&self) -> Vec<(String, u64)> {
        self.entries()
    }

    fn record(&self, jti: &str, until: u64) -> std::io::Result<()> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("recording is made to fail"));
        }
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((jti.to_owned(), until));
        Ok(())
    }
}

/// An empty [`SpentInMemory`].
#[must_use]
pub fn spent() -> Arc<SpentInMemory> {
    Arc::new(SpentInMemory::default())
}

/// A verifier over `issuer`, with a 30 s refetch interval and a clock the test sets.
pub fn verifier(issuer: &FakeIssuer) -> (Arc<Verifier>, Arc<AtomicU64>) {
    let config = OidcConfig::parse(&config_text(&issuer.url)).expect("a config");
    let source = issuer.source(std::time::Duration::from_secs(5));
    let (clock, now) = clock();
    (
        Arc::new(Verifier::new(
            config,
            Arc::new(source),
            std::time::Duration::from_secs(30),
            clock,
            spent(),
        )),
        now,
    )
}

/// The claims of a token CI would send, valid at [`NOW`] for the issuer at `issuer`.
pub fn good_claims(issuer: &str) -> Value {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    json!({
        "iss": issuer, "aud": AUDIENCE, "sub": "repo:example-org/example-repo:environment:canary",
        "exp": NOW + 300, "iat": NOW, "nbf": NOW,
        "jti": format!("jti-{}", SERIAL.fetch_add(1, Ordering::SeqCst)),
        "repository_id": "4242", "repository": "example-org/example-repo",
        "job_workflow_ref": WORKFLOW, "ref": "refs/heads/main", "environment": "canary",
        "event_name": "push", "runner_environment": "github-hosted",
        "sha": "0123456789abcdef0123456789abcdef01234567",
    })
}

/// `claims` signed by `key` under its own `kid`.
pub fn mint(key: &TestKey, claims: &Value) -> String {
    mint_with(
        key,
        &json!({"alg": "RS256", "typ": "JWT", "kid": key.kid}),
        claims,
    )
}

/// `claims` under `header`, signed by `key` as RS256 whatever the header says.
pub fn mint_with(key: &TestKey, header: &Value, claims: &Value) -> String {
    let input = format!(
        "{}.{}",
        B64.encode(header.to_string()),
        B64.encode(claims.to_string())
    );
    let signature = key.sign(&input);
    format!("{input}.{}", B64.encode(signature))
}

/// A token whose header and signature are whatever the caller makes them.
pub fn mint_raw(header: &Value, claims: &Value, signature: &[u8]) -> String {
    format!(
        "{}.{}.{}",
        B64.encode(header.to_string()),
        B64.encode(claims.to_string()),
        B64.encode(signature)
    )
}

/// The issuer of [`static_verifier`]: nothing listens there.
pub const STATIC_ISSUER: &str = "https://token.example";

/// A key source that holds one JWKS and opens no socket: for the tests of a caller that must
/// not depend on a network (a paused clock, no ports).
pub struct StaticKeys(pub String);

impl super::KeySource for StaticKeys {
    fn jwks(&self) -> futures::future::BoxFuture<'_, Result<String, super::FetchError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

/// A verifier for [`STATIC_ISSUER`] that knows `keys()[0]` (`k1`) and no other, and a clock
/// the test sets (it starts at [`NOW`]). Mint with [`mint`] over [`good_claims`]`(STATIC_ISSUER)`.
pub fn static_verifier() -> (Arc<Verifier>, Arc<AtomicU64>) {
    let config = OidcConfig::parse(&config_text(STATIC_ISSUER)).expect("a config");
    let source = StaticKeys(jwks_of(&[&keys()[0]]));
    let (clock, now) = clock();
    (
        Arc::new(Verifier::new(
            config,
            Arc::new(source),
            std::time::Duration::from_secs(30),
            clock,
            spent(),
        )),
        now,
    )
}
