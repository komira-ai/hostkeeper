//! The Mac path: a request with an MDM nonce is approved only when every condition holds, a
//! declined attempt keeps the nonce, the nonce lives one hour, a coordinator without a gate
//! approves no Mac, claims approve nothing, no answer or audit entry carries the nonce, and
//! the allowlist and the issue refuse what they must.

use std::io::Write as _;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{MAC_NONCE_VALID_SECS, MacNonceHash, RequestState};
use hostkeeper_enrollment::fs::{FaultFs, Fs, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollResponse, EnrollState};
use hostkeeper_proto::hostkeeper::operator::v1::{EnrollmentRefusal, ListPendingRequest};
use prost::Message;

use super::support::{
    Audited, HostKey, NOW, Rig, SERIAL, audit_after, audit_of, audited, coordinator_over, enrolled,
    host_id, mac_request, requested,
};
use crate::enrollment::{
    AllowlistError, DeviceStateError, DeviceStatus, Enrollment, FakeDeviceState, MacAllowlist,
    MacDecline, MacNonceError, MacOutcome, MacPolicy, Random, os_random,
};
use crate::testutil::{OPERATOR, scratch};

/// The allowlist entry of [`SERIAL`]: host id `mac-<serial>`, role `build`, service `kbf`.
const ALLOWED: &str = r#"
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"
role = "build"
services = ["kbf"]
"#;

/// The host id the allowlist gives [`SERIAL`].
const MAC_HOST: &str = "mac-c02xk1abjg5j";

/// A device the gate says is enrolled and supervised.
const GOOD: DeviceStatus = DeviceStatus {
    enrolled: true,
    supervised: true,
};

/// An enrollment over `fs` approving a Mac as `allowlist` and the gate `gate` say.
fn mac_rig(name: &str, fs: Arc<dyn Fs>, gate: &Arc<FakeDeviceState>, allowlist: &str) -> Rig {
    let policy = MacPolicy {
        device_state: gate.clone(),
        allowlist: MacAllowlist::parse(allowlist).expect("an allowlist"),
    };
    Rig::configured(name, fs, os_random(), |enrollment| {
        enrollment.with_mac(policy)
    })
}

/// A gate that says `serial` is `status`.
fn gate_saying(
    serial: &str,
    status: Result<DeviceStatus, DeviceStateError>,
) -> Arc<FakeDeviceState> {
    let gate = Arc::new(FakeDeviceState::default());
    gate.set(serial, status);
    gate
}

/// The audit entry of a declined attempt for `key`'s request.
fn declined(why: MacDecline, key: &HostKey) -> Audited {
    audited(
        "mdm-nonce",
        "mac_auto_approve_declined",
        None,
        &format!(
            "{}; request {}; serial {SERIAL}",
            why.token(),
            key.key_hash()
        ),
    )
}

/// The state the store holds `key`'s request in.
fn state_of(enrollment: &Enrollment, key: &HostKey) -> RequestState {
    enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state
}

/// One row of the table: what it changes from the case where every condition holds.
#[derive(Clone, Copy, Debug)]
enum Change {
    Nothing,
    UnknownNonce,
    Expired,
    Used,
    WrongSerial,
    NotEnrolled,
    NotSupervised,
    NotAllowlisted,
    GateError,
}

/// Catches any one condition dropped: each row changes exactly one, only the row that changes
/// none approves, and each other leaves the request pending and audits its own token.
#[tokio::test]
async fn mac_auto_approve_requires_every_condition() {
    let rows = [
        (Change::Nothing, None),
        (Change::UnknownNonce, Some(MacDecline::NonceUnknown)),
        (Change::Expired, Some(MacDecline::NonceExpired)),
        (Change::Used, Some(MacDecline::NonceUsed)),
        (Change::WrongSerial, Some(MacDecline::NonceWrongSerial)),
        (Change::NotEnrolled, Some(MacDecline::DeviceNotEnrolled)),
        (Change::NotSupervised, Some(MacDecline::DeviceNotSupervised)),
        (Change::NotAllowlisted, Some(MacDecline::SerialNotAllowed)),
        (Change::GateError, Some(MacDecline::DeviceStateUnavailable)),
    ];
    for (n, (change, decline)) in rows.into_iter().enumerate() {
        let status = match change {
            Change::NotEnrolled => Ok(DeviceStatus {
                enrolled: false,
                ..GOOD
            }),
            Change::NotSupervised => Ok(DeviceStatus {
                supervised: false,
                ..GOOD
            }),
            Change::GateError => Err(DeviceStateError("the gate is down".to_owned())),
            _ => Ok(GOOD),
        };
        let allowlist = match change {
            Change::NotAllowlisted => "",
            _ => ALLOWED,
        };
        let gate = gate_saying(SERIAL, status);
        let name = format!("mac-row-{n}");
        let enrollment = mac_rig(&name, Arc::new(RealFs), &gate, allowlist);
        let rig = coordinator_over(&format!("{name}-coordinator"), &enrollment).await;
        let e = &enrollment.enrollment;
        let issued_for = match change {
            Change::WrongSerial => "OTHERSERIAL1",
            _ => SERIAL,
        };
        let nonce = e.issue_mac_nonce(issued_for).expect("a nonce");
        let presented = match change {
            Change::UnknownNonce => b"a nonce the gate never issued".to_vec(),
            _ => nonce.as_bytes().to_vec(),
        };
        if let Change::Used = change {
            let first = HostKey::new();
            e.enroll(mac_request(
                &first,
                "machine-first",
                SERIAL,
                nonce.as_bytes(),
            ))
            .expect("enrolled");
            assert_eq!(state_of(e, &first), RequestState::Approved);
        }
        let offset = match change {
            Change::Expired => MAC_NONCE_VALID_SECS + 1,
            _ => 10,
        };
        enrollment.clock.set(NOW + offset);
        let key = HostKey::new();
        let before = audit_of(&rig.coordinator).len();
        let answer = e
            .enroll(mac_request(&key, "machine-mac", SERIAL, &presented))
            .expect("enrolled");
        match decline {
            None => {
                assert_eq!(answer.state, EnrollState::Unspecified as i32, "{change:?}");
                assert_eq!(state_of(e, &key), RequestState::Approved);
                let recorded = e.store().recorded_approvals().expect("read");
                assert_eq!(recorded.len(), 1);
                assert_eq!(
                    (recorded[0].host_id.as_str(), recorded[0].role.as_str()),
                    (MAC_HOST, "build")
                );
                assert_eq!(recorded[0].services, ["kbf"]);
                assert!(recorded[0].mac_nonce);
                assert_eq!(
                    audit_after(&rig.coordinator, before),
                    [
                        requested(&key, None, "pending"),
                        audited(
                            "mdm-nonce",
                            "enroll_approve",
                            Some(MAC_HOST),
                            &format!(
                                "request {}; role build; services kbf; claims machine_id=machine-mac \
                             mac_serial={SERIAL} platform_uuid=4C4C4544-0042-5A10-8048-B7C04F4E3332; \
                             approved by its MDM nonce",
                                key.key_hash()
                            )
                        )
                    ]
                );
            }
            Some(why) => {
                assert_eq!(answer.state, EnrollState::Pending as i32, "{change:?}");
                assert_eq!(state_of(e, &key), RequestState::Pending, "{change:?}");
                assert_eq!(
                    audit_after(&rig.coordinator, before),
                    [requested(&key, None, "pending"), declined(why, &key)],
                    "{change:?}"
                );
            }
        }
    }
}

/// Catches a nonce consumed by an attempt rather than by the approval: declined while the
/// device is not supervised, the same nonce approves once it is.
#[tokio::test]
async fn a_declined_attempt_does_not_burn_the_nonce() {
    let gate = gate_saying(
        SERIAL,
        Ok(DeviceStatus {
            supervised: false,
            ..GOOD
        }),
    );
    let enrollment = mac_rig("mac-not-burnt", Arc::new(RealFs), &gate, ALLOWED);
    let e = &enrollment.enrollment;
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    let key = HostKey::new();
    let request = mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes());
    e.enroll(request.clone()).expect("enrolled");
    assert_eq!(state_of(e, &key), RequestState::Pending);
    assert_eq!(
        e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
        MacOutcome::Declined(MacDecline::DeviceNotSupervised)
    );
    gate.set(SERIAL, Ok(GOOD));
    let again = e.enroll(request.clone()).expect("posted again");
    assert_eq!(again.state, EnrollState::Unspecified as i32);
    assert_eq!(state_of(e, &key), RequestState::Approved);
    // Posted once more after its approval: nothing is tried again, and it stays approved.
    let late = e.enroll(request).expect("posted once more");
    assert_eq!(late.state, EnrollState::Unspecified as i32);
    assert_eq!(state_of(e, &key), RequestState::Approved);
}

/// The allowlist of [`SERIAL`] and of a second serial approved as another host with another
/// role.
const TWO_ALLOWED: &str = r#"
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"
role = "build"
services = ["kbf"]

[[mac]]
serial = "C02VIP000001"
host_id_prefix = "vip-"
role = "admin"
services = ["kbf"]
"#;

/// Catches the serial of a later post being trusted: a request first posted claiming
/// [`SERIAL`] and posted again claiming another allowlisted serial, with the nonce issued for
/// [`SERIAL`], is approved as [`SERIAL`]'s host and role, never as the other serial's.
#[tokio::test]
async fn a_repost_claiming_another_serial_is_judged_by_the_first() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    gate.set("C02VIP000001", Ok(GOOD));
    let enrollment = mac_rig("mac-repost-serial", Arc::new(RealFs), &gate, TWO_ALLOWED);
    let e = &enrollment.enrollment;
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    let key = HostKey::new();
    e.enroll(mac_request(&key, "machine-mac", SERIAL, &[]))
        .expect("posted without a nonce");
    let answer = e
        .enroll(mac_request(
            &key,
            "machine-mac",
            "C02VIP000001",
            nonce.as_bytes(),
        ))
        .expect("posted again");
    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    let recorded = e.store().recorded_approvals().expect("read");
    assert_eq!(
        (recorded[0].host_id.as_str(), recorded[0].role.as_str()),
        (MAC_HOST, "build")
    );
    assert_eq!(
        e.store()
            .host_role(&host_id("vip-c02vip000001"))
            .expect("read"),
        None
    );
    // The gate answers nothing for a serial it was not told about.
    assert_eq!(
        crate::enrollment::DeviceState::status(&*gate, "C02UNKNOWN01"),
        Err(DeviceStateError("unknown device".to_owned()))
    );
}

/// Catches a serial's text put in the audit as it came, and a store that cannot be read
/// approving a Mac: a serial with a space is quoted with `?`, and a failed read is
/// `store_failed` with the request left pending.
#[tokio::test]
async fn a_mac_path_quotes_its_serial_and_fails_closed() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let fs = Arc::new(FaultFs::new());
    let enrollment = mac_rig("mac-closed", fs.clone(), &gate, ALLOWED);
    let rig = coordinator_over("mac-closed-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    let odd = HostKey::new();
    let before = audit_of(&rig.coordinator).len();
    e.enroll(mac_request(&odd, "machine-odd", "C02 X", nonce.as_bytes()))
        .expect("enrolled");
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            requested(&odd, None, "pending"),
            audited(
                "mdm-nonce",
                "mac_auto_approve_declined",
                None,
                &format!(
                    "serial_not_allowed; request {}; serial C02?X",
                    odd.key_hash()
                )
            )
        ]
    );
    let key = HostKey::new();
    e.enroll(mac_request(&key, "machine-mac", SERIAL, &[]))
        .expect("enrolled without a nonce");
    fs.fail_at(fs.count() + 1);
    assert_eq!(
        e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
        MacOutcome::Refused(EnrollmentRefusal::StoreFailed)
    );
    assert_eq!(state_of(e, &key), RequestState::Pending);
}

/// Catches a window that is not exactly one hour: a nonce approves at 3599 and 3600 seconds
/// after its issue and not at 3601, by the injected clock.
#[tokio::test]
async fn a_nonce_expires_after_exactly_one_hour() {
    for (offset, approves) in [
        (MAC_NONCE_VALID_SECS - 1, true),
        (MAC_NONCE_VALID_SECS, true),
        (MAC_NONCE_VALID_SECS + 1, false),
    ] {
        let gate = gate_saying(SERIAL, Ok(GOOD));
        let enrollment = mac_rig(
            &format!("mac-hour-{offset}"),
            Arc::new(RealFs),
            &gate,
            ALLOWED,
        );
        let e = &enrollment.enrollment;
        let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
        enrollment.clock.set(NOW + offset);
        let key = HostKey::new();
        e.enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
            .expect("enrolled");
        let approved = state_of(e, &key) == RequestState::Approved;
        assert_eq!(approved, approves, "{offset} seconds after the issue");
    }
}

/// Catches a coordinator started without a gate approving a Mac: with no allowlist the serial
/// is not allowed, and with one the device is unknown.
#[tokio::test]
async fn no_gate_configured_auto_approves_nothing() {
    let enrollment = Rig::new("mac-no-gate");
    let rig = coordinator_over("mac-no-gate-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    let key = HostKey::new();
    let before = audit_of(&rig.coordinator).len();
    let answer = e
        .enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
        .expect("enrolled");
    assert_eq!(answer.state, EnrollState::Pending as i32);
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            requested(&key, None, "pending"),
            declined(MacDecline::SerialNotAllowed, &key)
        ]
    );

    let policy = MacPolicy {
        allowlist: MacAllowlist::parse(ALLOWED).expect("an allowlist"),
        ..MacPolicy::default()
    };
    let gateless = Rig::configured("mac-no-gate-listed", Arc::new(RealFs), os_random(), |e| {
        e.with_mac(policy)
    });
    let e = &gateless.enrollment;
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    let answer = e
        .enroll(mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes()))
        .expect("enrolled");
    assert_eq!(answer.state, EnrollState::Pending as i32);
    assert_eq!(
        e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
        MacOutcome::Declined(MacDecline::DeviceStateUnavailable)
    );
    // A request the store does not hold is refused, not approved.
    assert_eq!(
        e.mac_auto_approve(&HostKey::new().key_hash(), nonce.as_bytes()),
        MacOutcome::Refused(EnrollmentRefusal::UnknownRequest)
    );
    assert!(format!("{:?}", MacPolicy::default()).starts_with("MacPolicy"));
}

/// Catches the serial a request claims being trusted: claims equal to an allowlisted,
/// supervised Mac's, without a nonce, leave the request pending and try nothing.
#[tokio::test]
async fn claims_alone_never_approve() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let enrollment = mac_rig("mac-claims", Arc::new(RealFs), &gate, ALLOWED);
    let rig = coordinator_over("mac-claims-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    let key = HostKey::new();
    let before = audit_of(&rig.coordinator).len();
    let answer = e
        .enroll(mac_request(&key, "machine-mac", SERIAL, &[]))
        .expect("enrolled");
    assert_eq!(answer.state, EnrollState::Pending as i32);
    assert_eq!(state_of(e, &key), RequestState::Pending);
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [requested(&key, None, "pending")],
        "the request, and no approval tried"
    );
    let listed = rig
        .coordinator
        .enrollment_list(
            &rig.coordinator.caller(Some(OPERATOR)).await,
            &ListPendingRequest::default(),
        )
        .expect("listed");
    assert!(!listed.requests[0].mac_nonce_presented);
}

/// Catches a Mac approved over a host id another key holds, or over a duplicate claim: the
/// approval is refused as an operator's would be, audited by `mdm-nonce`, and the nonce is
/// kept.
#[tokio::test]
async fn a_mac_approval_is_refused_as_an_operators_would_be() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let enrollment = mac_rig("mac-refused", Arc::new(RealFs), &gate, ALLOWED);
    let rig = coordinator_over("mac-refused-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    enrolled(e, &HostKey::new(), "machine-old", MAC_HOST);
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    let key = HostKey::new();
    let before = audit_of(&rig.coordinator).len();
    e.enroll(mac_request(&key, "machine-new", SERIAL, nonce.as_bytes()))
        .expect("enrolled");
    assert_eq!(state_of(e, &key), RequestState::Pending);
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            requested(&key, None, "pending"),
            audited(
                "mdm-nonce",
                "enroll_approve_refused",
                Some(MAC_HOST),
                &format!("host_id_taken; request {}", key.key_hash())
            )
        ]
    );
    assert_eq!(
        e.mac_auto_approve(&key.key_hash(), nonce.as_bytes()),
        MacOutcome::Refused(EnrollmentRefusal::HostIdTaken),
        "the nonce was kept"
    );
    let clone = HostKey::new();
    e.enroll(mac_request(&clone, "machine-new", SERIAL, nonce.as_bytes()))
        .expect("a duplicate claim");
    assert_eq!(
        e.mac_auto_approve(&clone.key_hash(), nonce.as_bytes()),
        MacOutcome::Refused(EnrollmentRefusal::DuplicateClaim)
    );
}

/// Every 32-byte sequence a reader of `bytes` might find a nonce by: its bytes, its SHA-256,
/// and each as hex.
fn traces(nonce: &[u8]) -> Vec<Vec<u8>> {
    let hash = *MacNonceHash::of(nonce).as_bytes();
    let hex = |bytes: &[u8]| {
        KeyHash::from_bytes(bytes)
            .expect("32 bytes")
            .to_hex()
            .into_bytes()
    };
    vec![nonce.to_vec(), hash.to_vec(), hex(nonce), hex(&hash)]
}

/// Catches a nonce, or its SHA-256, in a `List` answer, an audit entry, the audit file or the
/// ops file after a Mac was declined and then approved by it.
#[tokio::test]
async fn no_secret_in_list_or_audit() {
    let gate = gate_saying(
        SERIAL,
        Ok(DeviceStatus {
            supervised: false,
            ..GOOD
        }),
    );
    let enrollment = mac_rig("mac-secret", Arc::new(RealFs), &gate, ALLOWED);
    let rig = coordinator_over("mac-secret-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    let e = &enrollment.enrollment;
    let nonce = e.issue_mac_nonce(SERIAL).expect("a nonce");
    assert_eq!(format!("{nonce:?}"), "MacNonce(..)");
    let key = HostKey::new();
    let request = mac_request(&key, "machine-mac", SERIAL, nonce.as_bytes());
    e.enroll(request.clone()).expect("declined");
    let mut seen: Vec<Vec<u8>> = vec![
        rig.coordinator
            .enrollment_list(&operator, &ListPendingRequest::default())
            .expect("listed")
            .encode_to_vec(),
    ];
    gate.set(SERIAL, Ok(GOOD));
    let answer: EnrollResponse = e.enroll(request).expect("approved");
    assert_eq!(answer.state, EnrollState::Unspecified as i32);
    let entries = rig.coordinator.ops().audit_log().expect("the audit log");
    assert!(entries.iter().any(|entry| entry.action == "enroll_approve"));
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "mac_auto_approve_declined")
    );
    seen.push(serde_json::to_vec(&entries).expect("JSON"));
    for file in ["desired.json.ops", "desired.json.ops.audit"] {
        seen.push(std::fs::read(rig.dir.path().join(file)).expect("read"));
    }
    for bytes in &seen {
        for trace in traces(nonce.as_bytes()) {
            assert!(!bytes.windows(trace.len()).any(|w| w == trace.as_slice()));
        }
    }
}

/// Catches an allowlist read past a rule: its shape, a serial, a host id it makes, a role, a
/// service, and a serial listed twice are each refused, and a missing file stops the start.
#[test]
fn the_mac_allowlist_refuses_what_breaks_its_rules() {
    let entry = |serial: &str, prefix: &str, role: &str, services: &str| {
        format!(
            "[[mac]]\nserial = \"{serial}\"\nhost_id_prefix = \"{prefix}\"\nrole = \"{role}\"\n\
             services = [{services}]\n"
        )
    };
    let good = entry(SERIAL, "mac-", "build", "\"kbf\"");
    let listed = MacAllowlist::parse(&good).expect("an allowlist");
    let found = listed.get(SERIAL).expect("listed");
    assert_eq!(found.host_id, host_id(MAC_HOST));
    assert_eq!(
        listed.services().map(|s| s.as_str()).collect::<Vec<_>>(),
        ["kbf"]
    );
    assert!(listed.get("OTHERSERIAL1").is_none());
    assert_eq!(
        MacAllowlist::parse("").expect("empty"),
        MacAllowlist::default()
    );
    for bad in [
        "mac = 1".to_owned(),
        format!("{good}extra = 1\n"),
        entry("", "mac-", "build", ""),
        entry("C02 X", "mac-", "build", ""),
        entry(&"C".repeat(65), "mac-", "build", ""),
        entry(SERIAL, "Mac-", "build", ""),
        entry(SERIAL, &"m".repeat(60), "build", ""),
        entry(SERIAL, "mac-", "1build", ""),
        entry(SERIAL, "mac-", "build-", ""),
        entry(SERIAL, "mac-", "build", "\"-kbf\""),
        format!("{good}{good}"),
    ] {
        let refused = MacAllowlist::parse(&bad).expect_err("refused");
        assert!(matches!(refused, AllowlistError::Invalid(_)), "{bad}");
        assert!(
            refused.to_string().starts_with("--mac-serial-allowlist: "),
            "{refused}"
        );
    }
    let dir = scratch("mac-allowlist");
    let path = dir.path().join("allowlist.toml");
    let refused = MacAllowlist::load(&path).expect_err("missing");
    assert!(matches!(refused, AllowlistError::Read { .. }), "{refused}");
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(good.as_bytes()))
        .expect("written");
    assert_eq!(MacAllowlist::load(&path).expect("read"), listed);
}

/// Catches a nonce issued for no serial, from no random bytes, twice from the same bytes, or
/// past a store that cannot record it.
#[test]
fn a_nonce_is_issued_only_for_a_serial_from_fresh_random_bytes() {
    let enrollment = Rig::new("mac-issue");
    let long = "C".repeat(65);
    for bad in ["", "C02 X", "C02-X", long.as_str()] {
        assert!(matches!(
            enrollment.enrollment.issue_mac_nonce(bad),
            Err(MacNonceError::InvalidSerial)
        ));
    }
    assert_eq!(
        enrollment
            .enrollment
            .issue_mac_nonce(&"C".repeat(64))
            .expect("the longest serial")
            .as_bytes()
            .len(),
        32
    );
    let none: Random = Box::new(|_| false);
    let starved = Rig::over("mac-issue-none", Arc::new(RealFs), none);
    assert!(matches!(
        starved.enrollment.issue_mac_nonce(SERIAL),
        Err(MacNonceError::NoRandom)
    ));
    let same: Random = Box::new(|buffer| {
        buffer.fill(7);
        true
    });
    let stuck = Rig::over("mac-issue-same", Arc::new(RealFs), same);
    stuck.enrollment.issue_mac_nonce(SERIAL).expect("the first");
    let again = stuck
        .enrollment
        .issue_mac_nonce(SERIAL)
        .expect_err("the same bytes");
    assert!(matches!(again, MacNonceError::NoRandom), "{again}");
    let fs = Arc::new(FaultFs::new());
    let failing = Rig::over("mac-issue-store", fs.clone(), os_random());
    fs.fail_at(fs.count() + 1);
    let refused = failing
        .enrollment
        .issue_mac_nonce(SERIAL)
        .expect_err("the lock fails");
    assert!(matches!(refused, MacNonceError::Store(_)), "{refused}");
}
