//! ABM, scripted, for the tests (AGENTS.md T12): a transport that answers each request by a
//! handler the test gives and keeps what it was asked, and [`Sim`], a handler that behaves as the
//! parts of ABM the client uses do (spike abm): a session that must be opened with a signature and
//! that the test can expire, a device list with cursors, the profile definition and the assignment.
//! No socket is opened and no clock is read.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use hyper::Method;
use serde_json::{Value, json};

use super::transport::{AbmRequest, AbmResponse, AbmTransport, TransportError};

/// What a handler answers.
pub(crate) type Reply = Result<AbmResponse, TransportError>;

/// A handler: the request asked, the answer.
pub(crate) type Handler = Box<dyn Fn(&AbmRequest) -> Reply + Send + Sync>;

/// A transport that answers by a handler and remembers every request.
pub(crate) struct FakeAbm {
    handler: Handler,
    seen: Mutex<Vec<AbmRequest>>,
}

impl FakeAbm {
    /// A transport answering by `handler`.
    pub(crate) fn new(handler: Handler) -> Arc<Self> {
        Arc::new(Self {
            handler,
            seen: Mutex::new(Vec::new()),
        })
    }

    /// Every request asked, in order.
    pub(crate) fn seen(&self) -> Vec<AbmRequest> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The `METHOD path` of every request asked, in order.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.seen()
            .iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect()
    }
}

impl AbmTransport for FakeAbm {
    fn send(&self, request: AbmRequest) -> BoxFuture<'_, Reply> {
        let answer = (self.handler)(&request);
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        Box::pin(async move { answer })
    }
}

/// A JSON answer.
pub(crate) fn json_reply(status: u16, body: &Value) -> Reply {
    Ok(AbmResponse {
        status,
        retry_after: None,
        body: serde_json::to_vec(body).unwrap_or_default(),
    })
}

/// A plain-text answer.
pub(crate) fn text_reply(status: u16, body: &str) -> Reply {
    Ok(AbmResponse {
        status,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    })
}

/// The header `name` of `request`.
pub(crate) fn header<'a>(request: &'a AbmRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(have, _)| have.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The body of `request` as JSON.
pub(crate) fn body_of(request: &AbmRequest) -> Value {
    serde_json::from_slice(request.body.as_deref().unwrap_or(b"null")).unwrap_or(Value::Null)
}

/// One device of the simulated ABM.
#[derive(Clone, Debug)]
pub(crate) struct SimDevice {
    pub(crate) serial: String,
    pub(crate) profile_status: String,
    pub(crate) profile_uuid: Option<String>,
}

/// The parts of ABM the sync talks to.
#[derive(Default)]
pub(crate) struct SimState {
    /// The session ABM holds as good; none when none was opened or the test expired it.
    pub(crate) session: Option<String>,
    /// How many sessions were opened.
    pub(crate) sessions: u32,
    /// The devices ABM lists.
    pub(crate) devices: Vec<SimDevice>,
    /// The changes since the cursor `c<n>`: serial and `op_type`, by `n`.
    pub(crate) log: Vec<(String, String)>,
    /// Cursors that ABM no longer takes.
    pub(crate) dead_cursors: Vec<String>,
    /// The page size the simulator cuts pages at (0: one page).
    pub(crate) page: usize,
    /// The profiles defined, by uuid.
    pub(crate) profiles: BTreeMap<String, Value>,
    /// The serials ABM answers `NOT_ACCESSIBLE` to an assignment.
    pub(crate) inaccessible: Vec<String>,
    /// The verbs the profile assignment is refused with `404` for.
    pub(crate) refuse_assign: Vec<Method>,
    /// An answer to `/session` that replaces the real one.
    pub(crate) session_fault: Option<(u16, String)>,
}

/// The simulated ABM, as a handler.
#[derive(Clone, Default)]
pub(crate) struct Sim(pub(crate) Arc<Mutex<SimState>>);

impl Sim {
    /// The state, to arrange or to read.
    pub(crate) fn state(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lists a device whose profile status is `status`.
    pub(crate) fn device(&self, serial: &str, status: &str, profile: Option<&str>) {
        self.state().devices.push(SimDevice {
            serial: serial.to_owned(),
            profile_status: status.to_owned(),
            profile_uuid: profile.map(str::to_owned),
        });
    }

    /// The handler of this simulation.
    pub(crate) fn handler(&self) -> Handler {
        let sim = self.clone();
        Box::new(move |request| sim.answer(request))
    }

    fn record(device: &SimDevice, op: Option<&str>) -> Value {
        let mut record = json!({
            "serial_number": device.serial,
            "model": "Mac Studio",
            "description": "MAC STUDIO",
            "profile_status": device.profile_status,
            "color": "SILVER",
            "os": "OSX",
            "device_family": "Mac",
        });
        if let Some(uuid) = &device.profile_uuid {
            record["profile_uuid"] = json!(uuid);
        }
        if let Some(op) = op {
            record["op_type"] = json!(op);
        }
        record
    }

    fn answer(&self, request: &AbmRequest) -> Reply {
        let mut state = self.state();
        if request.path == "/session" {
            if let Some((status, body)) = &state.session_fault {
                return text_reply(*status, body);
            }
            if !header(request, "Authorization")
                .is_some_and(|h| h.starts_with("OAuth realm=\"ADM\""))
            {
                return text_reply(403, "signature_invalid");
            }
            state.sessions += 1;
            let session = format!("session-{}", state.sessions);
            state.session = Some(session.clone());
            return json_reply(200, &json!({ "auth_session_token": session }));
        }
        if header(request, "X-ADM-Auth-Session") != state.session.as_deref()
            || state.session.is_none()
        {
            return text_reply(403, "Forbidden");
        }
        let body = body_of(request);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/account") => json_reply(
                200,
                &json!({ "server_name": "js-mdm", "org_name": "Org", "org_id": "1" }),
            ),
            ("POST", "/server/devices") => {
                let from = body["cursor"]
                    .as_str()
                    .and_then(|c| c.strip_prefix("full-"))
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                let end = if state.page == 0 {
                    state.devices.len()
                } else {
                    (from + state.page).min(state.devices.len())
                };
                let devices: Vec<Value> = state.devices[from..end]
                    .iter()
                    .map(|d| Self::record(d, None))
                    .collect();
                let more = end < state.devices.len();
                let cursor = if more {
                    format!("full-{end}")
                } else {
                    format!("c{}", state.log.len())
                };
                json_reply(
                    200,
                    &json!({ "cursor": cursor, "devices": devices, "more_to_follow": more, "fetched_until": "2026-11-10T00:00:00Z" }),
                )
            }
            ("POST", "/devices/sync") => {
                let cursor = body["cursor"].as_str().unwrap_or_default().to_owned();
                let Some(from) = cursor
                    .strip_prefix('c')
                    .and_then(|n| n.parse::<usize>().ok())
                else {
                    return text_reply(400, "INVALID_CURSOR");
                };
                if state.dead_cursors.contains(&cursor) || from > state.log.len() {
                    return text_reply(400, "INVALID_CURSOR");
                }
                let changes: Vec<(String, String)> = state.log[from..].to_vec();
                let devices: Vec<Value> = changes
                    .iter()
                    .map(|(serial, op)| {
                        let device = state
                            .devices
                            .iter()
                            .find(|d| &d.serial == serial)
                            .cloned()
                            .unwrap_or(SimDevice {
                                serial: serial.clone(),
                                profile_status: "removed".to_owned(),
                                profile_uuid: None,
                            });
                        Self::record(&device, Some(op))
                    })
                    .collect();
                json_reply(
                    200,
                    &json!({ "cursor": format!("c{}", state.log.len()), "devices": devices, "more_to_follow": false }),
                )
            }
            ("POST", "/profile") => {
                let uuid = format!("PROFILE{:028}", state.profiles.len() + 1);
                state.profiles.insert(uuid.clone(), body);
                json_reply(200, &json!({ "profile_uuid": uuid, "devices": {} }))
            }
            ("POST" | "PUT", "/profile/devices") => {
                if state.refuse_assign.contains(&request.method) {
                    return text_reply(404, "Not Found");
                }
                let uuid = body["profile_uuid"].as_str().unwrap_or_default().to_owned();
                let mut outcome = serde_json::Map::new();
                for serial in body["devices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    let ok = !state.inaccessible.iter().any(|s| s == serial);
                    if ok
                        && let Some(device) = state.devices.iter_mut().find(|d| d.serial == serial)
                    {
                        device.profile_status = "assigned".to_owned();
                        device.profile_uuid = Some(uuid.clone());
                    }
                    outcome.insert(
                        serial.to_owned(),
                        json!(if ok { "SUCCESS" } else { "NOT_ACCESSIBLE" }),
                    );
                }
                json_reply(200, &json!({ "profile_uuid": uuid, "devices": outcome }))
            }
            _ => text_reply(404, "Not Found"),
        }
    }
}

/// A token of made-up parts.
pub(crate) fn sample_token() -> crate::mdm_secrets::AbmToken {
    crate::mdm_secrets::AbmToken {
        consumer_key: "CK_abc".to_owned(),
        consumer_secret: "CS+secret/x".to_owned(),
        access_token: "AT~tok".to_owned(),
        access_secret: "AS secret&".to_owned(),
        access_secret_expiry: "2027-10-31T22:01:36Z".to_owned(),
    }
}

/// The time every test's clock reads.
pub(crate) const NOW: u64 = 1_700_000_000;

/// A clock that always reads `unix`.
pub(crate) fn clock_at(unix: u64) -> hostkeeper_oidc::Clock {
    Arc::new(move || unix)
}

/// Random bytes that are `0, 1, 2, ...`.
pub(crate) fn counting_random() -> crate::enrollment::Random {
    Box::new(|bytes: &mut [u8]| {
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from(i).unwrap_or(0);
        }
        true
    })
}

/// A source of random bytes that has run dry.
pub(crate) fn dry_random() -> crate::enrollment::Random {
    Box::new(|_: &mut [u8]| false)
}

/// A sleeper that returns at once and records how long each wait was asked to be.
pub(crate) fn recording_sleep() -> (super::client::Sleep, Arc<Mutex<Vec<std::time::Duration>>>) {
    let waits: Arc<Mutex<Vec<std::time::Duration>>> = Arc::default();
    let seen = Arc::clone(&waits);
    let sleep: super::client::Sleep = Arc::new(move |wait| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(wait);
        Box::pin(async {})
    });
    (sleep, waits)
}

/// The pacing the tests use: 30 seconds, 3 retries, a wait of at most 300 seconds.
pub(crate) fn pacing() -> super::client::Pacing {
    super::client::Pacing {
        timeout: std::time::Duration::from_secs(30),
        retries: 3,
        max_wait: std::time::Duration::from_secs(300),
    }
}

/// A client over `transport` at [`NOW`] with counting nonces, and the waits it is asked for.
pub(crate) fn client_over(
    transport: Arc<dyn AbmTransport>,
) -> (
    super::client::AbmClient,
    Arc<Mutex<Vec<std::time::Duration>>>,
) {
    let (sleep, waits) = recording_sleep();
    (
        super::client::AbmClient::new(transport, clock_at(NOW), counting_random(), sleep),
        waits,
    )
}

/// A CMS recipient identifier, whatever it names.
pub(crate) fn rid() -> cms::enveloped_data::RecipientIdentifier {
    use cms::der::asn1::OctetString;
    cms::enveloped_data::RecipientIdentifier::SubjectKeyIdentifier(
        cms::x509_cert::ext::pkix::SubjectKeyIdentifier(
            OctetString::new(vec![1, 2, 3, 4]).expect("an octet string"),
        ),
    )
}

/// A random source for the builders of envelopes.
pub(crate) fn rng() -> rsa::rand_core::UnwrapErr<getrandom::SysRng> {
    rsa::rand_core::UnwrapErr(getrandom::SysRng)
}

/// The DER of a CMS message holding `enveloped`.
pub(crate) fn der_of(enveloped: &cms::enveloped_data::EnvelopedData) -> Vec<u8> {
    use cms::der::Encode as _;
    cms::content_info::ContentInfo {
        content_type: cms::der::asn1::ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.3"),
        content: cms::der::Any::encode_from(enveloped).expect("encodes"),
    }
    .to_der()
    .expect("DER")
}

/// An envelope made by the `cms` builder: key transport by PKCS#1 v1.5 to `to`, content by
/// `algorithm`.
pub(crate) fn built(
    plain: &[u8],
    to: &rsa::PrivateKey,
    algorithm: cms::builder::ContentEncryptionAlgorithm,
) -> Vec<u8> {
    use cms::builder::{EnvelopedDataBuilder, KeyEncryptionInfo, KeyTransRecipientInfoBuilder};
    let mut source = rng();
    let mut builder = EnvelopedDataBuilder::new(None, plain, algorithm, None).expect("a builder");
    builder
        .add_recipient_info(
            KeyTransRecipientInfoBuilder::new(rid(), KeyEncryptionInfo::Rsa(to.to_public_key()))
                .expect("a recipient"),
        )
        .expect("added");
    der_of(&builder.build_with_rng(&mut source).expect("an envelope"))
}

/// The clock the coordinator's tests give the ABM client: always [`NOW`].
pub(crate) fn client_clock() -> hostkeeper_oidc::Clock {
    clock_at(NOW)
}

/// An ABM client over a simulated ABM and tables in a scratch directory the caller keeps.
pub(crate) fn simulated_abm(
    name: &str,
) -> (super::Abm, Sim, Arc<FakeAbm>, hostkeeper_testkit::Scratch) {
    let sim = Sim::default();
    let fake = FakeAbm::new(sim.handler());
    let dir = crate::testutil::scratch(name);
    let store = Arc::new(
        crate::store::mdm::files::FileMdm::open(&dir.path().join("mdm")).expect("the tables"),
    );
    let (sleep, _) = recording_sleep();
    let abm = super::Abm::new(
        store,
        Arc::clone(&fake) as _,
        clock_at(NOW),
        counting_random(),
        sleep,
    );
    (abm, sim, fake, dir)
}
