//! The SSH access a heartbeat is answered with and the report it carries ([`crate::ssh_heartbeat`]):
//! what each class of host is sent, when it is sent again, and what is kept of a report. The
//! answer over gRPC and the operator's view are in `grpc/tests/ssh_heartbeat.rs`.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::RetireRequest;

use super::{FEATURE, MAX_DETAIL, STATES, SshReports, access_of, generation, kept};
use crate::first_serving_tests::{APPROVED, FIRST, Fixture, WEB, daemon};
use crate::marks_support::{operator, runs, who};
use crate::ssh_ledger::{Issue, Ledger, LedgerError, Revocation};
use crate::ssh_sign::tests::{NOW, OPERATORS, Tune, World, world};
use crate::testutil::{OPERATOR, id};

/// A fingerprint of the form `ssh-keygen -l` prints, 43 characters after `SHA256:`.
const FP: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// A heartbeat over gRPC that announces `features` and carries `report`.
fn said(features: &[&str], report: Option<agent::SshReport>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: features.iter().map(|f| (*f).to_owned()).collect(),
        ssh: report,
        ..agent::HeartbeatRequest::default()
    }
}

/// A heartbeat of a daemon that announces the feature and carries `report`.
fn announcing(report: Option<agent::SshReport>) -> agent::HeartbeatRequest {
    said(&["heartbeat-v2", FEATURE], report)
}

/// A report that holds `generation` applied.
fn applied(generation: u64) -> agent::SshReport {
    agent::SshReport {
        applied_generation: generation,
        state: "applied".to_owned(),
        ..agent::SshReport::default()
    }
}

/// A coordinator that signs, as `world` makes it.
fn signing(name: &str) -> World {
    world(name, Some(OPERATORS), Tune::default())
}

/// What the coordinator of `world` sends the host `host` that sends `request`.
async fn sent(
    world: &World,
    host: &str,
    request: &agent::HeartbeatRequest,
) -> Option<agent::SshAccess> {
    world
        .coordinator
        .ssh_access_for(&who(host, "daemon"), request)
        .await
}

/// A certificate for `host` of the ledger of `world`, revoked: its serial.
fn revoked(world: &World, host: &str) -> u64 {
    let issued = world
        .ledger
        .issue(&Issue {
            operator: "operator",
            host_id: host,
            fingerprint: "SHA256:x",
            ttl_secs: 3600,
            reason: "",
            api_token: None,
        })
        .expect("signed");
    world
        .ledger
        .revoke("operator", Revocation::Serial(issued.serial))
        .expect("revoked");
    issued.serial
}

/// What the status counts of dropped SSH reports.
fn dropped(world: &World) -> Option<u64> {
    let status = world.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("ssh").copied()
}

// ---- the feature ----------------------------------------------------------------------------

/// Proves: the feature is advertised by name, so a daemon's announcement is agreed.
///
/// Catches: a feature acted on and not advertised, which no daemon would then announce.
#[test]
fn the_feature_is_advertised() {
    assert_eq!(FEATURE, "ssh-v1");
    assert!(crate::features::FEATURES.contains(&FEATURE));
}

/// Catches: `ssh-v1` advertised and not acted on: a daemon that announces it is sent its access,
/// and the same daemon that does not announce it is sent none.
pub(crate) async fn ssh_v1_is_acted_on() {
    let world = signing("ssh-heartbeat-acted-on");
    assert!(sent(&world, "node-07", &announcing(None)).await.is_some());
    assert_eq!(
        sent(&world, "node-07", &said(&["heartbeat-v2"], None)).await,
        None
    );
}

/// Proves: nothing is sent unless the daemon announces the feature, this coordinator advertises
/// it, and the coordinator holds an SSH user CA.
///
/// Catches: an access sent to a daemon that did not ask (which reads the answer strictly), one
/// sent when the feature is not advertised, and one sent by a coordinator with no CA.
#[tokio::test]
async fn nothing_is_sent_without_the_feature_on_both_sides_and_a_ca() {
    let mut world = signing("ssh-heartbeat-gates");
    assert!(sent(&world, "node-07", &announcing(None)).await.is_some());
    assert_eq!(
        sent(&world, "node-07", &said(&["heartbeat-v2"], None)).await,
        None
    );
    world.coordinator = world.coordinator.with_features(&["heartbeat-v2"]);
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);
    let mut world = signing("ssh-heartbeat-no-ca");
    world.coordinator.ssh = None;
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);
}

// ---- what a host is sent --------------------------------------------------------------------

/// Proves: a fleet host is sent both keys of the CA secret, current first, its own principal, the
/// Linux login user, `keep`, and the serials of its revoked certificates that have not expired,
/// and not another host's; the access is enabled.
///
/// Catches: the control-plane key sent to a fleet host, a missing second key (a rotation that
/// locks hosts out), another host's serial, and a principal that is not the host's.
#[tokio::test]
async fn a_fleet_host_is_sent_both_keys_its_principal_login_policy_and_serials() {
    let world = signing("ssh-heartbeat-fleet");
    let own = revoked(&world, "node-07");
    let _other = revoked(&world, "node-08");
    let access = sent(&world, "node-07", &announcing(None))
        .await
        .expect("an access");
    assert!(access.enabled);
    assert_eq!(access.ca_public_keys, world.fleet);
    assert_eq!(access.principal, "hk-host-node-07");
    assert_eq!(access.login_user, "root");
    assert_eq!(access.password_auth, "keep");
    assert_eq!(access.revoked_serials, vec![own]);
    assert_ne!(access.generation, 0);
}

/// Proves: a Mac, by its enrollment's serial number, is sent the Macs' login user.
///
/// Catches: the Linux account sent to a Mac.
#[test]
fn a_mac_is_sent_the_macs_login_user() {
    let world = signing("ssh-heartbeat-mac");
    let mut host = world
        .coordinator
        .enrolled_host("node-07")
        .expect("read")
        .expect("enrolled");
    let ssh = world.coordinator.ssh.as_ref().expect("ssh");
    assert_eq!(
        access_of(ssh, "node-07", &host, Vec::new()).login_user,
        "root"
    );
    host.claims.mac_serial = "C02ABC".to_owned();
    assert_eq!(
        access_of(ssh, "node-07", &host, Vec::new()).login_user,
        "admin"
    );
}

/// Proves: `--ssh-password-auth off` is what every host is sent as its password policy.
///
/// Catches: a flag that is read and not sent.
#[tokio::test]
async fn the_password_policy_of_the_flag_is_sent() {
    let mut world = signing("ssh-heartbeat-password");
    world.coordinator.ssh.as_mut().expect("ssh").password_auth = crate::config::PasswordAuth::Off;
    let access = sent(&world, "node-07", &announcing(None))
        .await
        .expect("an access");
    assert_eq!(access.password_auth, "off");
}

/// Proves: a control-plane host is sent only the control-plane key: the public key of the file
/// when it was given, else the public half of the private key the coordinator holds; and with
/// neither it is sent `enabled = false` and no key, which reverts what its daemon set up.
///
/// Catches: the fleet's keys sent to the control plane (a stolen coordinator would then be root
/// there), and a control-plane host left trusting a CA the coordinator no longer names.
#[tokio::test]
async fn a_control_plane_host_is_sent_only_the_control_plane_key_or_the_revert() {
    let mut world = signing("ssh-heartbeat-cp");
    let access = sent(&world, "nuc", &announcing(None))
        .await
        .expect("access");
    assert!(access.enabled);
    assert_eq!(access.ca_public_keys, vec![world.control_plane.clone()]);
    assert_eq!(access.principal, "hk-host-nuc");

    let file = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPublicOfTheFile".to_owned();
    world
        .coordinator
        .ssh
        .as_mut()
        .expect("ssh")
        .control_plane_public = Some(file.clone());
    let access = sent(&world, "nuc", &announcing(None))
        .await
        .expect("access");
    assert_eq!(access.ca_public_keys, vec![file]);

    let none = world_without_cp_key();
    let access = sent(&none, "nuc", &announcing(None)).await.expect("access");
    assert!(!access.enabled);
    assert!(access.ca_public_keys.is_empty());
    let fleet = sent(&none, "node-07", &announcing(None))
        .await
        .expect("access");
    assert!(fleet.enabled, "the fleet is not reverted");
}

fn world_without_cp_key() -> World {
    world(
        "ssh-heartbeat-no-cp-key",
        Some(OPERATORS),
        Tune {
            no_control_plane_key: true,
            ..Tune::default()
        },
    )
}

// ---- the generation -------------------------------------------------------------------------

/// A fixed access, as the pinned value of the generation reads it.
fn pinned() -> agent::SshAccess {
    agent::SshAccess {
        generation: 0,
        enabled: true,
        ca_public_keys: vec![
            "ssh-ed25519 AAAA1".to_owned(),
            "ssh-ed25519 AAAA2".to_owned(),
        ],
        login_user: "root".to_owned(),
        principal: "hk-host-node-07".to_owned(),
        password_auth: "keep".to_owned(),
        revoked_serials: vec![3, 9],
    }
}

/// Proves: the generation of a fixed access is the first eight bytes, big-endian, of the SHA-256
/// of each field with its length in eight big-endian bytes in front, by a value made outside this
/// code; it does not depend on the access' own generation field; and a change of any one field
/// (`enabled`, a key, the order of the keys, the login user, the principal, the policy, a serial)
/// changes it.
///
/// Catches: a generation that changes with the code (every host would apply its access again),
/// one that ignores a field (a host would not be sent the change), and fields that run together.
#[test]
fn the_generation_is_pinned_and_changes_with_every_field() {
    let base = pinned();
    assert_eq!(generation(&base), 16_467_899_284_743_012_834);
    let mut same = base.clone();
    same.generation = 77;
    assert_eq!(generation(&same), generation(&base));
    let changes: [fn(&mut agent::SshAccess); 10] = [
        |a| a.enabled = false,
        |a| {
            a.ca_public_keys.pop();
        },
        |a| a.ca_public_keys.reverse(),
        |a| a.ca_public_keys[1].push('x'),
        |a| a.login_user = "ops".to_owned(),
        |a| a.principal = "hk-host-node-08".to_owned(),
        |a| a.password_auth = "off".to_owned(),
        |a| a.revoked_serials.push(10),
        |a| a.revoked_serials = vec![3],
        // The boundary between two texts is part of the encoding.
        |a| {
            a.login_user = "roo".to_owned();
            a.principal = "thk-host-node-07".to_owned();
        },
    ];
    for change in changes {
        let mut other = base.clone();
        change(&mut other);
        assert_ne!(generation(&other), generation(&base), "{other:?}");
    }
}

/// Proves: two coordinators built from the same configuration send a host the same generation, so
/// the instances of one deployment and a restarted instance agree.
///
/// Catches: a generation that counts in memory, which two instances would not share and a restart
/// would lower.
#[tokio::test]
async fn the_generation_is_the_same_on_two_coordinators_of_one_configuration() {
    let one = signing("ssh-heartbeat-same-one");
    let two = signing("ssh-heartbeat-same-two");
    let (a, b) = (
        sent(&one, "node-07", &announcing(None)).await.expect("a"),
        sent(&two, "node-07", &announcing(None)).await.expect("b"),
    );
    assert_eq!(a, b);
    let other = sent(&two, "node-08", &announcing(None)).await.expect("c");
    assert_ne!(other.generation, a.generation, "the principal differs");
}

/// Proves: the access is sent in every answer until the host's last report holds its generation
/// (applied or refused), a report of an older generation or none leaves it sent, and a revocation
/// or a changed password policy sends it again with a new generation.
///
/// Catches: an access sent once and lost with the answer, one sent for ever, and a change that a
/// host that applied the earlier one is never told.
#[tokio::test]
async fn the_access_is_sent_until_the_host_reports_its_generation_and_again_after_a_change() {
    let mut world = signing("ssh-heartbeat-until");
    let id7 = id("node-07");
    let first = sent(&world, "node-07", &announcing(None))
        .await
        .expect("sent");
    assert_eq!(
        sent(&world, "node-07", &announcing(None)).await,
        Some(first.clone()),
        "sent again while no report holds it"
    );
    world
        .coordinator
        .hear_ssh_report(&who("node-07", "daemon"), &announcing(Some(applied(1))));
    assert_eq!(
        sent(&world, "node-07", &announcing(None)).await,
        Some(first.clone()),
        "an older generation does not hold it"
    );
    world.coordinator.hear_ssh_report(
        &who("node-07", "daemon"),
        &announcing(Some(applied(first.generation))),
    );
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);
    assert!(world.coordinator.ssh_reports.report_of(&id7).is_some());

    let serial = revoked(&world, "node-07");
    let second = sent(&world, "node-07", &announcing(None))
        .await
        .expect("again");
    assert_eq!(second.revoked_serials, vec![serial]);
    assert_ne!(second.generation, first.generation);
    world.coordinator.hear_ssh_report(
        &who("node-07", "daemon"),
        &announcing(Some(applied(second.generation))),
    );
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);

    world.coordinator.ssh.as_mut().expect("ssh").password_auth = crate::config::PasswordAuth::Off;
    let third = sent(&world, "node-07", &announcing(None))
        .await
        .expect("again");
    assert_eq!(third.password_auth, "off");
    assert_ne!(third.generation, second.generation);
    // Another host's report does not hold this one's access.
    assert!(sent(&world, "node-08", &announcing(None)).await.is_some());
}

/// Proves: a revoked certificate that has expired by the ledger's clock is no longer sent, and the
/// generation changes with it.
///
/// Catches: a serial list that grows for ever.
#[tokio::test]
async fn an_expired_certificates_serial_leaves_the_list() {
    let world = signing("ssh-heartbeat-expired");
    let serial = revoked(&world, "node-07");
    let with = sent(&world, "node-07", &announcing(None))
        .await
        .expect("sent");
    assert_eq!(with.revoked_serials, vec![serial]);
    world.ledger.set_clock(NOW + 3601);
    let without = sent(&world, "node-07", &announcing(None))
        .await
        .expect("sent");
    assert!(without.revoked_serials.is_empty());
    assert_ne!(without.generation, with.generation);
}

// ---- what goes wrong ------------------------------------------------------------------------

/// A ledger that panics when it is read.
struct Panicking;

impl Ledger for Panicking {
    fn issue(&self, _: &Issue<'_>) -> Result<crate::ssh_ledger::Issued, LedgerError> {
        Err(LedgerError::Unwritten(String::new()))
    }
    fn revoke(&self, _: &str, _: Revocation<'_>) -> Result<u64, LedgerError> {
        Err(LedgerError::Unwritten(String::new()))
    }
    fn denied(&self, _: &str, _: &str, _: &str) -> Result<(), LedgerError> {
        Err(LedgerError::Unwritten(String::new()))
    }
    fn revoked_serials(&self, _: &str) -> Result<Vec<u64>, LedgerError> {
        panic!("the ledger panicked");
    }
}

/// Proves: when the ledger cannot be read (or its read fails outright), nothing is sent for that
/// heartbeat, and the next one that can read it is sent the access.
///
/// Catches: an access sent without the revocations (a host that then trusts a revoked
/// certificate), and a heartbeat that fails because the ledger is down.
#[tokio::test]
async fn a_ledger_failure_sends_nothing_for_that_heartbeat() {
    let mut world = signing("ssh-heartbeat-ledger-down");
    world.ledger.refuse_next(1);
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);
    assert!(sent(&world, "node-07", &announcing(None)).await.is_some());
    world.coordinator.ssh.as_mut().expect("ssh").ledger = Arc::new(Panicking);
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);
}

/// Proves: a host that is not enrolled, and one whose record cannot be read, are sent nothing for
/// that heartbeat; the second is sent the access at the next one.
///
/// Catches: an access made for a host with no record (and so no known class or account).
#[tokio::test]
async fn a_host_that_is_not_enrolled_or_cannot_be_read_is_sent_nothing() {
    let world = signing("ssh-heartbeat-unread");
    assert_eq!(sent(&world, "ghost", &announcing(None)).await, None);
    world.records.fail_at(world.records.count() + 1);
    assert_eq!(sent(&world, "node-07", &announcing(None)).await, None);
    assert!(sent(&world, "node-07", &announcing(None)).await.is_some());
}

// ---- the report -----------------------------------------------------------------------------

/// A report that keeps every rule, in full.
fn full() -> agent::SshReport {
    agent::SshReport {
        applied_generation: 5,
        state: "needs_full_disk_access".to_owned(),
        host_key_fingerprints: vec![FP.to_owned(); 8],
        detail: "keyword".to_owned(),
    }
}

/// Proves: a report that keeps the rules is kept as it is written: each state of the closed set,
/// eight fingerprints and a detail of exactly 128 bytes.
///
/// Catches: a valid report changed or refused on its way in.
#[test]
fn a_report_that_keeps_the_rules_is_kept_as_it_is() {
    for state in STATES {
        let report = agent::SshReport {
            state: (*state).to_owned(),
            ..full()
        };
        assert_eq!(kept(&report), Some(report.clone()), "{state}");
    }
    let edge = agent::SshReport {
        detail: "d".repeat(128),
        ..full()
    };
    assert_eq!(kept(&edge), Some(edge.clone()));
    assert_eq!(
        kept(&agent::SshReport::default()),
        Some(agent::SshReport {
            state: "other".to_owned(),
            ..agent::SshReport::default()
        })
    );
}

/// Proves: a state outside the closed set, of at most 32 bytes, is kept as `other`.
///
/// Catches: a daemon's newer state kept as it is, or the whole report refused for it.
#[test]
fn an_unknown_state_is_kept_as_other() {
    let report = agent::SshReport {
        state: "a_newer_state".to_owned(),
        ..full()
    };
    let held = kept(&report).expect("kept");
    assert_eq!(held.state, "other");
    assert_eq!(held.applied_generation, 5);
    let longest = agent::SshReport {
        state: "x".repeat(32),
        ..full()
    };
    assert_eq!(kept(&longest).expect("kept").state, "other");
}

/// Proves: a detail over 128 bytes is cut to 128 at a character boundary, the rest of the report
/// kept.
///
/// Catches: a cut inside a character (which would not be text) and a report refused for a long
/// detail.
#[test]
fn the_detail_is_cut_at_128_bytes_on_a_character_boundary() {
    let long = agent::SshReport {
        detail: "d".repeat(300),
        ..full()
    };
    assert_eq!(kept(&long).expect("kept").detail, "d".repeat(MAX_DETAIL));
    let wide = agent::SshReport {
        detail: format!("{}é", "d".repeat(127)),
        ..full()
    };
    assert_eq!(kept(&wide).expect("kept").detail, "d".repeat(127));
}

/// Proves: a report breaks a rule, and is dropped, when its state is over 32 bytes, it lists nine
/// fingerprints, or one of them is not `SHA256:` and 43 base64 characters.
///
/// Catches: a report kept that the proto says is dropped, and a bound that is off by one.
#[test]
fn a_report_that_breaks_a_rule_is_dropped() {
    let bad = |report: agent::SshReport| assert_eq!(kept(&report), None, "{report:?}");
    bad(agent::SshReport {
        state: "x".repeat(33),
        ..full()
    });
    bad(agent::SshReport {
        host_key_fingerprints: vec![FP.to_owned(); 9],
        ..full()
    });
    for fingerprint in [
        "",
        "SHA256:",
        "MD5:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        &FP[..FP.len() - 1],
        &format!("{FP}A"),
        "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    ] {
        bad(agent::SshReport {
            host_key_fingerprints: vec![FP.to_owned(), fingerprint.to_owned()],
            ..full()
        });
    }
}

/// Proves: the last report of a daemon that announces the feature is kept; a daemon that does not
/// announce it is ignored; a report that carries none counts as the empty one; and one that
/// breaks a rule is dropped and counted, and the one read before is kept.
///
/// Catches: a report kept for a daemon that did not announce the feature, a bad report that
/// replaces a good one, and a drop that is not counted.
#[test]
fn the_last_report_is_kept_and_a_bad_one_is_dropped_counted_and_the_last_one_kept() {
    let world = signing("ssh-heartbeat-reports");
    let host = who("node-07", "daemon");
    let reports = &world.coordinator.ssh_reports;
    let id7 = id("node-07");
    world
        .coordinator
        .hear_ssh_report(&host, &said(&["heartbeat-v2"], Some(full())));
    assert_eq!(reports.report_of(&id7), None, "it did not announce");
    world
        .coordinator
        .hear_ssh_report(&host, &announcing(Some(full())));
    assert_eq!(reports.report_of(&id7), Some(full()));
    assert_eq!(dropped(&world), None);
    let bad = agent::SshReport {
        state: "x".repeat(33),
        ..full()
    };
    world
        .coordinator
        .hear_ssh_report(&host, &announcing(Some(bad)));
    assert_eq!(
        reports.report_of(&id7),
        Some(full()),
        "the last one is kept"
    );
    assert_eq!(dropped(&world), Some(1));
    world.coordinator.hear_ssh_report(&host, &announcing(None));
    assert_eq!(
        reports.report_of(&id7),
        Some(agent::SshReport::default()),
        "a daemon that announced the feature and sent no report holds nothing"
    );
    assert_eq!(reports.report_of(&id("node-08")), None);
}

/// Proves: a forgotten host has no report, and the others keep theirs.
///
/// Catches: a forget that clears every host.
#[test]
fn a_forgotten_host_has_no_report() {
    let reports = SshReports::default();
    reports.note(&id("a"), full());
    reports.note(&id("b"), full());
    reports.forget(&id("a"));
    assert_eq!(reports.report_of(&id("a")), None);
    assert_eq!(reports.report_of(&id("b")), Some(full()));
}

/// Proves: the report of a host an operator retires is forgotten with the host, and so is that of
/// a retiring host whose final report frees its slot; the report of a host that stays is kept.
///
/// Catches: the reports of retired hosts kept for as long as the coordinator runs, and a
/// retirement that forgets another host's report.
#[test]
fn a_retired_host_has_no_report() {
    let f = Fixture::new("ssh-heartbeat-retire");
    for (n, host) in ["db-1", "db-2", "db-3"].into_iter().enumerate() {
        f.approve(&format!("m-{n}"), host, APPROVED, false);
        f.beat(host, "storage", FIRST, vec![runs(WEB, "1.0")]);
        f.rig
            .coordinator
            .hear_ssh_report(&who(host, "storage"), &announcing(Some(full())));
    }
    let reports = &f.rig.coordinator.ssh_reports;
    assert!(
        ["db-1", "db-2", "db-3"]
            .iter()
            .all(|h| reports.report_of(&id(h)).is_some())
    );
    f.rig
        .coordinator
        .retire_host(
            &operator(),
            &RetireRequest {
                node_id: "db-1".to_owned(),
                reason: "gone".to_owned(),
            },
        )
        .expect("retired");
    assert_eq!(reports.report_of(&id("db-1")), None);
    f.enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("db-2").expect("a host id"))
        .expect("denied");
    assert!(
        f.rig
            .coordinator
            .final_report(who("db-2", "storage"), daemon(vec![runs(WEB, "1.0")]))
    );
    assert_eq!(reports.report_of(&id("db-2")), None);
    assert!(
        reports.report_of(&id("db-3")).is_some(),
        "a host that stays"
    );
}
