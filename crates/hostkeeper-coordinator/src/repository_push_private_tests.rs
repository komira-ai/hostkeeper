//! The private repositories a host is sent ([`crate::repository_push`]): the set is made for the
//! host's group (the role its certificate names), a private entry reaches only a daemon that
//! announced `private-repository-v1`, and the digest is the digest of the host's own set.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry;

use crate::marks_support::{DB, WEB, ask, beat_of, operator, rig, runs, who};
use crate::repositories_support::{credential_in, komira, private};
use crate::repository_push::{FEATURE, PRIVATE_FEATURE};
use crate::testutil::{Rig, id};

const URI: &str = "https://priv.example.com/apt";
const FILE: &str = "/etc/hostkeeper/credentials/priv";

/// A rig whose `web` names the repository `priv`; `db` names `komira`; `solo`, of the role
/// `frontend`, names `priv` too.
fn priv_rig(name: &str) -> Rig {
    rig(
        name,
        "repository = \"priv\"\n\n[[service]]\nname = \"solo\"\nroles = [\"frontend\"]\nrepository = \"priv\"",
    )
}

/// Puts `repository` as the operator; a private one gets a credential file made for it.
fn put(rig: &Rig, mut repository: Repository) {
    if registry::is_private(&repository) {
        repository.credential_file = credential_in(
            rig.dir.path(),
            &format!("{}-credential", repository.name),
            "Bearer x",
        );
    }
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(repository),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
}

/// The host `host` of the role `role` heartbeats with `features`, runs `services`, and says it
/// holds no set.
fn beats(rig: &Rig, host: &str, role: &str, features: &[&str], services: &[&str]) {
    let announced = features.iter().map(|f| (*f).to_owned()).collect();
    rig.coordinator.hear_repositories(
        &who(host, role),
        &agent::HeartbeatRequest {
            features: announced,
            ..agent::HeartbeatRequest::default()
        },
    );
    let running = services.iter().map(|s| runs(s, "1.0")).collect();
    let mut all = vec!["heartbeat-v2"];
    all.extend(features);
    ask(rig, host, role, beat_of(&all, running));
}

fn set_for(rig: &Rig, host: &str) -> Option<agent::RepositorySet> {
    rig.coordinator.repository_set(&id(host))
}

/// `priv` as a host of `group` is sent it.
fn priv_for(group: &str) -> Repository {
    Repository {
        suites: vec![group.to_owned()],
        credential_file: String::new(),
        ..private("priv", URI, &["x"], 1, FILE)
    }
}

/// Proves: a daemon that announced `private-repository-v1` is sent the private entry its service
/// names with its own group (the role of its certificate) as the one suite and no credential file,
/// beside the public entry, and the digest is that of this set; the same entry sent to a host of
/// another group lists that group.
///
/// Catches: a private entry sent with the suites of other groups or with the credential file, or a
/// digest the daemon would compute differently.
#[test]
fn a_private_entry_is_sent_with_the_hosts_group_as_its_suite_and_no_credential() {
    let rig = priv_rig("private-push-sent");
    put(&rig, komira(2));
    put(&rig, private("priv", URI, &["daemon", "storage"], 1, FILE));
    let features = [FEATURE, PRIVATE_FEATURE];
    beats(&rig, "d1", "daemon", &features, &[WEB, DB]);
    beats(&rig, "d2", "storage", &features, &[WEB, DB]);

    let first = set_for(&rig, "d1").expect("a set");
    assert_eq!(first.entries, [komira(2), priv_for("daemon")]);
    assert_eq!(first.digest, registry::digest(&first.entries));
    assert_eq!(registry::check_set(&first.entries), Ok(()));
    let second = set_for(&rig, "d2").expect("a set");
    assert_eq!(second.entries, [komira(2), priv_for("storage")]);
    assert_ne!(first.digest, second.digest, "the digest is per host");
}

/// Proves: a daemon that announced `repositories-v1` but not `private-repository-v1` is sent the
/// public entries only; with only a private entry to send it is sent no set.
///
/// Catches: a private entry pushed to a daemon that cannot mirror it.
#[test]
fn a_daemon_that_did_not_announce_the_private_feature_is_sent_no_private_entry() {
    let rig = priv_rig("private-push-older");
    put(&rig, komira(2));
    put(&rig, private("priv", URI, &["daemon", "frontend"], 1, FILE));
    beats(&rig, "d1", "daemon", &[FEATURE], &[WEB, DB]);
    let set = set_for(&rig, "d1").expect("a set");
    assert_eq!(set.entries, [komira(2)]);
    assert_eq!(set.digest, registry::digest(&[komira(2)]));

    beats(&rig, "i1", "frontend", &[FEATURE], &["solo"]);
    assert_eq!(set_for(&rig, "i1"), None, "only a private entry is named");
}

/// Proves: a private entry whose suites do not list the host's group is left out of its set and
/// the rest is sent; a host whose set would be empty is sent none; the host shows the repository
/// as named, with the digest of what it is sent.
///
/// Catches: a host sent a group's private repository it is not in, or one that stops getting its
/// public repositories because a private one is not for it.
#[test]
fn a_private_entry_of_other_groups_is_left_out_and_the_rest_is_sent() {
    let rig = priv_rig("private-push-left-out");
    put(&rig, komira(2));
    put(&rig, private("priv", URI, &["storage"], 1, FILE));
    let features = [FEATURE, PRIVATE_FEATURE];
    beats(&rig, "d1", "daemon", &features, &[WEB, DB]);
    let set = set_for(&rig, "d1").expect("a set");
    assert_eq!(set.entries, [komira(2)]);

    beats(&rig, "i1", "frontend", &features, &["solo"]);
    assert_eq!(set_for(&rig, "i1"), None, "nothing left to send");
    let shown = rig
        .coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: "i1".to_owned(),
            },
        )
        .expect("the host")
        .host
        .expect("a host")
        .repositories;
    assert_eq!(
        shown,
        Some(proto::HostRepositories {
            named: vec!["priv".to_owned()],
            ..proto::HostRepositories::default()
        })
    );
}

/// Proves: `Hosts.Get` shows the digest of the host's own set, and the host is current when its
/// daemon reports that digest.
///
/// Catches: a host page that shows the registry's digest instead of the host's.
#[test]
fn hosts_show_the_digest_of_their_own_set() {
    let rig = priv_rig("private-push-shown");
    put(&rig, komira(2));
    put(&rig, private("priv", URI, &["daemon"], 1, FILE));
    let features = [FEATURE, PRIVATE_FEATURE];
    beats(&rig, "d1", "daemon", &features, &[WEB, DB]);
    let digest = registry::digest(&[komira(2), priv_for("daemon")]);
    let shown = |rig: &Rig| {
        rig.coordinator
            .get_host(
                &operator(),
                &proto::GetHostRequest {
                    node_id: "d1".to_owned(),
                },
            )
            .expect("the host")
            .host
            .expect("a host")
            .repositories
            .expect("shown")
    };
    let before = shown(&rig);
    assert_eq!(
        (before.digest.as_str(), before.current),
        (digest.as_str(), false)
    );
    rig.coordinator.hear_repositories(
        &who("d1", "daemon"),
        &agent::HeartbeatRequest {
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            repositories: Some(agent::RepositoriesReport {
                generation: 2,
                digest: digest.clone(),
                refused: None,
            }),
            ..agent::HeartbeatRequest::default()
        },
    );
    assert!(shown(&rig).current);
    assert_eq!(set_for(&rig, "d1"), None, "its set is in force");
}
