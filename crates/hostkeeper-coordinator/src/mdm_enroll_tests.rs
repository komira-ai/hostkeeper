//! The gate's enrollment side in the coordinator (docs/design/hostkeeper-mdm.md sections 5.1
//! to 5.4): the real gate over the real enrollment store, NanoHUB a fake on loopback that
//! queues every command. A request posted wakes the gate, which pushes a nonce bound to it; the
//! Mac's repost with that nonce is approved, the push is audited, and the nonce profile is
//! removed; a wiped Mac takes its own host id back through a new nonce. And the start refuses
//! the two flags of the side when they are wrong.

use std::convert::Infallible;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use hostkeeper_enrollment::enroll::{ApprovalRecord, Decision, RequestState};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::Role;
use hostkeeper_mdm_gate::enroll::{TICK, run};
use hostkeeper_mdm_gate::{
    DeviceRecord, DeviceStore, FileDeviceStore, Phase, ProfileAnswer, ProfileCommand,
};
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plist::{Dictionary, Value};

use crate::enrollment::tests::support::{
    FixedClock, HostKey, NOW, Rig, SERIAL, audit_of, audited, coordinator_over, host_id,
    mac_request,
};
use crate::enrollment::{GATE_AUDITOR, MacAllowlist, MacPolicy, os_random};
use crate::mdm::tests::{devices_dir, flags_in, record};
use crate::mdm::{Mdm, MdmError, MdmFlags};
use crate::testutil::scratch;

/// The platform UUID [`mac_request`] claims: the device's UDID, and its enrollment id.
const PLATFORM_UUID: &str = "4C4C4544-0042-5A10-8048-B7C04F4E3332";

/// The farm's profile identifier in these tests.
const IDENTIFIER: &str = "com.example.hostkeeper.mdm";

/// The allowlist entry of [`SERIAL`]: host id `mac-<serial>`, role `build`, service `kbf`.
const ALLOWED: &str = r#"
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"
role = "build"
services = ["kbf"]
"#;

/// A daemon config the daemon's loader accepts.
const CONFIG: &str = r#"coordinator = "https://coordinator.example:8443"
ca_sha256 = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// NanoHUB's API on loopback, port 0: it queues every command and keeps each body.
struct Hub {
    url: String,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    told: Arc<tokio::sync::Notify>,
}

impl Hub {
    async fn start() -> Hub {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let told = Arc::new(tokio::sync::Notify::new());
        let (b, t) = (Arc::clone(&bodies), Arc::clone(&told));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (b, t) = (Arc::clone(&b), Arc::clone(&t));
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let (b, t) = (Arc::clone(&b), Arc::clone(&t));
                        async move {
                            let body = request
                                .into_body()
                                .collect()
                                .await
                                .map(|body| body.to_bytes().to_vec())
                                .unwrap_or_default();
                            b.lock().unwrap_or_else(PoisonError::into_inner).push(body);
                            t.notify_one();
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("{}"))))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Hub { url, bodies, told }
    }

    /// The `Command` dictionary of every command heard.
    fn commands(&self) -> Vec<Dictionary> {
        self.bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|body| {
                Value::from_reader(std::io::Cursor::new(body))
                    .expect("a plist")
                    .into_dictionary()
                    .expect("a dictionary")["Command"]
                    .as_dictionary()
                    .expect("a Command")
                    .clone()
            })
            .collect()
    }

    /// Resolves once at least `n` commands were heard.
    async fn heard(&self, n: usize) {
        while self.commands().len() < n {
            self.told.notified().await;
        }
    }

    /// The nonces of the nonce profiles installed, in order.
    fn nonces(&self) -> Vec<Vec<u8>> {
        self.commands()
            .iter()
            .filter(|command| command["RequestType"].as_string() == Some("InstallProfile"))
            .filter_map(|command| {
                let payload = command["Payload"].as_data().expect("a Payload");
                let profile = Value::from_reader(std::io::Cursor::new(payload))
                    .expect("a profile")
                    .into_dictionary()
                    .expect("a dictionary");
                let settings = profile["PayloadContent"].as_array().expect("payloads")[0]
                    .as_dictionary()
                    .expect("a payload")["PayloadContent"]
                    .as_dictionary()
                    .expect("domains")
                    .get("dev.hostkeeper.enrollment")?
                    .as_dictionary()
                    .expect("the domain")["Forced"]
                    .as_array()
                    .expect("Forced")[0]
                    .as_dictionary()
                    .expect("an entry")["mcx_preference_settings"]
                    .as_dictionary()
                    .expect("settings")
                    .clone();
                Some(settings["nonce"].as_data().expect("data").to_vec())
            })
            .collect()
    }

    /// The identifiers of the profiles removed.
    fn removed(&self) -> Vec<String> {
        self.commands()
            .iter()
            .filter(|command| command["RequestType"].as_string() == Some("RemoveProfile"))
            .map(|command| command["Identifier"].as_string().expect("text").to_owned())
            .collect()
    }
}

/// The gate's flags over `dir` with the enrollment side, NanoHUB at `url`.
fn side_flags(dir: &Scratch, url: &str) -> MdmFlags {
    let config = dir.path().join("hostkeeper.toml");
    std::fs::write(&config, CONFIG).expect("the config");
    MdmFlags {
        mdm_api_url: Some(url.to_owned()),
        mdm_profile_identifier: Some(IDENTIFIER.to_owned()),
        mdm_daemon_config: Some(config),
        ..flags_in(dir)
    }
}

/// The Mac's device record: enrolled and supervised since `since`, its config profile
/// acknowledged, so the next profile the gate sends it is its nonce's.
fn ready(since: u64, enrollment: u32) -> DeviceRecord {
    let mut record = record(PLATFORM_UUID, Some(SERIAL), Phase::Enrolled, Some(true));
    record.enrolled_since_unix = Some(since);
    record.enrollment = enrollment;
    record.config_profile = Some(ProfileCommand {
        uuid: hostkeeper_mdm_gate::CommandUuid::from_random([7; 16]),
        sent_unix: since,
        answer: Some(ProfileAnswer {
            acknowledged: true,
            at_unix: since,
        }),
    });
    record
}

/// The gate over `records` (written first) with the enrollment side, its clock `clock`, and
/// an enrollment (its own clock at [`NOW`]) that asks it and tells it of each request,
/// attached to it.
fn gate_and_enrollment(
    dir: &Scratch,
    hub: &Hub,
    clock: &Arc<FixedClock>,
    records: &[DeviceRecord],
    name: &str,
) -> (Mdm, Rig) {
    let store = FileDeviceStore::open(Arc::new(RealFs), &devices_dir(dir)).expect("a store");
    for record in records {
        store.save(record).expect("saved");
    }
    let mdm = Mdm::open(
        &side_flags(dir, &hub.url),
        dir.path(),
        Arc::clone(clock) as _,
        os_random(),
    )
    .expect("the gate opens")
    .expect("the flags name a gate");
    let policy = MacPolicy {
        device_state: Mdm::device_state(Some(&mdm)),
        allowlist: MacAllowlist::parse(ALLOWED).expect("an allowlist"),
    };
    let posted = Mdm::posted(Some(&mdm)).expect("a gate is told");
    let rig = Rig::configured(name, Arc::new(RealFs), os_random(), |enrollment| {
        enrollment.with_mac(policy).with_posted(posted)
    });
    mdm.attach(&rig.enrollment);
    (mdm, rig)
}

/// Catches: a request posted that does not wake the gate (the nonce waits for the minute), a
/// nonce the Mac's repost is not approved by (bound to another request, issued for another
/// serial), the push not audited, the nonce profile left on the approved Mac.
#[tokio::test]
async fn a_posted_request_gets_a_nonce_that_approves_it_and_the_profile_is_removed() {
    let hub = Hub::start().await;
    let dir = scratch("mdm-enroll-flow");
    let clock = FixedClock::at(NOW);
    let (mdm, rig) =
        gate_and_enrollment(&dir, &hub, &clock, &[ready(NOW, 1)], "mdm-enroll-flow-rig");
    let coordinator = coordinator_over("mdm-enroll-flow-coordinator", &rig).await;
    let before = audit_of(&coordinator.coordinator).len();
    // An hour between ticks: only the request's wake makes the gate look during the test.
    let looping = tokio::spawn(run(Arc::clone(&mdm.gate), TICK * 60));

    let key = HostKey::new();
    let posted = rig
        .enrollment
        .enroll(mac_request(&key, "machine-mac", SERIAL, &[]))
        .expect("posted");
    assert_eq!(posted.state, EnrollState::Pending as i32);
    hub.heard(1).await;
    // A look takes the gate's turn, so this one starts once the woken look is done.
    mdm.gate.tick().await;
    looping.abort();
    let nonces = hub.nonces();
    assert_eq!(nonces.len(), 1);

    let enrollment = coordinator
        .coordinator
        .enrollment()
        .cloned()
        .expect("enrollment");
    coordinator
        .coordinator
        .enroll(
            &enrollment,
            mac_request(&key, "machine-mac", SERIAL, &nonces[0]),
        )
        .expect("reposted");
    let state = rig
        .enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state;
    assert_eq!(state, RequestState::Approved);
    let audit = audit_of(&coordinator.coordinator).split_off(before);
    assert!(
        audit.contains(&audited(
            GATE_AUDITOR,
            "mdm_nonce_pushed",
            None,
            &format!("serial {SERIAL}; request {}; push 1", key.key_hash())
        )),
        "{audit:?}"
    );

    mdm.gate.tick().await;
    assert_eq!(hub.removed(), [format!("{IDENTIFIER}.nonce")]);
}

/// Catches: a wiped Mac (a new enrollment, a new key) whose old key's approved request counts
/// as approved in this enrollment, so it gets no nonce; or whose nonce approval does not take
/// its own host id back from the old key (design 5.1).
#[tokio::test]
async fn a_wiped_mac_replaces_its_own_host_id_by_the_gates_nonce() {
    let hub = Hub::start().await;
    let dir = scratch("mdm-enroll-wiped");
    let clock = FixedClock::at(NOW + 100);
    let (mdm, rig) = gate_and_enrollment(
        &dir,
        &hub,
        &clock,
        &[ready(NOW + 100, 2)],
        "mdm-enroll-wiped-rig",
    );
    // The old key, approved as the Mac's host id by its own nonce before the wipe.
    rig.clock.set(NOW);
    let old = HostKey::new();
    rig.enrollment
        .enroll(mac_request(&old, "machine-mac", SERIAL, &[]))
        .expect("posted");
    let nonce = rig
        .enrollment
        .issue_mac_nonce(SERIAL, &old.key_hash())
        .expect("a nonce");
    let approved = rig
        .enrollment
        .enrolled(mac_request(&old, "machine-mac", SERIAL, nonce.as_bytes()))
        .expect("approved");
    assert!(approved.by_nonce.is_some());

    rig.clock.set(NOW + 100);
    let new = HostKey::new();
    rig.enrollment
        .enroll(mac_request(&new, "machine-mac-2", SERIAL, &[]))
        .expect("posted");
    mdm.gate.tick().await;
    let nonces = hub.nonces();
    assert_eq!(
        nonces.len(),
        1,
        "the old approval is of an earlier enrollment"
    );
    let enrolled = rig
        .enrollment
        .enrolled(mac_request(&new, "machine-mac-2", SERIAL, &nonces[0]))
        .expect("reposted");
    assert!(
        enrolled.by_nonce.is_some(),
        "approved by the nonce: {:?}",
        rig.enrollment.mac_auto_approve(&new.key_hash(), &nonces[0])
    );
    let view = rig
        .enrollment
        .store()
        .request(&new.key_hash())
        .expect("read")
        .expect("held");
    assert_eq!(view.state, RequestState::Approved);
    assert_eq!(
        view.host_id.map(|h| h.as_str().to_owned()).as_deref(),
        Some("mac-c02xk1abjg5j")
    );
}

/// Catches: a gate started with a profile identifier that is no reverse-DNS name, a daemon
/// config that cannot be read, or one the daemon's loader refuses; a refusal that names
/// neither the flag nor the reason.
#[tokio::test]
async fn the_start_refuses_a_bad_identifier_or_daemon_config() {
    let dir = scratch("mdm-enroll-flags");
    let good = side_flags(&dir, "http://127.0.0.1:9004");
    let open = |flags: &MdmFlags| {
        Mdm::open(flags, dir.path(), FixedClock::at(NOW), os_random()).expect_err("refused")
    };

    let flags = MdmFlags {
        mdm_profile_identifier: Some("hostkeeper".to_owned()),
        ..good.clone()
    };
    let error = open(&flags);
    assert!(matches!(error, MdmError::Identifier(_)), "{error}");

    let missing = dir.path().join("no-such.toml");
    let flags = MdmFlags {
        mdm_daemon_config: Some(missing.clone()),
        ..good.clone()
    };
    let error = open(&flags);
    assert!(
        error
            .to_string()
            .starts_with(&format!("--mdm-daemon-config {}: ", missing.display())),
        "{error}"
    );

    let refused = dir.path().join("refused.toml");
    std::fs::write(&refused, CONFIG.replace("release_channel", "channel")).expect("written");
    let flags = MdmFlags {
        mdm_daemon_config: Some(refused),
        ..good.clone()
    };
    let error = open(&flags);
    assert!(matches!(error, MdmError::Config(_)), "{error}");
    assert!(
        error.to_string().starts_with("--mdm-daemon-config: "),
        "{error}"
    );

    // Before the enrollment is attached the side allows no serial: a ready Mac is sent
    // nothing, and its record is unchanged. Without the side, attaching does nothing.
    let store = FileDeviceStore::open(Arc::new(RealFs), &devices_dir(&dir)).expect("a store");
    store.save(&ready(NOW, 1)).expect("saved");
    let mdm = Mdm::open(&good, dir.path(), FixedClock::at(NOW), os_random())
        .expect("opens")
        .expect("a gate");
    mdm.gate.tick().await;
    assert_eq!(mdm.gate.records(), [ready(NOW, 1)]);
    let without = Mdm::open(
        &flags_in(&dir),
        dir.path(),
        FixedClock::at(NOW),
        os_random(),
    )
    .expect("opens")
    .expect("a gate");
    let rig = Rig::new("mdm-enroll-flags-rig");
    without.attach(&rig.enrollment);
    without.gate.tick().await;
    assert_eq!(without.gate.records(), [ready(NOW, 1)]);
    assert!(Mdm::posted(None).is_none());
}

/// An approval of `key`'s request as `host`, recorded by `operator` at `at`.
fn approve_at(rig: &Rig, key: &HostKey, host: &str, at: u64) {
    rig.enrollment
        .store()
        .approve_recorded(
            &key.key_hash(),
            &key.key_hash(),
            &Decision {
                host_id: host_id(host),
                role: Role::parse("build").expect("a role"),
                services: Vec::new(),
                replace: false,
            },
            &ApprovalRecord {
                approver: "operator".to_owned(),
                approved_unix: at,
                reason: "a test".to_owned(),
                mac_nonce: None,
                mac_enrolled_since_unix: None,
            },
        )
        .expect("approved");
}

/// Catches: the coordinator's answers to the gate not those of its store: a duplicate claim not
/// flagged (so the gate takes it as a candidate), a request of another serial listed, a
/// request's posting time or platform UUID changed; an approval of another serial counted as
/// the Mac's, or an earlier approval taken for the last; a nonce not issued for a waiting
/// request, or issued for a malformed serial; the allowlist not the one the enrollment holds.
#[test]
fn the_enrollment_answers_the_gate_from_its_store() {
    use hostkeeper_mdm_gate::Enrollments as _;
    let policy = MacPolicy {
        allowlist: MacAllowlist::parse(ALLOWED).expect("an allowlist"),
        ..MacPolicy::default()
    };
    let rig = Rig::configured(
        "mdm-enroll-port",
        Arc::new(RealFs),
        os_random(),
        |enrollment| enrollment.with_mac(policy),
    );
    let e = &rig.enrollment;
    assert!(e.allowed(SERIAL));
    assert!(!e.allowed("C02OTHER0001"));

    let (first, twin, other) = (HostKey::new(), HostKey::new(), HostKey::new());
    e.enroll(mac_request(&first, "machine-mac", SERIAL, &[]))
        .expect("posted");
    rig.clock.set(NOW + 3);
    // The same machine id from another key: a duplicate claim.
    e.enroll(mac_request(&twin, "machine-mac", SERIAL, &[]))
        .expect("posted");
    e.enroll(mac_request(&other, "machine-other", "C02OTHER0001", &[]))
        .expect("posted");
    let mut waiting = e.waiting(SERIAL).expect("listed");
    waiting.sort_by_key(|request| request.posted_unix);
    assert_eq!(
        waiting,
        [
            hostkeeper_mdm_gate::Waiting {
                request_id: first.key_hash(),
                platform_uuid: PLATFORM_UUID.to_owned(),
                posted_unix: NOW,
                duplicate_claim: false,
            },
            hostkeeper_mdm_gate::Waiting {
                request_id: twin.key_hash(),
                platform_uuid: PLATFORM_UUID.to_owned(),
                posted_unix: NOW + 3,
                duplicate_claim: true,
            },
        ]
    );

    assert_eq!(e.last_approval_unix(SERIAL).expect("read"), None);
    approve_at(&rig, &other, "mac-other", NOW + 5);
    assert_eq!(e.last_approval_unix(SERIAL).expect("read"), None);
    approve_at(&rig, &first, "mac-first", NOW + 9);
    let later = HostKey::new();
    e.enroll(mac_request(&later, "machine-later", SERIAL, &[]))
        .expect("posted");
    approve_at(&rig, &later, "mac-later", NOW + 7);
    assert_eq!(e.last_approval_unix(SERIAL).expect("read"), Some(NOW + 9));

    let pending = HostKey::new();
    e.enroll(mac_request(&pending, "machine-pending", SERIAL, &[]))
        .expect("posted");
    let nonce = e.issue_nonce(SERIAL, &pending.key_hash()).expect("a nonce");
    assert_eq!(nonce.as_bytes().len(), 32);
    let refused = e
        .issue_nonce("not a serial", &pending.key_hash())
        .expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "a Mac serial is 1 to 64 ASCII letters and digits"
    );
}
