//! A version CI sets with no release kept (docs/design/packages-native.md section 10.7): a set
//! with a package digest, with a release index or with neither is accepted, and orders nothing,
//! so every host is held `no_build_for_host` and `Converge` says not converged: an old CI client's
//! call fails loudly in its wait. A release stored with archive entries and an index still loads
//! after a restart, and no pick returns its archive entries.
//!
//! `apt` is the native installer here; `hostkeeper-archive` is the one that no entry of is ever
//! picked.

mod support;

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{Target, TargetList};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use support::beats::{Daemon, FORCE, converge, grpc_beat, json_beat, reason, runs, said};
use support::rig::{Rig, Spec};
use support::targets::{
    GET_RELEASE, SET_DESIRED, digest, entry, grpc, list, put_komira, repository_entry, set,
};
use support::{OPERATOR, ci_token};

const SERVICES: &str = r#"
[[service]]
name = "web"
roles = ["daemon"]
stages = [100]
"#;

const X86: agent::Platform = agent::Platform::LinuxX8664;
const ARCHIVE: &str = "hostkeeper-archive";

/// A time past the coordinator's quiet start.
const LATER: u64 = 2_000;

fn rig(name: &str) -> Rig {
    Rig::new(
        name,
        Spec::recorded()
            .with_services(SERVICES)
            .with_oidc_services(&["web"])
            .without_legacy(),
    )
    .routed()
}

/// The lower-case hex SHA-256 of `text`.
fn sha256_of(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The bodies of the sets that have no release kept, each with what it names.
fn bodies() -> Vec<(&'static str, String)> {
    let index = r#"{"packages":[],"schema":1,"service":"web","version":"2.0"}"#;
    vec![
        (
            "a package digest",
            json!({"service": "web", "version": "2.0", "release_digest": digest('a')}).to_string(),
        ),
        (
            "a release index",
            json!({"service": "web", "version": "2.0",
                "release_index": index, "release_index_digest": sha256_of(index)})
            .to_string(),
        ),
        (
            "neither",
            json!({"service": "web", "version": "2.0"}).to_string(),
        ),
    ]
}

/// The daemons of the farm: `d1` runs `web` 1.0 from `apt`, `d2` does not run it and lists
/// `apt`, `d3` runs `web` 2.0 from `apt`, which is the version set.
fn daemons() -> [Daemon; 3] {
    [
        Daemon::new("d1", X86, &["apt"]).running(runs("web", "1.0", Some("apt"))),
        Daemon::new("d2", X86, &["apt"]),
        Daemon::new("d3", X86, &["apt"]).running(runs("web", "2.0", Some("apt"))),
    ]
}

/// Proves: a set with a package digest, with a release index (its digest held to its text) or
/// with neither is accepted and stored, and then no host is ordered anything, over gRPC and on the
/// JSON route: the host behind and the host that does not run `web` are each held
/// `no_build_for_host`, the host at the version is up to date; `Converge` says in progress, never
/// converged, though a host runs the version set; and `GetRelease` answers no release for it.
/// Catches: a version registered by a digest or an index still ordered by it, hosts shown as
/// up to date or missing instead of `no_build_for_host`, or a convergence that reports the
/// version reached though no host is picked a build of it.
#[tokio::test]
async fn a_set_with_a_digest_an_index_or_neither_is_accepted_and_orders_nothing() {
    for (n, (what, body)) in bodies().into_iter().enumerate() {
        let rig = rig(&format!("no-release-{n}"));
        rig.at(1_100);
        let (code, answer) = rig.desired(&body).await;
        assert_eq!(code, 200, "{what}: {answer}");
        let status = rig.status().await;
        assert_eq!(
            status["per_service"]["web"]["desired"]["version"], "2.0",
            "{what}"
        );
        rig.at(LATER);

        for daemon in daemons() {
            assert_eq!(said(&grpc_beat(&rig, &daemon).await), None, "{what}");
            let answered = json_beat(&rig, &daemon).await;
            assert_eq!(answered["reply"], json!("nothing"), "{what}: {answered}");
        }
        for host in ["d1", "d2"] {
            assert_eq!(
                reason(&rig, "web", host).await,
                proto::GateReason::NoBuildForHost,
                "{what}: {host}"
            );
        }
        // A host that runs the version needs no order: it is up to date, though not counted.
        assert_eq!(
            reason(&rig, "web", "d3").await,
            proto::GateReason::UpToDate,
            "{what}"
        );
        let convergence = converge(&rig, "web").await;
        assert_eq!(
            convergence.state(),
            proto::ConvergenceState::InProgress,
            "{what}"
        );
        let read = grpc(
            &rig,
            GET_RELEASE,
            Some(OPERATOR),
            &proto::GetReleaseRequest {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
            },
        )
        .await;
        assert_eq!(read.refusal(), (5, "release_unknown"), "{what}");
    }
}

/// Proves: `Releases.Force` of a version set with a package digest is accepted, and orders no
/// host: every host that runs the service is skipped `no_build_for_host` and none is targeted,
/// though the version is the desired one now.
/// Catches: a force of a registered digest ordering every host that digest, as it did.
#[tokio::test]
async fn a_force_of_a_version_set_with_a_digest_orders_no_host() {
    let rig = rig("no-release-force");
    rig.at(1_100);
    let (code, answer) = rig.desired(&bodies()[0].1).await;
    assert_eq!(code, 200, "{answer}");
    rig.at(LATER);
    for daemon in daemons() {
        grpc_beat(&rig, &daemon).await;
    }
    let forced = grpc(
        &rig,
        FORCE,
        Some(OPERATOR),
        &proto::ForceRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            reason: "an old digest".to_owned(),
            targets: Some(proto::force_request::Targets::Fleet(proto::FleetTarget {
                confirm_service: "web".to_owned(),
            })),
            include_held: false,
        },
    )
    .await;
    assert_eq!(forced.code, 0, "{forced:?}");
    let response: proto::ForceResponse = forced.decoded();
    assert_eq!(response.targeted, 0);
    let skipped: Vec<(&str, &str)> = response
        .skipped
        .iter()
        .map(|s| (s.node_id.as_str(), s.reason.as_str()))
        .collect();
    // A force targets the hosts that run the service: `d2` does not.
    assert_eq!(
        skipped,
        [("d1", "no_build_for_host"), ("d3", "no_build_for_host")]
    );
    for daemon in daemons() {
        assert_eq!(said(&grpc_beat(&rig, &daemon).await), None);
    }
}

/// An entry of `installer` for `web`, whose digest is all `c`.
fn web(installer: &str, c: char) -> Target {
    entry("web", installer, c, 1_000)
}

/// Targets with one list, `linux-x86_64`'s.
fn x86(entries: Vec<Target>) -> BTreeMap<String, TargetList> {
    BTreeMap::from([("linux-x86_64".to_owned(), list(entries))])
}

/// Proves: a release stored with archive entries and an index (the form every release had before
/// the archives ended) is read back after a restart, with its targets and its index digest, and
/// no pick returns an archive entry of it: a daemon that lists `hostkeeper-archive` and `apt`,
/// and does not run `web`, is ordered the `apt` entry; one that lists the archive alone is
/// ordered nothing, held `no_build_for_host`.
/// Catches: a stored release refused at its start, or its archive entry picked for a daemon that
/// still lists the installer.
#[tokio::test]
async fn a_stored_release_with_archive_entries_and_an_index_loads_and_picks_no_archive_entry() {
    let mut rig = rig("no-release-stored");
    put_komira(&rig).await;
    rig.at(1_100);
    let index = r#"{"packages":[],"schema":1,"service":"web","version":"2.0"}"#;
    let mut asked = set(
        "web",
        "2.0",
        x86(vec![
            repository_entry("web", "1:2.0-1", 'a'),
            web(ARCHIVE, 'f'),
        ]),
    );
    asked.release_index = Some(index.to_owned());
    asked.release_index_digest = Some(sha256_of(index));
    let answered = grpc(&rig, SET_DESIRED, Some(&ci_token()), &asked).await;
    assert_eq!(answered.code, 0, "{answered:?}");

    rig.restart();
    rig.at(LATER);
    let read = grpc(
        &rig,
        GET_RELEASE,
        Some(OPERATOR),
        &proto::GetReleaseRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
        },
    )
    .await;
    let release: proto::GetReleaseResponse = read.decoded();
    assert_eq!(release.targets, asked.targets);
    assert_eq!(release.release_index_digest, asked.release_index_digest);

    let both = Daemon::new("i1", X86, &[ARCHIVE, "apt"]);
    let order = said(&grpc_beat(&rig, &both).await).expect("an install");
    assert_eq!(
        (order.0, order.3.as_str(), order.5.map(|t| t.installer)),
        ("install", digest('a').as_str(), Some("apt".to_owned()))
    );
    let archive_only = Daemon::new("i2", X86, &[ARCHIVE]);
    assert_eq!(said(&grpc_beat(&rig, &archive_only).await), None);
    assert_eq!(
        reason(&rig, "web", "i2").await,
        proto::GateReason::NoBuildForHost
    );
}
