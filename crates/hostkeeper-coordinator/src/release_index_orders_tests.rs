//! The orders of a version set over a release index: each host is ordered the package its own
//! platform has in the index, together with the index's digest, as an update, an install or a
//! force; a host whose platform the index does not list is ordered nothing, and no failure is
//! counted for it; a version set with one package digest is ordered as before; and a rollback
//! goes back to the release the earlier version was registered with. Every time is the rig's
//! clock; every transport is the in-process one, with the host's identity explicit.

use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};
use serde_json::{Value, json};

use crate::assign_support::why_of;
use crate::converge::State;
use crate::decide::{Skip, Why};
use crate::marks_support::{
    WEB, ask, ask_typed, digest, fleet_target, force_request, operator, ops_bytes, rig, runs, set,
    update, who,
};
use crate::release_index_support::{
    ALL, beat_on, force_verb, index_body, index_text, install_verb, set_index, sha, update_verb,
    verb_of,
};
use crate::rollback_tests::{AUTO, round, status, two_failed};
use crate::testutil::{LEGACY, OPERATOR, version};

const X86: Platform = Platform::LinuxX86_64;
const ARM: Platform = Platform::LinuxArm64;
const MAC: Platform = Platform::DarwinArm64;

/// A heartbeat of a daemon of `platform` that runs `web` at `running`.
fn web_at(platform: Platform, running: &str) -> Heartbeat {
    beat_on(platform, vec![runs(WEB, running)])
}

/// One set over an index of three platforms, with a digest of its own for each, orders a
/// linux-x86_64 host, a linux-arm64 host and a macos-arm64 host (which the wire spells
/// `darwin-arm64`) each the package of its own platform and the index's SHA-256.
///
/// Catches: the digest of one platform ordered to every host (the daemon refuses a package whose
/// SHA-256 is not the order's), the Mac looked up under the wire's spelling and found nowhere,
/// or the index's digest left off the order.
#[tokio::test]
async fn one_set_over_a_three_platform_index_orders_each_host_its_own_package() {
    let rig = rig("index-three-platforms", "");
    let index = set_index(&rig, WEB, "2.0", &ALL).await;
    for (host, platform, n) in [("d1", X86, 1), ("d2", MAC, 3), ("d3", ARM, 2)] {
        let (reply, order) = ask_typed(&rig, host, "daemon", web_at(platform, "1.0"));
        assert_eq!(reply, update(WEB, "2.0"), "{host}");
        assert_eq!(
            verb_of(order),
            update_verb(WEB, "2.0", &digest(n), &index),
            "{host}"
        );
        // The host runs the version: its slot is free for the next host.
        let done = ask_typed(&rig, host, "daemon", web_at(platform, "2.0"));
        assert_eq!(done, (ServiceReply::Nothing, None), "{host}");
    }
}

/// The JSON heartbeat answer of an `order-v1` agent carries the package digest of the host's
/// platform and no key for the index: the answer's shape is frozen.
///
/// Catches: the index digest added to the JSON answer, whose strict readers refuse a key they do
/// not know; the digest of another platform in the answer.
#[tokio::test]
async fn the_json_answer_carries_the_hosts_package_digest_and_no_index_key() {
    let rig = rig("index-json-answer", "");
    set_index(&rig, WEB, "2.0", &ALL).await;
    let answer = rig
        .coordinator
        .heartbeat(who("d1", "daemon"), web_at(MAC, "1.0"))
        .expect("answered");
    let wrote: Value = serde_json::to_value(&answer).expect("JSON");
    let keys: Vec<&str> = wrote
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "features",
            "order_id",
            "order_try",
            "package_digest",
            "reply",
            "service",
            "version"
        ]
    );
    assert_eq!(wrote["package_digest"], json!(digest(3)));
}

/// An index that lists no package for a host's platform orders that host nothing, an update or
/// an install, over either transport, and says why (`no_package_for_platform`); the host takes no
/// slot, so a host of a listed platform is ordered at once; the host is counted as no failure,
/// no halt and no bad version, and it does not keep the service from converging.
///
/// Catches: the unlisted host ordered another platform's package (and failing it), the reason
/// missing or spelled otherwise, a slot held for a host that was told nothing, or a service that
/// never converges because one host cannot run the version.
#[tokio::test]
async fn an_index_without_the_hosts_platform_orders_it_nothing_and_says_why() {
    let rig = rig("index-unlisted", "");
    let index = set_index(&rig, WEB, "2.0", &ALL[..2]).await;
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", web_at(MAC, "1.0")),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(
        why_of(&rig, WEB, "d1"),
        Some(Why::Skip(Skip::NoPackageForPlatform))
    );
    let mut plain = web_at(MAC, "1.0");
    plain.agent_features = Vec::new();
    assert_eq!(ask(&rig, "d1", "daemon", plain), ServiceReply::Nothing);

    // The host of a listed platform is ordered right after: the Mac holds no slot.
    let (reply, order) = ask_typed(&rig, "d2", "daemon", web_at(X86, "1.0"));
    assert_eq!(reply, update(WEB, "2.0"));
    assert_eq!(verb_of(order), update_verb(WEB, "2.0", &digest(1), &index));

    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    let hold = status.per_service[WEB]
        .hosts
        .iter()
        .find(|host| host.host.node_id == "d1")
        .and_then(|host| host.hold);
    assert_eq!(
        serde_json::to_value(hold).expect("JSON"),
        json!("no_package_for_platform")
    );
    let marks = status.ops.services.get(WEB);
    assert!(marks.is_none_or(|m| m.bad.is_empty() && m.halt.is_none()));

    // The listed host runs the version; the Mac still runs 1.0, and the service has converged.
    ask_typed(&rig, "d2", "daemon", web_at(X86, "2.0"));
    let convergence = rig.coordinator.convergence_verified(WEB).expect("known");
    assert_eq!(convergence.state, State::Converged);
    assert_eq!(convergence.roles.iter().map(|c| c.failed).sum::<usize>(), 0);
    assert_eq!(
        convergence
            .roles
            .iter()
            .map(|c| c.at_desired)
            .sum::<usize>(),
        1
    );
}

/// An install names the package of the new host's platform, though the host runs nothing of the
/// service yet and so has no word of it but its platform; a platform the index does not list is
/// installed nothing.
///
/// Catches: an install that looks for the platform in the host's report of the service (there is
/// none), takes another platform's package, or orders a package the index does not list.
#[tokio::test]
async fn an_install_names_the_package_of_the_hosts_own_platform() {
    let rig = rig("index-install", "");
    let index = set_index(&rig, WEB, "2.0", &ALL[..2]).await;
    for (host, platform, n) in [("d1", ARM, 2), ("d2", X86, 1)] {
        let (reply, order) = ask_typed(&rig, host, "daemon", beat_on(platform, vec![]));
        assert_eq!(reply, ServiceReply::Nothing, "{host}");
        assert_eq!(
            verb_of(order),
            install_verb(WEB, "2.0", &digest(n), &index),
            "{host}"
        );
        ask_typed(&rig, host, "daemon", web_at(platform, "2.0"));
    }
    assert_eq!(
        ask_typed(&rig, "d3", "daemon", beat_on(MAC, vec![])),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(
        why_of(&rig, WEB, "d3"),
        Some(Why::Skip(Skip::NoPackageForPlatform))
    );
}

/// A version set with one package digest is ordered that digest to the hosts of every platform,
/// as an update and as an install, with no index digest: a fleet of one package works as before.
///
/// Catches: a platform lookup applied to a release that has none, so that a Mac is ordered
/// nothing, or an index digest on an order that has no index.
#[tokio::test]
async fn a_set_with_one_package_digest_orders_that_digest_to_every_platform() {
    let rig = rig("index-package-only", "");
    set(&rig, WEB, "2.0", 7).await;
    for (host, platform) in [("d1", X86), ("d2", MAC), ("d3", ARM)] {
        let (reply, order) = ask_typed(&rig, host, "daemon", web_at(platform, "1.0"));
        assert_eq!(reply, update(WEB, "2.0"), "{host}");
        assert_eq!(
            verb_of(order),
            update_verb(WEB, "2.0", &digest(7), ""),
            "{host}"
        );
        ask_typed(&rig, host, "daemon", web_at(platform, "2.0"));
    }
    let (_, order) = ask_typed(&rig, "d4", "daemon", beat_on(MAC, vec![]));
    assert_eq!(verb_of(order), install_verb(WEB, "2.0", &digest(7), ""));
}

/// A force over an index orders each targeted host the package of its platform with the index's
/// digest, lists the hosts of an unlisted platform as skipped (`no_package_for_platform`) and
/// orders them nothing, and records the index with the force; a host that has since moved to a
/// platform the index does not list is ordered nothing.
///
/// Catches: a force that orders one digest to every host, orders a package the index lacks, loses
/// the index digest, or drops the skipped host from the answer.
#[tokio::test]
async fn a_force_orders_each_host_its_platforms_package_and_skips_the_unlisted() {
    let rig = rig("index-force", "");
    for (host, platform) in [("d1", X86), ("d2", ARM), ("d3", MAC)] {
        let heard = ask_typed(&rig, host, "daemon", web_at(platform, "1.0"));
        assert_eq!(heard, (ServiceReply::Nothing, None), "{host}");
    }
    let index = set_index(&rig, WEB, "2.0", &ALL[..2]).await;
    let response = rig
        .coordinator
        .force(
            &operator(),
            force_request(WEB, "2.0", fleet_target(WEB), false),
        )
        .expect("the force is accepted");
    assert_eq!(response.targeted, 2);
    let skipped: Vec<(&str, &str)> = response
        .skipped
        .iter()
        .map(|host| (host.node_id.as_str(), host.reason.as_str()))
        .collect();
    assert_eq!(skipped, [("d3", "no_package_for_platform")]);

    for (host, platform, n) in [("d1", X86, 1), ("d2", ARM, 2)] {
        let (_, order) = ask_typed(&rig, host, "daemon", web_at(platform, "1.0"));
        assert_eq!(
            verb_of(order),
            force_verb(WEB, "2.0", &digest(n), &index, 1),
            "{host}"
        );
    }
    assert_eq!(
        ask_typed(&rig, "d3", "daemon", web_at(MAC, "1.0")),
        (ServiceReply::Nothing, None)
    );

    let record = rig
        .coordinator
        .ops()
        .marks(WEB)
        .force
        .expect("a force stands");
    assert_eq!(record.digest.as_str(), index);
    assert_eq!(record.release_index.map(|r| r.packages.len()), Some(2));

    // A host that moved to a platform the index lacks is ordered no force.
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", web_at(MAC, "1.0")),
        (ServiceReply::Nothing, None)
    );
}

/// A force of a version registered with one package digest writes the force record as it always
/// did: no key for an index, and the order names the digest and no index.
///
/// Catches: a new key in the ops file of every force, which a coordinator before this one
/// refuses to read.
#[tokio::test]
async fn a_force_of_a_package_digest_writes_no_index_key() {
    let rig = rig("index-force-plain", "");
    ask_typed(&rig, "d1", "daemon", web_at(X86, "1.0"));
    set(&rig, WEB, "2.0", 7).await;
    rig.coordinator
        .force(
            &operator(),
            force_request(WEB, "2.0", fleet_target(WEB), false),
        )
        .expect("the force is accepted");
    let text = String::from_utf8(ops_bytes(&rig).expect("the ops file")).expect("UTF-8");
    assert!(text.contains(&digest(7)), "the force names its digest");
    assert!(!text.contains("release_index"), "{text}");
    let (_, order) = ask_typed(&rig, "d1", "daemon", web_at(X86, "1.0"));
    assert_eq!(verb_of(order), force_verb(WEB, "2.0", &digest(7), "", 1));
}

/// An automatic rollback goes back to the release the earlier version was registered with: an
/// index when it had one (and no package digest), one package digest when it had that.
///
/// Catches: a rollback that keeps the release of the version it leaves, so the hosts are ordered
/// the failed version's packages under the old version's name.
#[tokio::test]
async fn a_rollback_goes_back_to_the_release_of_the_earlier_version() {
    let text = index_text(LEGACY, "1.0", &ALL);
    let index = sha(&text);
    let body = index_body(LEGACY, "1.0", &text, &index).to_string();
    let rig = crate::rollback_tests::rig("index-rollback-to-index", AUTO, &body).await;
    round(&rig, 2_000, &[]);
    let plain = json!({"version": "2.0", "min_up": {"storage": 0, "frontend": 0},
        "release_digest": digest(2)});
    rig.set(&plain.to_string()).await;
    round(&rig, 2_030, &two_failed());
    let back = status(&rig).services[LEGACY].clone();
    assert_eq!(back.version, version("1.0"));
    assert_eq!(back.release_digest, None);
    assert_eq!(
        back.release_index.map(|r| r.digest.to_string()),
        Some(index)
    );

    let body = json!({"version": "1.0", "min_up": {"storage": 0, "frontend": 0},
        "release_digest": digest(1)});
    let rig = crate::rollback_tests::rig("index-rollback-to-plain", AUTO, &body.to_string()).await;
    round(&rig, 2_000, &[]);
    set_index(&rig, LEGACY, "2.0", &ALL).await;
    round(&rig, 2_030, &two_failed());
    let back = status(&rig).services[LEGACY].clone();
    assert_eq!(back.version, version("1.0"));
    assert_eq!(back.release_index, None);
    assert_eq!(back.release_digest.map(|d| d.to_string()), Some(digest(1)));
}

/// An operator's set over an index is one audit entry whose reason names the version and the
/// index's digest.
///
/// Catches: an audit entry that says nothing of the release set, or the package-digest wording
/// for an index.
#[tokio::test]
async fn an_operators_set_over_an_index_is_audited_with_the_index_digest() {
    let rig = rig("index-audit", "");
    let text = index_text(WEB, "2.0", &ALL);
    let index = sha(&text);
    let body = index_body(WEB, "2.0", &text, &index);
    rig.coordinator
        .set_desired_operator("operator", body.to_string().as_bytes())
        .expect("the operator's set is accepted");
    let audit = rig.coordinator.ops().state().audit;
    let entry = audit.last().expect("an audit entry");
    assert_eq!(
        (entry.action.as_str(), entry.reason.as_deref()),
        (
            "set_desired",
            Some(format!("2.0 with release index {index}").as_str())
        )
    );
}
