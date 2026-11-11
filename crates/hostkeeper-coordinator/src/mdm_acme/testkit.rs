//! The fakes of the tests of the certificate for the MDM name (T12: no network, no real DNS, no
//! sleep): a clock that moves when told to or when something sleeps on it, a DNS provider that
//! keeps records in memory, and an ACME CA that answers the protocol over the [`Https`] port and
//! issues real certificates for the key of the CSR it is sent, so a renewed certificate can be
//! served by the listener's resolver in a test.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hostkeeper_mdm_push::BoxFuture;
use rcgen::{CertificateParams, CertificateSigningRequestParams, IsCa, Issuer, KeyPair};
use rustls::pki_types::CertificateSigningRequestDer;
use serde_json::{Value, json};

use super::Clock;
use super::dns::{CredentialError, DnsCredential, DnsError, DnsProvider, DnsProviders, TxtRecord};
use super::https::{HttpFailure, HttpReply, HttpRequest, Https};

/// A clock whose sleep moves it.
pub(crate) struct FakeClock {
    now: AtomicU64,
    slept: Mutex<Vec<Duration>>,
    limit: AtomicU64,
}

impl FakeClock {
    pub(crate) fn at(now: u64) -> Arc<Self> {
        Arc::new(Self {
            now: AtomicU64::new(now),
            slept: Mutex::new(Vec::new()),
            limit: AtomicU64::new(u64::MAX),
        })
    }

    /// After `sleeps` sleeps, the next one never returns.
    pub(crate) fn freeze_after(&self, sleeps: u64) {
        self.limit.store(sleeps, Ordering::SeqCst);
    }

    pub(crate) fn advance(&self, seconds: u64) {
        self.now.fetch_add(seconds, Ordering::SeqCst);
    }

    pub(crate) fn slept(&self) -> Vec<Duration> {
        self.slept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Clock for FakeClock {
    fn now_unix(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn sleep(&self, pause: Duration) -> BoxFuture<'_, ()> {
        let mut slept = self.slept.lock().unwrap_or_else(PoisonError::into_inner);
        if u64::try_from(slept.len()).unwrap_or(u64::MAX) >= self.limit.load(Ordering::SeqCst) {
            return Box::pin(std::future::pending());
        }
        slept.push(pause);
        drop(slept);
        self.now.fetch_add(pause.as_secs(), Ordering::SeqCst);
        Box::pin(std::future::ready(()))
    }
}

/// A DNS provider in memory.
#[derive(Default)]
pub(crate) struct FakeDns {
    inner: Mutex<DnsInner>,
}

#[derive(Default)]
struct DnsInner {
    records: BTreeMap<String, Vec<String>>,
    calls: Vec<String>,
    polls_until_visible: u32,
    reject: Option<u16>,
    fail_remove: bool,
    fail_publish: bool,
}

impl FakeDns {
    pub(crate) fn new() -> Arc<Self> {
        Arc::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DnsInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The record shows only after `polls` looks.
    pub(crate) fn show_after(&self, polls: u32) {
        self.lock().polls_until_visible = polls;
    }

    /// Every call answers as the provider refusing the credential with `status`.
    pub(crate) fn reject_with(&self, status: u16) {
        self.lock().reject = Some(status);
    }

    /// Calls are accepted again.
    pub(crate) fn accept(&self) {
        self.lock().reject = None;
    }

    pub(crate) fn fail_publish(&self) {
        self.lock().fail_publish = true;
    }

    pub(crate) fn fail_remove(&self) {
        self.lock().fail_remove = true;
    }

    pub(crate) fn values(&self, name: &str) -> Vec<String> {
        self.lock().records.get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    fn refused(inner: &DnsInner) -> Result<(), DnsError> {
        match inner.reject {
            Some(status) => Err(DnsError::Rejected { status }),
            None => Ok(()),
        }
    }
}

impl DnsProvider for FakeDns {
    fn publish<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<(), DnsError>> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner
                .calls
                .push(format!("publish {} ttl {}", record.name, record.ttl_s));
            Self::refused(&inner)?;
            if inner.fail_publish {
                return Err(DnsError::Failed("the provider is down".to_owned()));
            }
            inner
                .records
                .entry(record.name.clone())
                .or_default()
                .push(record.value.clone());
            Ok(())
        })
    }

    fn is_published<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<bool, DnsError>> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.calls.push(format!("look {}", record.name));
            Self::refused(&inner)?;
            if inner.polls_until_visible > 0 {
                inner.polls_until_visible -= 1;
                return Ok(false);
            }
            Ok(inner
                .records
                .get(&record.name)
                .is_some_and(|values| values.contains(&record.value)))
        })
    }

    fn remove<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<(), DnsError>> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.calls.push(format!("remove {}", record.name));
            if inner.fail_remove {
                return Err(DnsError::Failed("the provider is down".to_owned()));
            }
            if let Some(values) = inner.records.get_mut(&record.name) {
                values.retain(|value| *value != record.value);
            }
            Ok(())
        })
    }
}

/// Opens the one fake for a `godaddy` credential.
pub(crate) struct FakeProviders(pub(crate) Arc<FakeDns>);

impl DnsProviders for FakeProviders {
    fn open(&self, credential: &DnsCredential) -> Result<Arc<dyn DnsProvider>, CredentialError> {
        if credential.provider != "godaddy" {
            return Err(CredentialError::UnknownProvider(
                credential.provider.clone(),
            ));
        }
        Ok(Arc::clone(&self.0) as Arc<dyn DnsProvider>)
    }
}

/// The directory of the fake CA.
pub(crate) const DIRECTORY: &str = "https://acme.test/directory";

/// What the fake CA is told to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mood {
    /// Issue.
    #[default]
    Issues,
    /// The CA cannot be reached.
    Down,
    /// `newOrder` answers a problem.
    RefusesOrder,
    /// The authorization is already valid (a recent proof stands).
    AuthorizationValid,
    /// The authorization is invalid.
    AuthorizationInvalid,
    /// The authorization offers no dns-01 challenge.
    NoDnsChallenge,
    /// The order never leaves pending.
    NeverReady,
    /// The order becomes invalid after the challenge.
    OrderInvalid,
    /// The order stays processing after finalize.
    NeverIssues,
    /// The certificate URL answers text that is not a certificate.
    GarbageChain,
    /// The order carries an error once the challenge is answered.
    OrderErrors,
}

/// An ACME CA over the [`Https`] port. It does not check signatures; it checks that the DNS
/// fake holds a TXT value at the challenge name when it is told the challenge is ready.
pub(crate) struct FakeAcme {
    dns: Arc<FakeDns>,
    inner: Mutex<AcmeInner>,
    issuer: (Issuer<'static, KeyPair>, String, Vec<u8>),
}

#[derive(Default)]
struct AcmeInner {
    mood: Mood,
    nonce: u64,
    accounts: u32,
    orders: u32,
    requests: Vec<String>,
    payloads: Vec<(String, String)>,
    challenge_values: Vec<Vec<String>>,
    validity_s: u64,
    now: u64,
    start: u64,
    challenged: bool,
    finalized: bool,
    chain: String,
    dns_name: String,
    txt_name: String,
}

impl FakeAcme {
    /// A CA for `name` that looks for the challenge at `txt_name` in `dns` and issues
    /// certificates valid for `validity_s` from `now`.
    pub(crate) fn new(
        dns: &Arc<FakeDns>,
        name: &str,
        txt_name: &str,
        now: u64,
        validity_s: u64,
    ) -> Arc<Self> {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = KeyPair::generate().expect("a CA key");
        let cert = params.self_signed(&key).expect("a CA certificate");
        let pem = cert.pem();
        let der = cert.der().to_vec();
        Arc::new(Self {
            dns: Arc::clone(dns),
            inner: Mutex::new(AcmeInner {
                validity_s,
                now,
                start: now,
                dns_name: name.to_owned(),
                txt_name: txt_name.to_owned(),
                ..AcmeInner::default()
            }),
            issuer: (Issuer::new(params, key), pem, der),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AcmeInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Certificates issued from now on expire `validity_s` after `now`.
    pub(crate) fn set_now(&self, now: u64) {
        self.lock().now = now;
    }

    pub(crate) fn be(&self, mood: Mood) {
        self.lock().mood = mood;
    }

    /// Every request as `METHOD path`.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.lock().requests.clone()
    }

    /// The TXT values the DNS fake held at the name when each challenge was answered.
    pub(crate) fn challenge_values(&self) -> Vec<Vec<String>> {
        self.lock().challenge_values.clone()
    }

    /// The decoded payload of each POST to `path`.
    pub(crate) fn payloads(&self, path: &str) -> Vec<String> {
        self.lock()
            .payloads
            .iter()
            .filter(|(at, _)| at == path)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    /// The CA's own certificate, DER.
    pub(crate) fn root_der(&self) -> Vec<u8> {
        self.issuer.2.clone()
    }

    /// The accounts created.
    pub(crate) fn accounts(&self) -> u32 {
        self.lock().accounts
    }

    /// The orders made.
    pub(crate) fn orders(&self) -> u32 {
        self.lock().orders
    }

    fn reply(
        inner: &mut AcmeInner,
        status: u16,
        extra: &[(&str, String)],
        body: &str,
    ) -> HttpReply {
        inner.nonce += 1;
        let mut headers = vec![("replay-nonce".to_owned(), format!("nonce-{}", inner.nonce))];
        headers.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        HttpReply {
            status,
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    fn order_json(inner: &AcmeInner) -> Value {
        let (status, certificate) = if inner.finalized {
            if inner.mood == Mood::NeverIssues {
                ("processing", None)
            } else {
                ("valid", Some("https://acme.test/cert/1"))
            }
        } else if inner.challenged {
            match inner.mood {
                Mood::OrderInvalid => ("invalid", None),
                Mood::NeverReady => ("pending", None),
                _ => ("ready", None),
            }
        } else if matches!(inner.mood, Mood::AuthorizationValid) {
            ("ready", None)
        } else {
            ("pending", None)
        };
        let mut order = json!({
            "status": status,
            "identifiers": [{"type": "dns", "value": inner.dns_name}],
            "authorizations": ["https://acme.test/authz/1"],
            "finalize": "https://acme.test/finalize/1",
        });
        if let Some(url) = certificate {
            order["certificate"] = json!(url);
        }
        if inner.challenged && inner.mood == Mood::OrderErrors {
            order["error"] = json!({"type": "urn:ietf:params:acme:error:serverInternal", "detail": "the CA broke", "status": 500});
        }
        order
    }

    fn authorization_json(inner: &AcmeInner) -> Value {
        let status = match inner.mood {
            Mood::AuthorizationValid => "valid",
            Mood::AuthorizationInvalid => "invalid",
            _ => "pending",
        };
        let challenges = if inner.mood == Mood::NoDnsChallenge {
            json!([{"type": "http-01", "url": "https://acme.test/chall/2", "token": "tok2", "status": "pending"}])
        } else {
            json!([
                {"type": "http-01", "url": "https://acme.test/chall/2", "token": "tok2", "status": "pending"},
                {"type": "dns-01", "url": "https://acme.test/chall/1", "token": "tok1", "status": "pending"}
            ])
        };
        json!({
            "status": status,
            "identifier": {"type": "dns", "value": inner.dns_name},
            "challenges": challenges,
        })
    }

    fn issue(&self, inner: &mut AcmeInner, csr_b64: &str) {
        let der = URL_SAFE_NO_PAD.decode(csr_b64).expect("a base64url CSR");
        let mut csr =
            CertificateSigningRequestParams::from_der(&CertificateSigningRequestDer::from(der))
                .expect("a CSR");
        // Valid from when the CA was made, so a certificate issued "later" in a test is valid
        // now for a real client; it expires `validity_s` after the CA's notion of now.
        csr.params.not_before =
            time::OffsetDateTime::from_unix_timestamp(i64::try_from(inner.start).expect("a time"))
                .expect("a time");
        csr.params.not_after = time::OffsetDateTime::from_unix_timestamp(
            i64::try_from(inner.now + inner.validity_s).expect("a time"),
        )
        .expect("a time");
        let leaf = csr.signed_by(&self.issuer.0).expect("a certificate");
        inner.chain = format!("{}{}", leaf.pem(), self.issuer.1);
    }

    fn answer(&self, request: &HttpRequest) -> Result<HttpReply, HttpFailure> {
        let mut inner = self.lock();
        let path = request
            .url
            .strip_prefix("https://acme.test")
            .unwrap_or(&request.url)
            .split('?')
            .next()
            .unwrap_or_default()
            .to_owned();
        inner.requests.push(format!("{} {path}", request.method));
        if inner.mood == Mood::Down {
            return Err(HttpFailure("the CA cannot be reached".to_owned()));
        }
        let payload: String = serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|jws| jws.get("payload")?.as_str().map(str::to_owned))
            .and_then(|b64| URL_SAFE_NO_PAD.decode(b64).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();
        let inner = &mut *inner;
        inner.payloads.push((path.clone(), payload.clone()));
        Ok(match (request.method.as_str(), path.as_str()) {
            ("GET", "/directory") => Self::reply(
                inner,
                200,
                &[],
                &json!({
                    "newNonce": "https://acme.test/nonce",
                    "newAccount": "https://acme.test/account",
                    "newOrder": "https://acme.test/order",
                    "revokeCert": "https://acme.test/revoke",
                    "keyChange": "https://acme.test/key",
                })
                .to_string(),
            ),
            ("HEAD", "/nonce") => Self::reply(inner, 200, &[], ""),
            ("POST", "/account") => {
                inner.accounts += 1;
                Self::reply(
                    inner,
                    201,
                    &[("location", format!("https://acme.test/acct/{}", inner.accounts))],
                    &json!({"status": "valid"}).to_string(),
                )
            }
            ("POST", "/order") if inner.mood == Mood::RefusesOrder => Self::reply(
                inner,
                429,
                &[("content-type", "application/problem+json".to_owned())],
                &json!({"type": "urn:ietf:params:acme:error:rateLimited", "detail": "too many orders", "status": 429}).to_string(),
            ),
            ("POST", "/order") => {
                inner.orders += 1;
                inner.challenged = false;
                inner.finalized = false;
                let body = Self::order_json(inner).to_string();
                Self::reply(inner, 201, &[("location", "https://acme.test/order/1".to_owned())], &body)
            }
            ("POST", "/authz/1") => {
                let body = Self::authorization_json(inner).to_string();
                Self::reply(inner, 200, &[], &body)
            }
            ("POST", "/chall/1") => {
                let values = self.dns.values(&inner.txt_name);
                inner.challenge_values.push(values);
                inner.challenged = true;
                Self::reply(inner, 200, &[], &json!({"type": "dns-01", "url": "https://acme.test/chall/1", "token": "tok1", "status": "processing"}).to_string())
            }
            ("POST", "/order/1") => {
                let body = Self::order_json(inner).to_string();
                Self::reply(inner, 200, &[("location", "https://acme.test/order/1".to_owned())], &body)
            }
            ("POST", "/finalize/1") => {
                let csr = serde_json::from_str::<Value>(&payload)
                    .ok()
                    .and_then(|v| v.get("csr")?.as_str().map(str::to_owned))
                    .unwrap_or_default();
                inner.finalized = true;
                if inner.mood != Mood::NeverIssues {
                    self.issue(inner, &csr);
                }
                let body = Self::order_json(inner).to_string();
                Self::reply(inner, 200, &[], &body)
            }
            ("POST", "/cert/1") => {
                let chain = if inner.mood == Mood::GarbageChain {
                    "not a certificate".to_owned()
                } else {
                    inner.chain.clone()
                };
                Self::reply(inner, 200, &[("content-type", "application/pem-certificate-chain".to_owned())], &chain)
            }
            _ => Self::reply(inner, 404, &[], ""),
        })
    }
}

impl Https for FakeAcme {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpReply, HttpFailure>> {
        Box::pin(async move { self.answer(&request) })
    }
}

/// GoDaddy's v1 TXT records API in memory: `GET`, `PUT` and `DELETE` of
/// `/v1/domains/{zone}/records/TXT/{name}`, where a `PUT` replaces every record at the name.
#[derive(Default)]
pub(crate) struct FakeGoDaddyApi {
    inner: Mutex<ApiInner>,
}

#[derive(Default)]
struct ApiInner {
    records: BTreeMap<String, Vec<Value>>,
    requests: Vec<HttpRequest>,
    status: Option<u16>,
    body: Option<String>,
    down: bool,
}

impl FakeGoDaddyApi {
    pub(crate) fn new() -> Arc<Self> {
        Arc::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ApiInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds `data` at `name` (a path as `zone/name`) with `ttl`.
    pub(crate) fn hold(&self, at: &str, data: &str, ttl: u32) {
        self.lock()
            .records
            .entry(at.to_owned())
            .or_default()
            .push(json!({"data": data, "ttl": ttl, "name": "x", "type": "TXT"}));
    }

    /// The data held at `at`.
    pub(crate) fn data(&self, at: &str) -> Vec<String> {
        self.lock()
            .records
            .get(at)
            .map(|list| {
                list.iter()
                    .filter_map(|entry| entry["data"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every request heard.
    pub(crate) fn requests(&self) -> Vec<HttpRequest> {
        self.lock().requests.clone()
    }

    /// `METHOD zone/name` of every request heard.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.lock()
            .requests
            .iter()
            .map(|request| {
                let at = request
                    .url
                    .split_once("/v1/domains/")
                    .map_or("", |(_, at)| at);
                format!("{} {}", request.method, at.replacen("/records/TXT", "", 1))
            })
            .collect()
    }

    /// Every call answers `status` and, when given, `body`.
    pub(crate) fn answer_with(&self, status: u16, body: Option<&str>) {
        let mut inner = self.lock();
        inner.status = Some(status);
        inner.body = body.map(str::to_owned);
    }

    pub(crate) fn go_down(&self) {
        self.lock().down = true;
    }
}

impl Https for FakeGoDaddyApi {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpReply, HttpFailure>> {
        Box::pin(async move {
            let mut inner = self.lock();
            inner.requests.push(request.clone());
            if inner.down {
                return Err(HttpFailure("the API cannot be reached".to_owned()));
            }
            let reply = |status: u16, body: String| HttpReply {
                status,
                headers: Vec::new(),
                body: body.into_bytes(),
            };
            if let Some(status) = inner.status {
                return Ok(reply(status, inner.body.clone().unwrap_or_default()));
            }
            let at = request
                .url
                .split_once("/v1/domains/")
                .map(|(_, at)| at.replacen("/records/TXT", "", 1))
                .unwrap_or_default();
            Ok(match request.method.as_str() {
                "GET" => reply(
                    200,
                    serde_json::to_string(&inner.records.get(&at).cloned().unwrap_or_default())
                        .unwrap_or_default(),
                ),
                "PUT" => {
                    let list: Vec<Value> =
                        serde_json::from_slice(&request.body).unwrap_or_default();
                    inner.records.insert(at, list);
                    reply(200, "{}".to_owned())
                }
                "DELETE" => {
                    inner.records.remove(&at);
                    reply(204, String::new())
                }
                _ => reply(405, String::new()),
            })
        })
    }
}

/// A TLS server's configuration for `name`, and the roots a client trusts to reach it.
pub(crate) fn tls_server(name: &str) -> (rustls::RootCertStore, Arc<rustls::ServerConfig>) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("params");
    ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().expect("a key");
    let ca = ca_params.self_signed(&ca_key).expect("a CA");
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().expect("a key");
    let leaf = CertificateParams::new(vec![name.to_owned()])
        .expect("params")
        .signed_by(&leaf_key, &issuer)
        .expect("a leaf");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).expect("a root");
    let key = rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der()).expect("a key");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_no_client_auth()
    .with_single_cert(vec![leaf.der().clone()], key)
    .expect("a server config");
    (roots, Arc::new(config))
}
