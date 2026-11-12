//! Which entry of a release each host is ordered (docs/design/packages.md, section 3.3, build
//! item A3): the installers a daemon lists, read from its heartbeats over gRPC; the pick of the
//! entry of the installer a host runs, else of the first entry its daemon lists; the entry in the
//! orders over gRPC to a daemon that announced `target-v1`; and `no_target`, `no_build_for_host`
//! and `installer_dropped`, which are neither failed nor missing and hold no convergence.
//!
//! The fleet is mixed: daemons before `target-v1` and with it, on `linux-x86_64` and
//! `linux-arm64`, listing `deb`, `rpm`, only `tarball`, or nothing.

mod support;

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{Param, Target, TargetList, param};
use prost::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::beats::{Daemon, FORCE, converge, grpc_beat, host, json_beat, reason, runs, said};
use support::rig::{Rig, Spec};
use support::targets::{SET_DESIRED, digest, entry, grpc, list, put_komira, repository_entry};
use support::{OPERATOR, ci_token};

const SERVICES: &str = r#"
[[service]]
name = "web"
roles = ["daemon"]
stages = [100]
"#;

const X86: agent::Platform = agent::Platform::LinuxX8664;
const ARM: agent::Platform = agent::Platform::LinuxArm64;
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

/// `web`'s entry of `installer`, whose digest is all `c`.
fn web(installer: &str, c: char) -> Target {
    entry("web", installer, c, 1_000)
}

/// Targets with one list, `linux-x86_64`'s.
fn x86(entries: Vec<Target>) -> BTreeMap<String, TargetList> {
    BTreeMap::from([("linux-x86_64".to_owned(), list(entries))])
}

/// A set of `web` at `version` with `targets`, with room for every host to update at once.
fn request(version: &str, targets: BTreeMap<String, TargetList>) -> proto::SetDesiredRequest {
    let mut request = support::targets::set("web", version, targets);
    request
        .desired
        .as_mut()
        .expect("a desired state")
        .max_updating = [("daemon".to_owned(), 16)].into_iter().collect();
    request
}

/// Sets `request` as CI does.
async fn set_by_ci(rig: &Rig, request: &proto::SetDesiredRequest) {
    let answered = grpc(rig, SET_DESIRED, Some(&ci_token()), request).await;
    assert_eq!(answered.code, 0, "{answered:?}");
}

/// Sets `web` at `version` with `targets`, as CI does.
async fn set(rig: &Rig, version: &str, targets: BTreeMap<String, TargetList>) {
    set_by_ci(rig, &request(version, targets)).await;
}

/// The release index of `web` at `version` listing the `linux-x86_64` archive whose digest is
/// all `c` and whose size is 1000, in the form the channel writes.
fn index_text(version: &str, c: char) -> String {
    json!({
        "packages": [{
            "bytes": 1_000,
            "file": format!("web-{version}-linux-x86_64.tar.zst"),
            "platform": "linux-x86_64",
            "sha256": digest(c),
        }],
        "schema": 1,
        "service": "web",
        "version": version,
    })
    .to_string()
}

/// The lower-case hex SHA-256 of `text`.
fn sha256_of(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `request` with the release index `text` and its digest.
fn with_index(mut request: proto::SetDesiredRequest, text: &str) -> proto::SetDesiredRequest {
    request.release_index = Some(text.to_owned());
    request.release_index_digest = Some(sha256_of(text));
    request
}

/// The verb, the installer of the target and the package digest of an order whose target is the
/// entry it names; panics on a target whose digest is not the package digest.
fn picked(said: Option<support::beats::Said>) -> Option<(&'static str, String, String)> {
    said.map(|(verb, _, _, package, _, target)| {
        let target = target.expect("a target");
        assert_eq!(target.digest, package, "the package digest is the entry's");
        (verb, target.installer, package)
    })
}

/// The pick of `[deb, rpm, tarball]`, and of `[tarball, deb]`, for new hosts listing `deb`,
/// `rpm`, or only `tarball`.
fn first_listed_cases() -> [(&'static str, Vec<&'static str>); 3] {
    [
        ("d1", vec![ARCHIVE, "tarball", "deb"]),
        ("d2", vec![ARCHIVE, "tarball", "rpm"]),
        ("d3", vec![ARCHIVE, "tarball"]),
    ]
}

/// Proves: with the list `[deb, rpm, tarball]`, a new host whose daemon lists `deb` and
/// `tarball` is ordered the `deb` entry, one listing `rpm` and `tarball` the `rpm` entry, and one
/// listing only `tarball` the tarball, each an install over gRPC carrying the entry, the entry's
/// digest as the package digest, and `target-v1` echoed.
/// Catches: a pick that takes the list's first entry whatever the daemon lists, or the daemon's
/// first installer whatever the list's order.
#[tokio::test]
async fn each_new_host_is_ordered_the_first_entry_its_daemon_lists() {
    let rig = rig("pick-first-listed");
    set(
        &rig,
        "2.0",
        x86(vec![web("deb", 'a'), web("rpm", 'b'), web("tarball", 'c')]),
    )
    .await;
    rig.at(LATER);
    let wanted = [("deb", 'a'), ("rpm", 'b'), ("tarball", 'c')];
    for ((name, installers), (installer, c)) in first_listed_cases().into_iter().zip(wanted) {
        let answer = grpc_beat(&rig, &Daemon::new(name, X86, &installers)).await;
        assert_eq!(
            picked(said(&answer)),
            Some(("install", installer.to_owned(), digest(c))),
            "{name}"
        );
        assert!(answer.features.iter().any(|f| f == "target-v1"), "{name}");
    }
}

/// Proves: with the list `[tarball, deb]`, the hosts that list `deb`, `rpm` or only `tarball`
/// are every one ordered the tarball: CI's order of the list decides.
/// Catches: a pick that prefers a native format over the list's order.
#[tokio::test]
async fn with_the_tarball_first_every_new_host_is_ordered_the_tarball() {
    let rig = rig("pick-tarball-first");
    set(&rig, "2.0", x86(vec![web("tarball", 'c'), web("deb", 'a')])).await;
    rig.at(LATER);
    for (name, installers) in first_listed_cases() {
        let answer = grpc_beat(&rig, &Daemon::new(name, X86, &installers)).await;
        assert_eq!(
            picked(said(&answer)),
            Some(("install", "tarball".to_owned(), digest('c'))),
            "{name}"
        );
    }
}

/// Proves: with the list `[deb, rpm]`, a new host that lists only `tarball` is ordered nothing
/// and held `no_build_for_host`; `Hosts.Get` shows the installers its daemon listed; and the
/// version converges once the host picked the `deb` runs it, with the counts naming one host
/// picked and one with no build, none missing and none failed.
/// Catches: a host no entry fits counted as missing or failed, or holding the convergence; the
/// installers a daemon listed not shown.
#[tokio::test]
async fn a_host_no_entry_fits_is_no_build_for_host_and_holds_no_convergence() {
    let rig = rig("pick-no-build");
    set(&rig, "2.0", x86(vec![web("deb", 'a'), web("rpm", 'b')])).await;
    rig.at(LATER);
    let deb = Daemon::new("d1", X86, &[ARCHIVE, "tarball", "deb"]);
    assert!(said(&grpc_beat(&rig, &deb).await).is_some());
    let tarball_only = Daemon::new("d3", X86, &[ARCHIVE, "tarball"]);
    assert_eq!(said(&grpc_beat(&rig, &tarball_only).await), None);
    assert_eq!(
        reason(&rig, "web", "d3").await,
        proto::GateReason::NoBuildForHost
    );
    let shown = host(&rig, "d3").await;
    assert_eq!(shown.installers, [ARCHIVE, "tarball"]);
    assert_eq!(
        shown.heartbeat.expect("a heartbeat").installers,
        [ARCHIVE, "tarball"]
    );

    let updated = deb.clone().running(runs("web", "2.0", Some("deb")));
    assert_eq!(said(&grpc_beat(&rig, &updated).await), None);
    let convergence = converge(&rig, "web").await;
    assert_eq!(
        convergence.state(),
        proto::ConvergenceState::Converged,
        "{convergence:?}"
    );
    let counts = convergence.hosts.expect("the counts");
    assert_eq!(
        (counts.picked, counts.no_build_for_host, counts.missing),
        (1, 1, 0)
    );
    assert!(convergence.roles.iter().all(|role| role.failed == 0));
    assert_eq!(
        host(&rig, "d1")
            .await
            .heartbeat
            .expect("a heartbeat")
            .services[0]
            .installer,
        Some("deb".to_owned())
    );
}

/// Proves: with the list `[apt from a repository, tarball]` (packages-repositories.md section
/// 2), a new host whose daemon lists `apt` is ordered the repository entry over gRPC, whole (its
/// repository, package version and pin), with the pin as the package digest; one listing only
/// `tarball` is ordered the tarball; and an old daemon, which counts as listing
/// `hostkeeper-archive` alone, is ordered nothing and held `no_build_for_host`.
/// Catches: a repository entry ordered to a daemon that does not list its installer, picked by
/// anything but the equality of installer names, or sent without its two fields.
#[tokio::test]
async fn an_apt_entry_is_ordered_only_to_a_host_whose_daemon_lists_apt() {
    let rig = rig("pick-repository");
    put_komira(&rig).await;
    let apt = repository_entry("web", "1:2.0-1", 'a');
    set(&rig, "2.0", x86(vec![apt.clone(), web("tarball", 'c')])).await;
    rig.at(LATER);
    let lists_apt = Daemon::new("r1", X86, &[ARCHIVE, "tarball", "apt"]);
    let order = said(&grpc_beat(&rig, &lists_apt).await).expect("an install");
    assert_eq!(
        (order.0, order.3.as_str(), order.5),
        ("install", digest('a').as_str(), Some(apt))
    );
    let tarball_only = Daemon::new("r2", X86, &[ARCHIVE, "tarball"]);
    assert_eq!(
        picked(said(&grpc_beat(&rig, &tarball_only).await)),
        Some(("install", "tarball".to_owned(), digest('c')))
    );
    let old = Daemon::old("r3", X86);
    assert_eq!(said(&grpc_beat(&rig, &old).await), None);
    assert_eq!(
        reason(&rig, "web", "r3").await,
        proto::GateReason::NoBuildForHost
    );
}

/// Proves: a host of a platform the release has no list for (`linux-arm64`, left out) is ordered
/// nothing and held `no_target`, is not missing, and the version converges without it.
/// Catches: a host of a null platform ordered something, counted missing, or holding the
/// convergence.
#[tokio::test]
async fn a_host_of_a_platform_with_no_list_is_no_target_and_holds_no_convergence() {
    let rig = rig("pick-no-target");
    set(&rig, "2.0", x86(vec![web("tarball", 'c')])).await;
    rig.at(LATER);
    let arm = Daemon::new("a1", ARM, &[ARCHIVE, "tarball"]);
    assert_eq!(said(&grpc_beat(&rig, &arm).await), None);
    assert_eq!(reason(&rig, "web", "a1").await, proto::GateReason::NoTarget);
    let x = Daemon::new("d1", X86, &[ARCHIVE, "tarball"]);
    assert!(said(&grpc_beat(&rig, &x).await).is_some());
    grpc_beat(&rig, &x.running(runs("web", "2.0", Some("tarball")))).await;
    let convergence = converge(&rig, "web").await;
    assert_eq!(convergence.state(), proto::ConvergenceState::Converged);
    let counts = convergence.hosts.expect("the counts");
    assert_eq!((counts.picked, counts.no_target, counts.missing), (1, 1, 0));
}

/// Proves: a daemon before `target-v1` is picked nothing, whatever the release lists: with the
/// list `[deb, hostkeeper-archive]` and a release index set beside it, a daemon that runs `web`
/// and one that does not are each held `no_build_for_host` and ordered nothing, over gRPC and on
/// the JSON route; and with a list that has no archive entry the same.
/// Catches: an old daemon ordered the archive entry of a release set before the archives ended,
/// which it can no longer be installed from, or an index digest made up for a set that carried
/// none.
#[tokio::test]
async fn an_old_daemon_is_picked_nothing_whatever_the_release_lists() {
    let rig = rig("pick-old-index");
    let text = index_text("2.0", 'f');
    let targets = x86(vec![web("deb", 'a'), web(ARCHIVE, 'f')]);
    set_by_ci(&rig, &with_index(request("2.0", targets), &text)).await;
    rig.at(LATER);
    let running = Daemon::old("o1", X86).running(runs("web", "1.0", None));
    let answer = grpc_beat(&rig, &running).await;
    assert_eq!(said(&answer), None);
    assert!(!answer.features.iter().any(|f| f == "target-v1"));
    assert_eq!(
        reason(&rig, "web", "o1").await,
        proto::GateReason::NoBuildForHost
    );
    let answered = json_beat(&rig, &running).await;
    assert_eq!(answered["reply"], json!("nothing"), "{answered}");

    let new = Daemon::old("o2", X86);
    assert_eq!(said(&grpc_beat(&rig, &new).await), None);
    assert_eq!(
        reason(&rig, "web", "o2").await,
        proto::GateReason::NoBuildForHost
    );

    let plain = self::rig("pick-old-no-archive");
    set(
        &plain,
        "2.0",
        x86(vec![web("deb", 'a'), web("tarball", 'c')]),
    )
    .await;
    plain.at(LATER);
    assert_eq!(
        said(&grpc_beat(&plain, &Daemon::old("o3", X86)).await),
        None
    );
    assert_eq!(
        reason(&plain, "web", "o3").await,
        proto::GateReason::NoBuildForHost
    );
    assert_eq!(said(&grpc_beat(&plain, &running).await), None);
    assert_eq!(
        reason(&plain, "web", "o1").await,
        proto::GateReason::NoBuildForHost
    );
}

/// Proves: a host that runs `web` from its `tarball` is ordered the list's `tarball` entry
/// although a `deb` entry comes first and its daemon lists `deb` too.
/// Catches: a host moved to another installer by an update, which only an uninstall may do.
#[tokio::test]
async fn a_host_running_from_its_tarball_is_ordered_the_tarball_though_a_deb_comes_first() {
    let rig = rig("pick-kept-installer");
    set(&rig, "2.0", x86(vec![web("deb", 'a'), web("tarball", 'c')])).await;
    rig.at(LATER);
    let daemon = Daemon::new("d1", X86, &[ARCHIVE, "tarball", "deb"]).running(runs(
        "web",
        "1.0",
        Some("tarball"),
    ));
    assert_eq!(
        picked(said(&grpc_beat(&rig, &daemon).await)),
        Some(("update", "tarball".to_owned(), digest('c')))
    );
}

/// Proves: a host that runs `web` from its `tarball` is ordered nothing and held
/// `installer_dropped` when the list has no `tarball` entry, and so is one whose daemon no longer
/// lists `tarball`; `Converge` names both, counts them, and is not converged, since no host was
/// picked an entry; neither is failed.
/// Catches: a dropped installer counted as failed, ordered another format, or left out of what
/// `Converge` names.
#[tokio::test]
async fn a_host_whose_installer_the_release_dropped_is_named_by_converge() {
    let rig = rig("pick-dropped");
    set(&rig, "2.0", x86(vec![web("deb", 'a')])).await;
    rig.at(LATER);
    let dropped = Daemon::new("d1", X86, &[ARCHIVE, "tarball", "deb"]).running(runs(
        "web",
        "1.0",
        Some("tarball"),
    ));
    let unlisted =
        Daemon::new("d2", X86, &[ARCHIVE, "deb"]).running(runs("web", "1.0", Some("tarball")));
    for daemon in [&dropped, &unlisted] {
        assert_eq!(
            said(&grpc_beat(&rig, daemon).await),
            None,
            "{}",
            daemon.host
        );
        assert_eq!(
            reason(&rig, "web", &daemon.host).await,
            proto::GateReason::InstallerDropped
        );
    }
    let convergence = converge(&rig, "web").await;
    assert_eq!(convergence.installer_dropped, ["d1", "d2"]);
    assert_ne!(convergence.state(), proto::ConvergenceState::Converged);
    let counts = convergence.hosts.expect("the counts");
    assert_eq!((counts.installer_dropped, counts.picked), (2, 0));
    assert!(convergence.roles.iter().all(|role| role.failed == 0));
}

/// Proves: a list whose one entry names an installer no daemon lists (`debb`) is picked for no
/// host, and the version is not converged though no host is behind it, with counts of no host
/// picked and the others by why: the daemon before `target-v1` is `no_build_for_host` too.
/// Catches: a release no host can take reported converged, as an empty count would make it.
#[tokio::test]
async fn a_misspelt_installer_no_host_is_picked_for_leaves_the_version_not_converged() {
    let rig = rig("pick-misspelt");
    set(&rig, "2.0", x86(vec![web("debb", 'a')])).await;
    rig.at(LATER);
    let new = Daemon::new("d1", X86, &[ARCHIVE, "deb"]);
    let old = Daemon::old("o1", X86).running(runs("web", "1.0", None));
    for daemon in [&new, &old] {
        assert_eq!(said(&grpc_beat(&rig, daemon).await), None);
    }
    let convergence = converge(&rig, "web").await;
    assert_eq!(convergence.state(), proto::ConvergenceState::InProgress);
    let counts = convergence.hosts.expect("the counts");
    assert_eq!(
        (
            counts.picked,
            counts.no_build_for_host,
            counts.installer_dropped
        ),
        (0, 2, 0)
    );
}

/// Proves: a daemon of `target-v1` that runs `web` from its tarball is picked nothing until the
/// coordinator has read its installers over gRPC: its first heartbeat, over JSON, is answered no
/// order and it is held `not_a_member`; its next one over gRPC is ordered the tarball; and after the coordinator restarts, which
/// keeps no list, the same holds again, `Hosts.Get` showing no installers until the gRPC beat;
/// a host that does not run `web` is shown `not_a_member` too, and no reason of the release,
/// before its list is read; a host of a platform with no list is `no_target` before its list is
/// read.
/// Catches: a host picked from an empty list (`no_build_for_host`) or as an old daemon (the
/// archive) before its list is read.
#[tokio::test]
async fn a_target_v1_host_is_picked_nothing_until_its_installers_are_read() {
    let mut rig = rig("pick-unheard");
    set(
        &rig,
        "2.0",
        x86(vec![web("tarball", 'c'), web(ARCHIVE, 'f')]),
    )
    .await;
    rig.at(LATER);
    let daemon =
        Daemon::new("d1", X86, &[ARCHIVE, "tarball"]).running(runs("web", "1.0", Some("tarball")));
    for _ in 0..2 {
        let answered = json_beat(&rig, &daemon).await;
        assert_eq!(answered["reply"], json!("nothing"), "{answered}");
        assert_eq!(
            reason(&rig, "web", "d1").await,
            proto::GateReason::NotAMember
        );
        assert!(host(&rig, "d1").await.installers.is_empty());
        assert_eq!(
            picked(said(&grpc_beat(&rig, &daemon).await)),
            Some(("update", "tarball".to_owned(), digest('c')))
        );
        rig.restart();
        rig.at(LATER + 100);
    }
    // A platform the release has no list for is the release's choice: shown at once, not
    // waiting for the list.
    rig.at(LATER + 1_000);
    let fresh = Daemon::new("d9", X86, &[ARCHIVE, "tarball"]);
    json_beat(&rig, &fresh).await;
    assert_eq!(
        reason(&rig, "web", "d9").await,
        proto::GateReason::NotAMember,
        "no word of what it can install: no reason of the release is shown"
    );
    let arm =
        Daemon::new("a1", ARM, &[ARCHIVE, "tarball"]).running(runs("web", "1.0", Some("tarball")));
    json_beat(&rig, &arm).await;
    assert_eq!(reason(&rig, "web", "a1").await, proto::GateReason::NoTarget);
}

/// Proves: one host beating over gRPC, then JSON, then gRPC keeps the installers its first
/// heartbeat listed (`Hosts.Get` shows them after the JSON one); over JSON it is sent no order of
/// its `deb` and its next gRPC reply orders it; and a host that runs `web` from archives is sent
/// no order over JSON either, being picked nothing.
/// Catches: a JSON heartbeat that clears the list read over gRPC, or an order on the JSON route.
#[tokio::test]
async fn the_json_route_keeps_the_list_and_carries_no_order() {
    let rig = rig("pick-json-route");
    let deb = Daemon::new("d1", X86, &[ARCHIVE, "deb"]).running(runs("web", "1.0", Some("deb")));
    let archive = Daemon::new("d2", X86, &[ARCHIVE]).running(runs("web", "1.0", None));
    grpc_beat(&rig, &deb).await;
    grpc_beat(&rig, &archive).await;
    set(&rig, "2.0", x86(vec![web("deb", 'a'), web(ARCHIVE, 'f')])).await;
    rig.at(LATER);

    let answered = json_beat(&rig, &deb).await;
    assert_eq!(answered["reply"], json!("nothing"), "{answered}");
    assert!(
        answered["features"]
            .as_array()
            .expect("features")
            .contains(&json!("target-v1"))
    );
    assert_eq!(host(&rig, "d1").await.installers, [ARCHIVE, "deb"]);
    assert_eq!(
        picked(said(&grpc_beat(&rig, &deb).await)),
        Some(("update", "deb".to_owned(), digest('a')))
    );

    let answered = json_beat(&rig, &archive).await;
    assert_eq!(answered["reply"], json!("nothing"), "{answered}");
    assert_eq!(answered.get("package_digest"), None, "{answered}");
    assert_eq!(
        reason(&rig, "web", "d2").await,
        proto::GateReason::InstallerDropped
    );
}

/// Proves: a host picked nothing (`no_build_for_host`) whose next gRPC heartbeat lists another
/// installer is picked from the new list and ordered it.
/// Catches: a pick made once and kept past the daemon's restart.
#[tokio::test]
async fn a_host_whose_next_grpc_heartbeat_lists_other_installers_is_picked_again() {
    let rig = rig("pick-new-list");
    set(&rig, "2.0", x86(vec![web("deb", 'a'), web("tarball", 'c')])).await;
    rig.at(LATER);
    let before = Daemon::new("d1", X86, &[ARCHIVE, "rpm"]);
    assert_eq!(said(&grpc_beat(&rig, &before).await), None);
    let after = Daemon::new("d1", X86, &[ARCHIVE, "tarball"]);
    assert_eq!(
        picked(said(&grpc_beat(&rig, &after).await)),
        Some(("install", "tarball".to_owned(), digest('c')))
    );
}

/// Proves: a host ordered the `deb` whose daemon answers `installer_not_listed` (its list changed
/// at a restart), and lists `tarball` alone in the same heartbeat, is ordered the tarball at once,
/// with no deferral wait; a deferral of any other reason still waits.
/// Catches: an `installer_not_listed` deferral that holds the host for the deferral backoff.
#[tokio::test]
async fn an_order_answered_installer_not_listed_is_picked_again_at_the_next_heartbeat() {
    let rig = rig("pick-not-listed");
    set(&rig, "2.0", x86(vec![web("deb", 'a'), web("tarball", 'c')])).await;
    rig.at(LATER);
    let daemon =
        Daemon::new("d1", X86, &[ARCHIVE, "deb"]).running(runs("web", "1.0", Some("tarball")));
    let mut deferred = daemon.clone();
    deferred.installers = vec![ARCHIVE.to_owned(), "tarball".to_owned()];
    // A host that runs from its tarball is ordered the tarball, whatever its list says first.
    let mut running = runs("web", "1.0", Some("tarball"));
    for (why, ordered) in [("installer_not_listed", true), ("drain_bound", false)] {
        running.deferred = Some(agent::DeferredReport {
            target: "2.0".to_owned(),
            reason: why.to_owned(),
            retry_after_secs: 0,
        });
        deferred.services = vec![running.clone()];
        let answer = grpc_beat(&rig, &deferred).await;
        assert_eq!(said(&answer).is_some(), ordered, "{why}");
    }
    let fresh = Daemon::new("d2", X86, &[ARCHIVE, "deb"]);
    assert_eq!(
        picked(said(&grpc_beat(&rig, &fresh).await)),
        Some(("install", "deb".to_owned(), digest('a')))
    );
    let mut answered = fresh.clone();
    answered.installers = vec![ARCHIVE.to_owned(), "tarball".to_owned()];
    let mut absent = runs("web", "1.0", None);
    absent.health = agent::Health::Absent;
    absent.deferred = Some(agent::DeferredReport {
        target: "2.0".to_owned(),
        reason: "installer_not_listed".to_owned(),
        retry_after_secs: 0,
    });
    answered.services = vec![absent];
    assert_eq!(
        picked(said(&grpc_beat(&rig, &answered).await)),
        Some(("install", "tarball".to_owned(), digest('c')))
    );
}

/// Proves: going back to a version set with targets that has no entry of the installer a host
/// runs (an operator setting the earlier version again) orders the host nothing and holds it
/// `installer_dropped`; going back to a version set with a release index and no targets orders it
/// nothing either, and holds it `no_build_for_host`: a version with no release kept orders
/// nothing.
/// Catches: a rollback that orders a host another format, or an order made of a release index.
#[tokio::test]
async fn a_rollback_follows_the_pick_of_the_version_it_goes_to() {
    let rig = rig("pick-rollback");
    let text = index_text("0.9", 'f');
    set_by_ci(
        &rig,
        &with_index(support::targets::set("web", "0.9", BTreeMap::new()), &text),
    )
    .await;
    set(&rig, "1.0", x86(vec![web("deb", 'a')])).await;
    set(&rig, "2.0", x86(vec![web("tarball", 'c')])).await;
    rig.at(LATER);
    let daemon = Daemon::new("d1", X86, &[ARCHIVE, "tarball", "deb"]).running(runs(
        "web",
        "2.0",
        Some("tarball"),
    ));
    assert_eq!(said(&grpc_beat(&rig, &daemon).await), None);

    let back = grpc(
        &rig,
        SET_DESIRED,
        Some(OPERATOR),
        &request("1.0", x86(vec![web("deb", 'a')])),
    )
    .await;
    assert_eq!(back.code, 0, "{back:?}");
    assert_eq!(said(&grpc_beat(&rig, &daemon).await), None);
    assert_eq!(
        reason(&rig, "web", "d1").await,
        proto::GateReason::InstallerDropped
    );

    let mut index = with_index(support::targets::set("web", "0.9", BTreeMap::new()), &text);
    index
        .desired
        .as_mut()
        .expect("a desired state")
        .max_updating = [("daemon".to_owned(), 16)].into_iter().collect();
    let back = grpc(&rig, SET_DESIRED, Some(OPERATOR), &index).await;
    assert_eq!(back.code, 0, "{back:?}");
    assert_eq!(said(&grpc_beat(&rig, &daemon).await), None);
    assert_eq!(
        reason(&rig, "web", "d1").await,
        proto::GateReason::NoBuildForHost
    );
}

/// Proves: `Releases.Force` to a version set with targets orders each targeted host the entry
/// the rollout gate would pick (the `deb` of a host running from its `deb`), and skips the others
/// by the same reasons: `installer_dropped` for a host running from a tarball the list dropped,
/// `no_target` for a host of a platform with no list, `no_build_for_host` for a daemon before
/// `target-v1` and for a Mac that has the service staged and never run and lists no installer of
/// its list (a tarball) (and the status says so); the force's order carries the entry over gRPC.
/// Catches: a force that orders every host one digest, or that ignores the pick.
#[tokio::test]
async fn a_force_to_a_version_set_with_targets_follows_the_pick() {
    let rig = rig("pick-force");
    let mut targets = x86(vec![web("deb", 'a'), web(ARCHIVE, 'f')]);
    targets.insert("macos-arm64".to_owned(), list(vec![web("tarball", 'd')]));
    set(&rig, "2.0", targets).await;
    rig.at(LATER);
    let deb = Daemon::new("d1", X86, &[ARCHIVE, "deb"]).running(runs("web", "1.0", Some("deb")));
    let dropped =
        Daemon::new("d2", X86, &[ARCHIVE, "tarball"]).running(runs("web", "1.0", Some("tarball")));
    let old = Daemon::old("o1", X86).running(runs("web", "1.0", None));
    let arm = Daemon::new("a1", ARM, &[ARCHIVE]).running(runs("web", "1.0", None));
    let mut staged = runs("web", "1.0", None);
    staged.health = agent::Health::Absent;
    let unbuilt = Daemon::new("n1", agent::Platform::DarwinArm64, &[ARCHIVE]).running(staged);
    for daemon in [&deb, &dropped, &old, &arm, &unbuilt] {
        grpc_beat(&rig, daemon).await;
    }
    let forced = grpc(
        &rig,
        FORCE,
        Some(OPERATOR),
        &proto::ForceRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            reason: "the hook is broken".to_owned(),
            targets: Some(proto::force_request::Targets::Fleet(proto::FleetTarget {
                confirm_service: "web".to_owned(),
            })),
            include_held: false,
            host_group: String::new(),
        },
    )
    .await;
    let response: proto::ForceResponse = forced.decoded();
    let skipped: Vec<(String, String)> = response
        .skipped
        .into_iter()
        .map(|s| (s.node_id, s.reason))
        .collect();
    assert_eq!(
        skipped,
        [
            ("a1".to_owned(), "no_target".to_owned()),
            ("d2".to_owned(), "installer_dropped".to_owned()),
            ("n1".to_owned(), "no_build_for_host".to_owned()),
            ("o1".to_owned(), "no_build_for_host".to_owned()),
        ]
    );
    assert_eq!(response.targeted, 1);
    assert_eq!(
        reason(&rig, "web", "n1").await,
        proto::GateReason::NoBuildForHost
    );
    assert_eq!(
        picked(said(&grpc_beat(&rig, &deb).await)),
        Some(("force", "deb".to_owned(), digest('a')))
    );
    assert_eq!(said(&grpc_beat(&rig, &old).await), None);
    assert_eq!(said(&grpc_beat(&rig, &dropped).await), None);
}

/// Proves: the largest entry a release lets through (a manifest of 16 KiB encoded, the most a
/// manifest may be, and 2 KiB of locators, the most an entry may have) is accepted, and the reply
/// that orders it to a daemon announcing every feature, which the coordinator echoes, is at most
/// 32 KiB, the reply a `target-v1` daemon reads.
/// Catches: an order of one entry too large for the daemon's reply limit.
#[tokio::test]
async fn a_reply_with_the_largest_entry_fits_in_32_kib() {
    const MANIFEST: usize = 16 * 1024;
    let rig = rig("pick-largest");
    let mut largest = web("tarball", 'c');
    largest.locations = vec![support::targets::location(&"a".repeat(1024))];
    largest.attestation.as_mut().expect("a bundle").locations =
        vec![support::targets::location(&"b".repeat(1024))];
    let text = |n: usize| Param {
        value: Some(param::Value::Text("x".repeat(n))),
    };
    let manifest = largest.manifest.as_mut().expect("a manifest");
    for n in 0..64 {
        manifest.params.insert(format!("p{n:02}"), text(200));
    }
    // Grow one param to the limit; its length's varint may grow by a byte on the way.
    let mut size = 200 + MANIFEST - manifest.encoded_len();
    while manifest.encoded_len() != MANIFEST {
        manifest.params.insert("p00".to_owned(), text(size));
        size -= usize::from(manifest.encoded_len() > MANIFEST);
    }
    set(&rig, "2.0", x86(vec![largest.clone()])).await;
    rig.at(LATER);
    let mut daemon = Daemon::new("d1", X86, &[ARCHIVE, "tarball"]);
    daemon.features = support::beats::OLD
        .iter()
        .chain(&[
            "target-v1",
            "deferred-v1",
            "halt-v1",
            "verdict-acks-v1",
            "grpc-v1",
        ])
        .map(|f| (*f).to_owned())
        .collect();
    let answer = grpc_beat(&rig, &daemon).await;
    assert_eq!(said(&answer).and_then(|o| o.5), Some(largest));
    assert_eq!(answer.features.len(), 8, "{:?}", answer.features);
    assert!(
        answer.encoded_len() <= 32 * 1024,
        "{}",
        answer.encoded_len()
    );
}

/// Proves: a set whose JSON form names a null platform gives the same release as one that leaves
/// the platform out, and its hosts are `no_target` alike: the value the gateway drops is read as
/// no list.
/// Catches: a null list taken as an empty list, which would read as no build for the host.
#[tokio::test]
async fn a_null_platform_is_no_target() {
    let rig = rig("pick-null-platform");
    let mut body: Value =
        support::targets::set_json(&request("2.0", x86(vec![web("tarball", 'c')])));
    body["targets"]["linux-arm64"] = Value::Null;
    let (code, answer) =
        support::targets::gateway(&rig, SET_DESIRED, Some(OPERATOR), &body.to_string()).await;
    assert_eq!(code, 200, "{answer}");
    rig.at(LATER);
    let arm = Daemon::new("a1", ARM, &[ARCHIVE, "tarball"]);
    assert_eq!(said(&grpc_beat(&rig, &arm).await), None);
    assert_eq!(reason(&rig, "web", "a1").await, proto::GateReason::NoTarget);
}

/// Proves: a force of a version whose list has no archive entry (`[deb]`) orders the host the
/// pick gives the `deb`, with the entry, and skips none of them.
/// Catches: a force that judges a host by the archive entries alone, as the record of the force
/// names them, and skips every host of a release with none as `no_package_for_platform`.
#[tokio::test]
async fn a_force_of_a_release_with_no_archive_orders_the_hosts_picked_an_entry() {
    let rig = rig("pick-force-no-archive");
    set(&rig, "2.0", x86(vec![web("deb", 'a')])).await;
    rig.at(LATER);
    let deb = Daemon::new("d1", X86, &[ARCHIVE, "deb"]).running(runs("web", "1.0", Some("deb")));
    grpc_beat(&rig, &deb).await;
    let forced = grpc(
        &rig,
        FORCE,
        Some(OPERATOR),
        &proto::ForceRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            reason: "the hook is broken".to_owned(),
            targets: Some(proto::force_request::Targets::Fleet(proto::FleetTarget {
                confirm_service: "web".to_owned(),
            })),
            include_held: false,
            host_group: String::new(),
        },
    )
    .await;
    let response: proto::ForceResponse = forced.decoded();
    assert_eq!((response.targeted, response.skipped), (1, Vec::new()));
    assert_eq!(
        picked(said(&grpc_beat(&rig, &deb).await)),
        Some(("force", "deb".to_owned(), digest('a')))
    );
}

/// Proves: the counts of `Converge` and the reasons `Hosts.Get` adds cover only the hosts the
/// decision asks of: a host of a role the service does not run, one an operator excluded, one an
/// operator holds and one whose agent does not list every service it runs are none of them, while
/// a host picked an entry is counted.
/// Catches: hosts the service does not count reported as picked nothing, which would read as
/// builds missing for hosts that are not the service's at all.
#[tokio::test]
async fn the_counts_leave_out_the_hosts_the_decision_does_not_ask() {
    let rig = rig("pick-counted");
    set(&rig, "2.0", x86(vec![web("tarball", 'c')])).await;
    rig.at(LATER);
    let picked =
        Daemon::new("d1", X86, &[ARCHIVE, "tarball"]).running(runs("web", "1.0", Some("tarball")));
    let mut storage = Daemon::new("s1", X86, &[ARCHIVE]);
    storage.role = "storage".to_owned();
    let excluded = Daemon::new("e1", X86, &[ARCHIVE]);
    let held = Daemon::new("h1", X86, &[ARCHIVE]);
    let mut unmanaged = Daemon::new("u1", X86, &[ARCHIVE]);
    unmanaged.features.retain(|f| f != "heartbeat-v2");
    for daemon in [&picked, &storage, &excluded, &held, &unmanaged] {
        grpc_beat(&rig, daemon).await;
    }
    let answered = grpc(
        &rig,
        "/hostkeeper.operator.v1.Hosts/Exclude",
        Some(OPERATOR),
        &proto::ExcludeRequest {
            service: "web".to_owned(),
            node_id: "e1".to_owned(),
            reason: "a broken disk".to_owned(),
            abandon: false,
        },
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let answered = grpc(
        &rig,
        "/hostkeeper.operator.v1.Services/Hold",
        Some(OPERATOR),
        &proto::HoldRequest {
            scope: Some(proto::HoldScope {
                scope: Some(proto::hold_scope::Scope::Host("h1".to_owned())),
            }),
            service: "web".to_owned(),
            reason: "a migration".to_owned(),
        },
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let counts = converge(&rig, "web").await.hosts.expect("the counts");
    assert_eq!(
        (counts.picked, counts.no_build_for_host, counts.no_target),
        (1, 0, 0)
    );
    for node in ["s1", "e1"] {
        let shown = host(&rig, node).await;
        assert!(
            !shown
                .services
                .iter()
                .any(|one| one.gate_reason() == proto::GateReason::NoBuildForHost),
            "{node}: {shown:?}"
        );
    }
}

/// Proves: a daemon of `target-v1` that does not read order terms (`order-v1`) is still sent the
/// entry over gRPC, the order naming it; and a daemon before both is told nothing over the JSON
/// route, which carries no order.
/// Catches: the entry sent only to a daemon that reads terms, or an order on the JSON route.
#[tokio::test]
async fn the_entry_goes_to_a_target_v1_daemon_that_reads_no_terms() {
    let rig = rig("pick-no-terms");
    set(
        &rig,
        "2.0",
        x86(vec![web("tarball", 'c'), web(ARCHIVE, 'f')]),
    )
    .await;
    rig.at(LATER);
    let mut new =
        Daemon::new("d1", X86, &[ARCHIVE, "tarball"]).running(runs("web", "1.0", Some("tarball")));
    new.features.retain(|f| f != "order-v1");
    assert_eq!(
        picked(said(&grpc_beat(&rig, &new).await)),
        Some(("update", "tarball".to_owned(), digest('c')))
    );
    let mut old = Daemon::old("o1", X86).running(runs("web", "1.0", None));
    old.features.retain(|f| f != "order-v1");
    let answered = json_beat(&rig, &old).await;
    assert_eq!(answered["reply"], json!("nothing"), "{answered}");
    assert_eq!(answered.get("package_digest"), None, "{answered}");
}
