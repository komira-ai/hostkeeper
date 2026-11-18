//! The SSH access of a reply and the report a heartbeat carries: announced and applied only by a
//! daemon that sets sshd up, applied only while the coordinator advertises `ssh-v1`, and gating
//! nothing the order of the same reply does.

use hostkeeper_link::testing::World;
use hostkeeper_link::{ANNOUNCED, Feature, PASSWORD_KEEP, PushedSsh, Reply, SSH, SshReport};
use hostkeeper_wire::wire::{MAX_FEATURES, MAX_FEATURES_BEFORE_SSH};

use crate::{Rig, recorded, reply, update};

fn access(generation: u64) -> PushedSsh {
    PushedSsh {
        generation,
        enabled: true,
        ca_public_keys: vec!["ssh-ed25519 AAAA".to_owned()],
        login_user: "root".to_owned(),
        principal: "hk-host-web-04".to_owned(),
        revoked_serials: vec![3],
        password_auth: PASSWORD_KEEP.to_owned(),
    }
}

fn sent(features: &[Feature], generation: u64) -> Reply {
    Reply {
        features: features.to_vec(),
        ssh: Some(access(generation)),
        ..Reply::default()
    }
}

fn setting_up(name: &str) -> Rig {
    Rig::with_world(name, World::default().setting_ssh_up())
}

fn report(generation: u64) -> SshReport {
    SshReport {
        applied_generation: generation,
        state: "applied".to_owned(),
        host_key_fingerprints: vec!["SHA256:aaa".to_owned()],
        detail: String::new(),
    }
}

/// Proves a daemon with no sshd setup announces exactly [`ANNOUNCED`], reports nothing about
/// SSH, and applies no access, even from a reply that advertises `ssh-v1` and carries it.
#[test]
fn a_host_with_no_ssh_setup_announces_none_and_reports_none() {
    let rig = Rig::new("host-ssh-none");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[Feature::SshV1], 3)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].features, ANNOUNCED.to_vec());
    assert_eq!(beats[1].ssh, None);
    assert!(rig.world.ssh.applied().is_empty());
}

/// Proves a daemon with an sshd setup announces `ssh-v1`, after the others, only once the
/// coordinator's last reply advertised it, stops when a reply no longer does, and sends the report
/// the setup gives (none while it has none).
///
/// Catches: a daemon that names `ssh-v1` to a coordinator of a release before it, which reads at
/// most 16 names and drops a longer list whole.
#[test]
fn a_host_with_an_ssh_setup_announces_the_feature_and_sends_its_report() {
    let rig = setting_up("host-ssh-report");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(None, &[Feature::SshV1])));
    rig.world.link.answer(Ok(reply(None, &[])));
    heartbeater.beat_once();
    rig.world.ssh.set_report(Some(report(7)));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = rig.world.link.beats();
    let mut features = ANNOUNCED.to_vec();
    assert_eq!(beats[0].features, features);
    features.push(SSH);
    assert_eq!(beats[1].features, features);
    assert_eq!(beats[2].features, ANNOUNCED.to_vec());
    assert_eq!(beats[0].ssh, None);
    assert_eq!(beats[1].ssh, Some(report(7)));
    assert_eq!(Feature::parse("ssh-v1"), Some(Feature::SshV1));
    assert_eq!(Feature::SshV1.as_str(), "ssh-v1");
}

/// Proves the access of a reply goes to the setup only while that reply advertises `ssh-v1`,
/// whole and unchanged, once per reply.
#[test]
fn access_is_applied_only_when_the_reply_advertises_the_feature() {
    let rig = setting_up("host-ssh-apply");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(sent(&[], 1)));
    rig.world
        .link
        .answer(Ok(sent(&[Feature::HaltV1, Feature::SshV1], 2)));
    rig.world.link.answer(Ok(reply(None, &[Feature::SshV1])));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(rig.world.ssh.applied(), vec![(access(2), 0)]);
}

/// Proves the access of a reply that also carries an order is applied, and the order is still
/// started: nothing the setup came to holds an order back.
#[test]
fn an_order_is_started_whatever_the_setup_came_to() {
    let rig = setting_up("host-ssh-order");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    let mut answer = sent(&[Feature::SshV1], 4);
    answer.order = Some(update("o-1", 1, "x", "2.0"));
    rig.world.link.answer(Ok(answer));
    heartbeater.beat_once();
    assert_eq!(rig.world.ssh.applied(), vec![(access(4), 0)]);
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Proves a daemon with every port (repositories, facts, private repositories, operator keys,
/// host prerequisites and an sshd setup) names at most [`MAX_FEATURES_BEFORE_SSH`] features to a
/// coordinator that does not advertise `ssh-v1`, and all of them, `ssh-v1` last and within
/// [`MAX_FEATURES`], to one that does.
///
/// Catches: a heartbeat whose feature list a coordinator of the release before drops whole, so
/// the host is sent no repositories, facts or keys and no build is picked for it.
#[test]
fn a_daemon_with_every_port_names_no_more_than_an_older_coordinator_reads() {
    let world = World::default()
        .managing_repositories()
        .keeping_facts()
        .keeping_operator_keys()
        .keeping_prerequisites()
        .reading_private_repositories()
        .setting_ssh_up();
    let rig = Rig::with_world("host-ssh-every-port", world);
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(None, &[])));
    rig.world.link.answer(Ok(reply(None, &[Feature::SshV1])));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    let beats = rig.world.link.beats();
    assert_eq!(beats[0].features.len(), MAX_FEATURES_BEFORE_SSH);
    assert_eq!(beats[1].features.len(), MAX_FEATURES_BEFORE_SSH);
    assert!(!beats[1].features.contains(&SSH));
    assert_eq!(beats[2].features.len(), MAX_FEATURES_BEFORE_SSH + 1);
    assert_eq!(beats[2].features.last(), Some(&SSH));
    assert!(beats[2].features.len() <= MAX_FEATURES);
}
