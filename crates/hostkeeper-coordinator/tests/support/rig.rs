//! A coordinator built the way `config::build` builds one (the rollout file, the legacy service
//! and the certificate namespaces of the flags, the state file and the ops file of a directory),
//! driven through `http::handle` on a clock the test sets. The verifier is the testkit's static
//! one, so CI's token needs no network.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::http::{Peer, Surface, handle};
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::Coordinator;
use hostkeeper_coordinator::services::Services;
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock as oidc_clock, config_text, jwks_of, keys,
};
use hostkeeper_oidc::{AllowList, Verifier};
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::Request;

use super::{Flags, OPERATOR, ci_token, clock_of, recorded, scratch, updater_der};

/// How long a body has in the tests that are not about patience.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// What a coordinator is built from besides its directory.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The text of the rollout file.
    pub services: String,
    /// The services the allow-list entry of CI's token names, if it names any.
    pub oidc_services: Option<Vec<String>>,
    /// The flags: the legacy service and the certificate namespaces.
    pub flags: Flags,
}

impl Spec {
    /// kbf's definition and tierstore's (the recorded `services.toml`) under the recorded flags.
    pub fn recorded() -> Self {
        Self {
            services: recorded("services.toml"),
            oidc_services: None,
            flags: Flags::recorded(),
        }
    }

    /// The same with the rollout file `text`.
    pub fn with_services(mut self, text: &str) -> Self {
        self.services = text.to_owned();
        self
    }

    /// The same with an allow-list entry that names `names`.
    pub fn with_oidc_services(mut self, names: &[&str]) -> Self {
        self.oidc_services = Some(names.iter().map(|n| (*n).to_owned()).collect());
        self
    }

    /// The same without a legacy service.
    pub fn without_legacy(mut self) -> Self {
        self.flags.legacy_service = None;
        self
    }

    /// The same with the legacy service `name`.
    pub fn with_legacy(mut self, name: &str) -> Self {
        self.flags.legacy_service = Some(name.to_owned());
        self
    }

    /// The same with the certificate namespaces `role` and `host_role`.
    pub fn with_prefixes(mut self, role: &str, host_role: &str) -> Self {
        self.flags.role_uri_prefix = role.to_owned();
        self.flags.host_role_uri_prefix = host_role.to_owned();
        self
    }

    /// The text of the allow-list of CI's token.
    pub fn allow_list(&self) -> String {
        let mut text = config_text(STATIC_ISSUER);
        if let Some(names) = &self.oidc_services {
            let quoted: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
            text.push_str(&format!("services = [{}]\n", quoted.join(", ")));
        }
        text
    }

    /// A coordinator that starts now, by `clock`, over the state file and the ops file in `dir`.
    pub fn coordinator(&self, dir: &Scratch, clock: &Arc<AtomicU64>) -> Coordinator {
        let legacy = self.flags.legacy_service.clone();
        let store =
            DesiredStore::open(&dir.path().join("desired.json"), legacy.as_deref()).expect("store");
        let ops = OpsStore::open(&dir.path().join("desired.json.ops")).expect("ops");
        let (oidc_now, _) = oidc_clock();
        let verifier = Verifier::new(
            AllowList::parse(&self.allow_list()).expect("an allow-list"),
            Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
            Duration::from_secs(30),
            oidc_now,
        );
        Coordinator::new(
            store,
            Arc::new(verifier),
            OPERATOR.to_owned(),
            clock_of(clock),
        )
        .with_services(Services::parse(&self.services).expect("services"))
        .with_legacy_service(legacy)
        .with_prefixes(self.flags.prefixes())
        .with_ops(ops)
    }
}

/// A request.
pub fn request(method: &str, path: &str, bearer: Option<&str>, body: &str) -> Request<Full<Bytes>> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request")
}

/// A running coordinator and the directory of its files.
pub struct Rig {
    pub spec: Spec,
    pub dir: Scratch,
    pub clock: Arc<AtomicU64>,
    pub coordinator: Coordinator,
}

/// The time every rig's coordinator starts at.
pub const STARTED_AT: u64 = 1_000;

impl Rig {
    pub fn new(name: &str, spec: Spec) -> Self {
        let dir = scratch(name);
        let clock = Arc::new(AtomicU64::new(STARTED_AT));
        let coordinator = spec.coordinator(&dir, &clock);
        Self {
            spec,
            dir,
            clock,
            coordinator,
        }
    }

    /// The rig of the recorded services and flags.
    pub fn recorded(name: &str) -> Self {
        Self::new(name, Spec::recorded())
    }

    /// A coordinator over the same files that starts now: what a restart makes.
    pub fn restart(&mut self) {
        self.coordinator = self.spec.coordinator(&self.dir, &self.clock);
    }

    pub fn at(&self, seconds: u64) {
        self.clock.store(seconds, Ordering::SeqCst);
    }

    /// The answer to `request` on `surface` from `peer`: the status and the body bytes.
    pub async fn call(
        &self,
        surface: Surface,
        peer: &Peer,
        request: Request<Full<Bytes>>,
    ) -> (u16, String) {
        let response = handle(&self.coordinator, surface, peer, request, PATIENCE).await;
        let code = response.status().as_u16();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("a body")
            .to_bytes();
        (code, String::from_utf8(bytes.to_vec()).expect("text"))
    }

    /// The peer of a connection presenting a certificate in the rig's namespaces.
    pub fn updater(&self, host: &str, role: &str) -> Peer {
        Some(Arc::from(
            updater_der(host, role, &self.spec.flags.prefixes()).as_slice(),
        ))
    }

    /// A heartbeat of the updater `host` on a host of role `role`.
    pub async fn heartbeat(&self, host: &str, role: &str, body: &str) -> (u16, String) {
        let peer = self.updater(host, role);
        let request = request("POST", "/v1/heartbeat", None, body);
        self.call(Surface::Heartbeat, &peer, request).await
    }

    /// A call on the control surface with `bearer`.
    pub async fn control(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        self.call(Surface::Control, &None, request(method, path, bearer, body))
            .await
    }

    /// `POST /v1/desired` with a token CI would send.
    pub async fn desired(&self, body: &str) -> (u16, String) {
        self.control("POST", "/v1/desired", Some(&ci_token()), body)
            .await
    }

    /// `POST /v1/operator` with the operator token.
    pub async fn operator(&self, body: &str) -> (u16, String) {
        self.control("POST", "/v1/operator", Some(OPERATOR), body)
            .await
    }

    /// `GET /v1/status` with the operator token, as JSON.
    pub async fn status(&self) -> serde_json::Value {
        let (code, body) = self.control("GET", "/v1/status", Some(OPERATOR), "").await;
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).expect("JSON")
    }

    /// `GET /v1/convergence` with a token CI would send; `query` is what follows the `?`.
    pub async fn convergence(&self, query: Option<&str>) -> (u16, String) {
        let path = match query {
            Some(query) => format!("/v1/convergence?{query}"),
            None => "/v1/convergence".to_owned(),
        };
        self.control("GET", &path, Some(&ci_token()), "").await
    }
}
