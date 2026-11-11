//! The Mac path's binding and its two consequences (docs/design/hostkeeper-mdm.md, sections
//! 5.1 and 5.2): a nonce approves only the request it was issued for; a Mac wiped after its
//! approval takes its own host id over from its earlier key, and only then; and a nonce
//! approval assigns its allowlist entry's services as an operator's approval does.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::Role;
use hostkeeper_proto::hostkeeper::agent::v1::{EnrollState, PollState};

use super::mac::{ALLOWED, GOOD, MAC_HOST, declined, gate_saying, mac_rig, state_of};
use super::support::{
    Audited, HostKey, NOW, SERIAL, audit_after, audit_of, audited, coordinator_over, host_id,
    mac_request, requested, signed_poll,
};
use crate::assignment::Entry;
use crate::enrollment::{DeviceStatus, Enrollment, MacDecline, MacOutcome};

/// The status of a device enrolled and supervised whose current MDM enrollment began at
/// `since`.
fn enrolled_since(since: Option<u64>) -> DeviceStatus {
    DeviceStatus {
        enrolled_since_unix: since,
        enrolled_via_dep: None,
        ..GOOD
    }
}

/// Posts `key`'s Mac request from `machine` through the coordinator, with a nonce issued for
/// [`SERIAL`] and that request: the answer's state.
fn enroll_by_nonce(
    rig: &crate::testutil::Rig,
    e: &Enrollment,
    key: &HostKey,
    machine: &str,
) -> i32 {
    let nonce = e.issue_mac_nonce(SERIAL, &key.key_hash()).expect("a nonce");
    rig.coordinator
        .enroll(e, mac_request(key, machine, SERIAL, nonce.as_bytes()))
        .expect("enrolled")
        .state
}

/// The audit entry of `key`'s approval as [`MAC_HOST`] by its nonce, from `machine`, recorded
/// as `action`.
fn approved_by_nonce(action: &str, key: &HostKey, machine: &str) -> Audited {
    audited(
        "mdm-nonce",
        action,
        Some(MAC_HOST),
        &format!(
            "request {}; role build; services kbf; claims machine_id={machine} \
             mac_serial={SERIAL} platform_uuid=4C4C4544-0042-5A10-8048-B7C04F4E3332; \
             approved by its MDM nonce",
            key.key_hash()
        ),
    )
}

/// The audit entry of the assignment of `kbf` to [`MAC_HOST`] by the nonce's approver.
fn assigned_by_nonce() -> Audited {
    audited(
        "mdm-nonce",
        "assign",
        Some(MAC_HOST),
        &format!("host {MAC_HOST}"),
    )
}

/// Catches a nonce that approves whichever request presents it: a local user's request with
/// the daemon's nonce is declined `nonce_wrong_request`, stays pending and leaves the nonce
/// unused, the daemon's own request is then approved by it, and the local request presenting
/// it again is `nonce_used`.
#[tokio::test]
async fn a_nonce_approves_only_the_request_it_was_bound_to() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let enrollment = mac_rig("mac-bound", Arc::new(RealFs), &gate, ALLOWED);
    let rig = coordinator_over("mac-bound-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;
    let (daemon, local) = (HostKey::new(), HostKey::new());
    let nonce = e
        .issue_mac_nonce(SERIAL, &daemon.key_hash())
        .expect("a nonce");
    let before = audit_of(&rig.coordinator).len();
    let theirs = rig
        .coordinator
        .enroll(
            e,
            mac_request(&local, "machine-local", SERIAL, nonce.as_bytes()),
        )
        .expect("enrolled");
    assert_eq!(theirs.state, EnrollState::Pending as i32);
    assert_eq!(state_of(e, &local), RequestState::Pending);
    assert_eq!(MacDecline::NonceWrongRequest.token(), "nonce_wrong_request");
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            requested(&local, None, "pending"),
            declined(MacDecline::NonceWrongRequest, &local)
        ]
    );
    let ours = rig
        .coordinator
        .enroll(
            e,
            mac_request(&daemon, "machine-daemon", SERIAL, nonce.as_bytes()),
        )
        .expect("enrolled");
    assert_eq!(ours.state, EnrollState::Unspecified as i32);
    assert_eq!(state_of(e, &daemon), RequestState::Approved);
    assert_eq!(
        e.mac_auto_approve(&local.key_hash(), nonce.as_bytes()),
        MacOutcome::Declined(MacDecline::NonceUsed)
    );
    assert_eq!(state_of(e, &local), RequestState::Pending);
}

/// One way a wiped Mac's new key asks for its own host id: what it changes from the case where
/// every condition of the takeover holds.
#[derive(Clone, Copy, Debug)]
enum Wipe {
    /// The gate saw the new enrollment begin one second after the old key's approval.
    Wiped,
    /// The enrollment began in the second of the old key's approval: no new enrollment since.
    NotSince,
    /// The gate does not say when the enrollment began.
    Unknown,
    /// The host id was approved for a request that did not claim the Mac's serial.
    OtherSerial,
    /// The host id is denied.
    Denied,
}

/// Catches a wiped Mac left waiting for an operator, and a host id taken over when any
/// condition of the takeover is false: the new key's nonce approval takes [`MAC_HOST`] over
/// from the old key (audited `enroll_replace`, the old key's certificate naming no host after
/// it, the services assigned again) only when the gate saw the device enroll anew after the old
/// approval and the old key's own request claimed the same serial; otherwise it is refused
/// `host_id_taken` (`denied` for a denied host id), and the old key keeps the host id.
#[tokio::test]
async fn a_wiped_mac_replaces_its_own_host_id() {
    let rows = [
        (Wipe::Wiped, None),
        (Wipe::NotSince, Some("host_id_taken")),
        (Wipe::Unknown, Some("host_id_taken")),
        (Wipe::OtherSerial, Some("host_id_taken")),
        (Wipe::Denied, Some("denied")),
    ];
    for (n, (row, refusal)) in rows.into_iter().enumerate() {
        let gate = gate_saying(SERIAL, Ok(enrolled_since(Some(NOW - 600))));
        let name = format!("mac-wiped-{n}");
        let enrollment = mac_rig(&name, Arc::new(RealFs), &gate, ALLOWED);
        let rig = coordinator_over(&format!("{name}-coordinator"), &enrollment).await;
        let e = &enrollment.enrollment;
        let old = HostKey::new();
        match row {
            Wipe::OtherSerial => {
                e.enroll(mac_request(&old, "machine-old", "C02OTHER0001", &[]))
                    .expect("enrolled");
                super::support::approve(e, &old, MAC_HOST, false);
            }
            _ => {
                let state = enroll_by_nonce(&rig, e, &old, "machine-old");
                assert_eq!(state, EnrollState::Unspecified as i32, "{row:?}");
            }
        }
        let old_certificate = signed_poll(e, &old, &old).expect("polled");
        assert_eq!(old_certificate.state, PollState::Issued as i32);
        if let Wipe::Denied = row {
            e.store().deny(&host_id(MAC_HOST)).expect("denied");
        }
        // The Mac is wiped: a new MDM enrollment, then a new key.
        let since = match row {
            Wipe::NotSince => Some(NOW),
            Wipe::Unknown => None,
            _ => Some(NOW + 1),
        };
        enrollment.clock.set(NOW + 900);
        gate.set(SERIAL, Ok(enrolled_since(since)));
        let new = HostKey::new();
        let before = audit_of(&rig.coordinator).len();
        let state = enroll_by_nonce(&rig, e, &new, "machine-new");
        let holds = |key: &HostKey| {
            e.store()
                .host_role_for(&host_id(MAC_HOST), &key.key_hash())
                .expect("read")
        };
        match refusal {
            None => {
                assert_eq!(state, EnrollState::Unspecified as i32);
                assert_eq!(state_of(e, &new), RequestState::Approved);
                assert_eq!(
                    audit_after(&rig.coordinator, before),
                    [
                        requested(&new, None, "pending"),
                        approved_by_nonce("enroll_replace", &new, "machine-new"),
                        assigned_by_nonce(),
                    ]
                );
                assert_eq!(holds(&new), Some(Role::parse("build").expect("a role")));
                assert_eq!(holds(&old), None);
                // The old key's certificate names no host, as after an operator's `replace`.
                assert!(
                    rig.coordinator
                        .identify(&old_certificate.certificate_der)
                        .is_err()
                );
                let issued = signed_poll(e, &new, &new).expect("polled");
                assert_eq!(
                    (issued.state, issued.host_id.as_str()),
                    (PollState::Issued as i32, MAC_HOST)
                );
            }
            Some(token) => {
                assert_eq!(state, EnrollState::Pending as i32, "{row:?}");
                assert_eq!(state_of(e, &new), RequestState::Pending, "{row:?}");
                assert_eq!(
                    audit_after(&rig.coordinator, before),
                    [
                        requested(&new, None, "pending"),
                        audited(
                            "mdm-nonce",
                            "enroll_approve_refused",
                            Some(MAC_HOST),
                            &format!("{token}; request {}", new.key_hash())
                        ),
                    ],
                    "{row:?}"
                );
                assert_eq!(holds(&new), None, "{row:?}");
                assert!(holds(&old).is_some(), "{row:?}");
            }
        }
    }
}

/// The allowlist of [`SERIAL`] naming a service the coordinator does not run.
const UNKNOWN_SERVICE: &str = r#"
[[mac]]
serial = "C02XK1ABJG5J"
host_id_prefix = "mac-"
role = "build"
services = ["ghost"]
"#;

/// Catches a nonce approval that records the entry's services without assigning them, assigns
/// them under another name than the approver's, assigns on a declined attempt, or undoes the
/// approval when the assignment is refused: the approval is followed by one `assign` entry by
/// `mdm-nonce` per service and the host is assigned `kbf` as an operator's approval assigns it;
/// a declined attempt assigns nothing; and an entry whose service the coordinator does not run
/// is approved with nothing assigned.
#[tokio::test]
async fn a_nonce_approval_assigns_the_entry_services() {
    let gate = gate_saying(SERIAL, Ok(GOOD));
    let enrollment = mac_rig("mac-assign", Arc::new(RealFs), &gate, ALLOWED);
    let rig = coordinator_over("mac-assign-coordinator", &enrollment).await;
    let e = &enrollment.enrollment;

    // A declined attempt: the nonce is another request's.
    let stranger = HostKey::new();
    let nonce = e
        .issue_mac_nonce(SERIAL, &HostKey::new().key_hash())
        .expect("a nonce");
    let before = audit_of(&rig.coordinator).len();
    rig.coordinator
        .enroll(
            e,
            mac_request(&stranger, "machine-stranger", SERIAL, nonce.as_bytes()),
        )
        .expect("enrolled");
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            requested(&stranger, None, "pending"),
            declined(MacDecline::NonceWrongRequest, &stranger)
        ]
    );
    let assignments = rig.coordinator.ops().state().assignments;
    assert_eq!(
        assignments
            .get("kbf")
            .and_then(|assigned| assigned.hosts.get(MAC_HOST)),
        None
    );

    let key = HostKey::new();
    let before = audit_of(&rig.coordinator).len();
    let state = enroll_by_nonce(&rig, e, &key, "machine-mac");
    assert_eq!(state, EnrollState::Unspecified as i32);
    assert_eq!(
        audit_after(&rig.coordinator, before),
        [
            requested(&key, None, "pending"),
            approved_by_nonce("enroll_approve", &key, "machine-mac"),
            assigned_by_nonce(),
        ]
    );
    let assignments = rig.coordinator.ops().state().assignments;
    assert_eq!(
        assignments["kbf"].hosts.get(MAC_HOST),
        Some(&Entry::Include)
    );

    let ghost = mac_rig("mac-assign-ghost", Arc::new(RealFs), &gate, UNKNOWN_SERVICE);
    let rig = coordinator_over("mac-assign-ghost-coordinator", &ghost).await;
    let e = &ghost.enrollment;
    let key = HostKey::new();
    let before = audit_of(&rig.coordinator).len();
    let state = enroll_by_nonce(&rig, e, &key, "machine-mac");
    assert_eq!(state, EnrollState::Unspecified as i32);
    assert_eq!(state_of(e, &key), RequestState::Approved);
    let actions: Vec<String> = audit_after(&rig.coordinator, before)
        .into_iter()
        .map(|entry| entry.1)
        .collect();
    assert_eq!(actions, ["enroll_requested", "enroll_approve"]);
    assert!(
        !rig.coordinator
            .ops()
            .state()
            .assignments
            .contains_key("ghost")
    );
}
