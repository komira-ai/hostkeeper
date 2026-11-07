//! A Mac nonce across a restart of the coordinator, and across a crash at the write that
//! approves and consumes it: the approval and the consumption are there together or not at
//! all.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::RequestState;
use hostkeeper_enrollment::fs::{FaultFs, Fs, Op, RealFs};
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;

use super::support::{HostKey, Rig, SERIAL, host_id, mac_request};
use crate::enrollment::{
    DeviceStatus, Enrollment, FakeDeviceState, MacAllowlist, MacDecline, MacOutcome, MacPolicy,
    os_random,
};

/// The host id the allowlist gives [`SERIAL`].
const MAC_HOST: &str = "mac-c02xk1abjg5j";

/// The policy of a gate that says every device of [`SERIAL`] is enrolled and supervised, with
/// [`SERIAL`] allowlisted.
fn policy() -> MacPolicy {
    let gate = FakeDeviceState::default();
    gate.set(
        SERIAL,
        Ok(DeviceStatus {
            enrolled: true,
            supervised: true,
            enrolled_since_unix: None,
        }),
    );
    MacPolicy {
        device_state: Arc::new(gate),
        allowlist: MacAllowlist::parse(
            "[[mac]]\nserial = \"C02XK1ABJG5J\"\nhost_id_prefix = \"mac-\"\nrole = \"build\"\n",
        )
        .expect("an allowlist"),
    }
}

/// A Mac's enrollment over `fs` with the policy of [`policy`], a nonce issued for [`SERIAL`],
/// and the key of a Mac's request: the rig, the nonce and the key.
fn mac_run(name: &str, fs: Arc<dyn Fs>) -> (Rig, Vec<u8>, HostKey) {
    let rig = Rig::configured(name, fs, os_random(), |e| e.with_mac(policy()));
    let key = HostKey::new();
    let nonce = rig
        .enrollment
        .issue_mac_nonce(SERIAL, &key.key_hash())
        .expect("a nonce")
        .as_bytes()
        .to_vec();
    (rig, nonce, key)
}

/// The state of `key`'s request in `enrollment`.
fn state_of(enrollment: &Enrollment, key: &HostKey) -> RequestState {
    enrollment
        .store()
        .request(&key.key_hash())
        .expect("read")
        .expect("held")
        .state
}

/// What another Mac request, from the machine `machine_id`, presenting `nonce` gets from
/// `enrollment`. Each try names a machine of its own, so none is a duplicate claim.
fn second_try(enrollment: &Enrollment, machine_id: &str, nonce: &[u8]) -> MacOutcome {
    let key = HostKey::new();
    enrollment
        .enroll(mac_request(&key, machine_id, SERIAL, nonce))
        .expect("enrolled");
    enrollment.mac_auto_approve(&key.key_hash(), nonce)
}

/// Whether `op` writes a file in the store's subdirectory `dir`.
fn writes_under(op: &Op, dir: &str) -> bool {
    match op {
        Op::WriteNew(path) => path.parent().is_some_and(|parent| parent.ends_with(dir)),
        _ => false,
    }
}

/// The numbers, counted from 1 from `base`, of the approval's write (the last request write
/// before the nonce's) and of the nonce's mark (its first write) among `ops`.
fn commit_and_mark(ops: &[Op], base: u64) -> (u64, u64) {
    let skip = usize::try_from(base).expect("small");
    let after = &ops[skip..];
    let mark = after
        .iter()
        .position(|op| writes_under(op, "mac-nonces"))
        .expect("the nonce is marked");
    let commit = after[..mark]
        .iter()
        .rposition(|op| writes_under(op, "requests"))
        .expect("the approval is written");
    let number = |at: usize| u64::try_from(at).expect("small") + 1;
    (number(commit), number(mark))
}

/// Catches a nonce consumed in a write of its own, or forgotten at a restart: after a restart
/// a used nonce declines `nonce_used` and the first host stays approved; a crash at the
/// approval's write leaves the request pending and the nonce good, a crash at the nonce's own
/// mark leaves the approval and the consumption, both read so by the restarted coordinator.
#[test]
fn a_nonce_is_consumed_with_the_approval_across_a_restart() {
    let (rig, nonce, key) = mac_run("mac-restart", Arc::new(RealFs));
    rig.enrollment
        .enroll(mac_request(&key, "machine-mac", SERIAL, &nonce))
        .expect("enrolled");
    assert_eq!(state_of(&rig.enrollment, &key), RequestState::Approved);
    let restarted = rig.restart_with(|e| e.with_mac(policy()));
    assert_eq!(
        second_try(&restarted, "machine-restarted", &nonce),
        MacOutcome::Declined(MacDecline::NonceUsed)
    );
    assert_eq!(state_of(&restarted, &key), RequestState::Approved);
    assert!(
        restarted
            .store()
            .host_role(&host_id(MAC_HOST))
            .expect("read")
            .is_some()
    );

    // Where the approval is written, and where the nonce is marked, in one `Enroll`.
    let probe_fs = Arc::new(FaultFs::new());
    let (probe, probe_nonce, probe_key) = mac_run("mac-crash-probe", probe_fs.clone());
    let base = probe_fs.count();
    probe
        .enrollment
        .enroll(mac_request(&probe_key, "machine-mac", SERIAL, &probe_nonce))
        .expect("enrolled");
    assert_eq!(
        state_of(&probe.enrollment, &probe_key),
        RequestState::Approved
    );
    let (commit, mark) = commit_and_mark(&probe_fs.ops(), base);
    assert!(commit < mark);

    // The approval's write fails: the request stays pending and the nonce good, then and
    // after a restart, where it approves.
    let fs = Arc::new(FaultFs::new());
    let (run, nonce, key) = mac_run("mac-crash-commit", fs.clone());
    fs.fail_at(fs.count() + commit);
    let answer = run
        .enrollment
        .enroll(mac_request(&key, "machine-mac", SERIAL, &nonce))
        .expect("answered");
    assert_eq!(answer.state, EnrollState::Pending as i32);
    let restarted = run.restart_with(|e| e.with_mac(policy()));
    assert_eq!(state_of(&restarted, &key), RequestState::Pending);
    let outcome = restarted.mac_auto_approve(&key.key_hash(), &nonce);
    assert!(
        matches!(&outcome, MacOutcome::Approved(entry) if entry.host_id == host_id(MAC_HOST)),
        "{outcome:?}"
    );

    // The nonce's mark fails: the approval stands and the nonce is used, then and after a
    // restart.
    let fs = Arc::new(FaultFs::new());
    let (run, nonce, key) = mac_run("mac-crash-mark", fs.clone());
    fs.fail_at(fs.count() + mark);
    run.enrollment
        .enroll(mac_request(&key, "machine-mac", SERIAL, &nonce))
        .expect("answered");
    assert_eq!(state_of(&run.enrollment, &key), RequestState::Approved);
    assert_eq!(
        second_try(&run.enrollment, "machine-before-restart", &nonce),
        MacOutcome::Declined(MacDecline::NonceUsed)
    );
    let restarted = run.restart_with(|e| e.with_mac(policy()));
    assert_eq!(state_of(&restarted, &key), RequestState::Approved);
    assert_eq!(
        second_try(&restarted, "machine-restarted", &nonce),
        MacOutcome::Declined(MacDecline::NonceUsed)
    );
}
