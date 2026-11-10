//! The move to native packages (docs/design/packages-native.md section 10.6, build item A18):
//! the pick that moves a host running a service from `hostkeeper-archive` to a native entry its
//! daemon lists, wherever that entry stands in the list; the archive entry for a host that lists
//! no native installer of the list, and for a daemon before `target-v1`; no archive entry ever
//! for a host running a native installer, but on a rollback to a version whose list has no
//! native entry; and the releases of the move, accepted over gRPC.
//!
//! The same pick for a Mac, whose list is the signed `.pkg` alone (`macos-pkg`, build item of
//! `release targets` for macOS): ordered to a Mac whose daemon announces `macos-pkg`, nothing to
//! one that does not.
//!
//! `apt` is the native installer here (a repository entry); `deb` and `tarball` are installers
//! of other formats, which do not move a host.

mod support;

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target, TargetList};
use support::beats::{Daemon, FORCE, converge, grpc_beat, json_beat, reason, runs, said};
use support::rig::{Rig, Spec};
use support::targets::{SET_DESIRED, digest, entry, grpc, list, repository_entry};
use support::{OPERATOR, ci_token};

const SERVICES: &str = r#"
[[service]]
name = "web"
roles = ["daemon"]
stages = [100]
"#;

const X86: agent::Platform = agent::Platform::LinuxX8664;
const MAC: agent::Platform = agent::Platform::DarwinArm64;
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

/// `web`'s artifact entry of `installer`, whose digest is all `c`.
fn web(installer: &str, c: char) -> Target {
    entry("web", installer, c, 1_000)
}

/// `web`'s `apt` entry at `package_version`, pinned by the digest all `c`.
fn apt(package_version: &str, c: char) -> Target {
    repository_entry("web", package_version, c)
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

/// Sets `web` at `version` with `targets`, as CI does.
async fn set(rig: &Rig, version: &str, targets: BTreeMap<String, TargetList>) {
    let answered = grpc(
        rig,
        SET_DESIRED,
        Some(&ci_token()),
        &request(version, targets),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
}

/// Sets `web` back to `version` with the `targets` it was set with, as an operator does to roll
/// it back.
async fn back_to(rig: &Rig, version: &str, targets: BTreeMap<String, TargetList>) {
    let answered = grpc(rig, SET_DESIRED, Some(OPERATOR), &request(version, targets)).await;
    assert_eq!(answered.code, 0, "{answered:?}");
}

/// The verb, the version, the installer of the target and the package digest of the order of
/// `daemon`'s next heartbeat over gRPC; panics on a target whose digest is not the package
/// digest.
async fn ordered(rig: &Rig, daemon: &Daemon) -> Option<(&'static str, String, String, String)> {
    said(&grpc_beat(rig, daemon).await).map(|(verb, _, version, package, _, target)| {
        let target = target.expect("a target");
        assert_eq!(target.digest, package, "the package digest is the entry's");
        (verb, version, target.installer, package)
    })
}

/// Proves: a host running `web` from `hostkeeper-archive` whose daemon lists `apt` is ordered the
/// `apt` entry, whole, with its pin as the package digest, whether the list names it before or
/// after the archive entry; and once it runs `web` from `apt` it is ordered the `apt` entry of
/// the next version.
/// Catches: the move not made (the host kept on the archive entry it runs), made only when the
/// native entry stands first, or the native entry sent without its repository fields.
#[tokio::test]
async fn a_host_running_from_archives_that_lists_apt_is_picked_the_apt_entry_wherever_it_stands() {
    for (name, entries) in [
        (
            "move-native-first",
            vec![apt("2.0-1", 'a'), web(ARCHIVE, 'f')],
        ),
        (
            "move-native-after",
            vec![web(ARCHIVE, 'f'), apt("2.0-1", 'a')],
        ),
    ] {
        let rig = rig(name);
        set(&rig, "2.0", x86(entries)).await;
        rig.at(LATER);
        let archived = Daemon::new("m1", X86, &[ARCHIVE, "apt"]).running(runs("web", "1.0", None));
        let order = said(&grpc_beat(&rig, &archived).await).expect("an update");
        assert_eq!(
            (order.0, order.2.as_str(), order.3.as_str(), order.5),
            (
                "update",
                "2.0",
                digest('a').as_str(),
                Some(apt("2.0-1", 'a'))
            ),
            "{name}"
        );
        set(&rig, "3.0", x86(vec![web(ARCHIVE, 'e'), apt("3.0-1", 'b')])).await;
        let moved =
            Daemon::new("m1", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
        assert_eq!(
            ordered(&rig, &moved).await,
            Some(("update", "3.0".to_owned(), "apt".to_owned(), digest('b'))),
            "{name}"
        );
    }
}

/// Proves: with the list `[apt, hostkeeper-archive]`, a host running `web` from archives whose
/// daemon lists no native installer of the list (`deb` and `tarball` only, or `macos-pkg`, which
/// the list has no entry of) is ordered the archive entry, and a daemon before `target-v1` is
/// ordered it too, with no target; a host that does not run `web` and lists `apt` is ordered the
/// `apt` entry as its install.
/// Catches: a host moved to an installer its daemon does not list, an old daemon sent a native
/// entry, or a host's first install held back by the move's rule.
#[tokio::test]
async fn a_host_that_lists_no_native_installer_of_the_list_and_an_old_daemon_are_picked_the_archive_entry()
 {
    let rig = rig("move-not-listed");
    set(&rig, "2.0", x86(vec![apt("2.0-1", 'a'), web(ARCHIVE, 'f')])).await;
    rig.at(LATER);
    for (host, installers) in [
        ("n1", vec![ARCHIVE, "deb", "tarball"]),
        ("n2", vec![ARCHIVE, "macos-pkg"]),
        ("n3", vec![ARCHIVE]),
    ] {
        let daemon = Daemon::new(host, X86, &installers).running(runs("web", "1.0", None));
        assert_eq!(
            ordered(&rig, &daemon).await,
            Some(("update", "2.0".to_owned(), ARCHIVE.to_owned(), digest('f'))),
            "{host}"
        );
    }
    let old = Daemon::old("o1", X86).running(runs("web", "1.0", None));
    let order = said(&grpc_beat(&rig, &old).await).expect("an update");
    assert_eq!(
        (order.0, order.3.as_str(), order.5),
        ("update", digest('f').as_str(), None)
    );
    let new = Daemon::new("i1", X86, &[ARCHIVE, "apt"]);
    assert_eq!(
        ordered(&rig, &new).await,
        Some(("install", "2.0".to_owned(), "apt".to_owned(), digest('a')))
    );
}

/// Proves: with lists that hold no native entry its daemon lists (`[deb, hostkeeper-archive]`
/// for a daemon listing `deb`, `[tarball, hostkeeper-archive]` for one listing `tarball`), a
/// host running `web` from archives stays on the archive entry; an installer of another format
/// never moves it.
/// Catches: the move's exception taken for any installer other than the one the host runs, which
/// would change a host's format with no native package to move to.
#[tokio::test]
async fn a_list_with_no_native_entry_its_daemon_lists_leaves_an_archive_host_on_the_archive_entry()
{
    for (name, other) in [("move-other-deb", "deb"), ("move-other-tarball", "tarball")] {
        let rig = rig(name);
        set(&rig, "2.0", x86(vec![web(other, 'a'), web(ARCHIVE, 'f')])).await;
        rig.at(LATER);
        let daemon = Daemon::new("k1", X86, &[ARCHIVE, other]).running(runs("web", "1.0", None));
        assert_eq!(
            ordered(&rig, &daemon).await,
            Some(("update", "2.0".to_owned(), ARCHIVE.to_owned(), digest('f'))),
            "{name}"
        );
    }
}

/// Proves: a host running `web` from `apt` is never ordered the archive entry of a newer
/// version: with `[hostkeeper-archive, apt]` it is ordered the `apt` entry, and with
/// `[hostkeeper-archive]` alone it is ordered nothing, held `installer_dropped` and named by
/// `Converge`.
/// Catches: a moved host sent back to archives by an update, which only the move's rollback may
/// do.
#[tokio::test]
async fn a_host_running_apt_is_never_picked_the_archive_entry_of_a_newer_version() {
    let rig = rig("move-no-way-back");
    set(&rig, "2.0", x86(vec![web(ARCHIVE, 'f'), apt("2.0-1", 'a')])).await;
    rig.at(LATER);
    let moved = Daemon::new("v1", X86, &[ARCHIVE, "apt"]).running(runs("web", "1.0", Some("apt")));
    assert_eq!(
        ordered(&rig, &moved).await,
        Some(("update", "2.0".to_owned(), "apt".to_owned(), digest('a')))
    );
    set(&rig, "3.0", x86(vec![web(ARCHIVE, 'e')])).await;
    let at_two = Daemon::new("v1", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &at_two).await), None);
    assert_eq!(
        reason(&rig, "web", "v1").await,
        proto::GateReason::InstallerDropped
    );
    let convergence = converge(&rig, "web").await;
    assert_eq!(convergence.installer_dropped, vec!["v1".to_owned()]);
}

/// Proves: the move's rollback. A host running `web` 2.0 from `apt`, when `web` goes back to 1.0
/// whose list has no native entry (`[hostkeeper-archive]`), is ordered 1.0's archive entry, over
/// gRPC with the entry and on the JSON route as an archive order is; when it goes back to 1.5,
/// whose list has an `apt` entry, it is ordered that one; a host whose daemon does not list `hostkeeper-archive` is ordered
/// nothing and held `installer_dropped`, and so is one whose daemon no longer lists `apt` going
/// back to 1.5.
/// Catches: the move made impossible to undo (a rollback held `installer_dropped`), a rollback
/// that moves a host to archives though its version has a native entry, or the archive entry
/// sent to a daemon that cannot install it.
#[tokio::test]
async fn a_rollback_across_the_move_carries_the_archive_entry_unless_the_version_has_a_native_one()
{
    let rig = rig("move-rollback");
    let one = x86(vec![web(ARCHIVE, 'f')]);
    let one_five = x86(vec![apt("1.5-1", 'b'), web(ARCHIVE, 'e')]);
    let two = x86(vec![apt("2.0-1", 'a'), web(ARCHIVE, 'd')]);
    set(&rig, "1.0", one.clone()).await;
    set(&rig, "1.5", one_five.clone()).await;
    set(&rig, "2.0", two).await;
    rig.at(LATER);
    let moved = Daemon::new("b1", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &moved).await), None, "up to date");
    // Its installers and the one it runs `web` with are heard over gRPC, then it beats over JSON.
    let json = Daemon::new("b4", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &json).await), None, "up to date");

    back_to(&rig, "1.0", one.clone()).await;
    assert_eq!(
        ordered(&rig, &moved).await,
        Some(("update", "1.0".to_owned(), ARCHIVE.to_owned(), digest('f')))
    );
    let no_archive = Daemon::new("b2", X86, &["apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &no_archive).await), None);
    assert_eq!(
        reason(&rig, "web", "b2").await,
        proto::GateReason::InstallerDropped
    );

    back_to(&rig, "1.5", one_five).await;
    let moved_again =
        Daemon::new("b3", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(
        ordered(&rig, &moved_again).await,
        Some(("update", "1.5".to_owned(), "apt".to_owned(), digest('b')))
    );
    // A daemon that no longer lists `apt` is not moved back to archives by a version that has a
    // native entry: only a version with none undoes the move.
    let unlisted = Daemon::new("b5", X86, &[ARCHIVE]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &unlisted).await), None);
    assert_eq!(
        reason(&rig, "web", "b5").await,
        proto::GateReason::InstallerDropped
    );

    // On the JSON route the archive order goes out as an old daemon's order is, with no target.
    back_to(&rig, "1.0", one).await;
    let answer = json_beat(&rig, &json).await;
    assert_eq!(answer["reply"], "update", "{answer}");
    assert_eq!(answer["package_digest"], digest('f').as_str(), "{answer}");
}

/// Proves: `Releases.Force` of `web` back to 1.0, whose list has no native entry, orders a host
/// running 2.0 from `apt` the archive entry of 1.0.
/// Catches: the operator's way back across the move refused where the gate's is not.
#[tokio::test]
async fn a_force_back_across_the_move_carries_the_archive_entry() {
    let rig = rig("move-force-back");
    let one = x86(vec![web(ARCHIVE, 'f')]);
    set(&rig, "1.0", one).await;
    set(&rig, "2.0", x86(vec![apt("2.0-1", 'a'), web(ARCHIVE, 'd')])).await;
    rig.at(LATER);
    let moved = Daemon::new("f1", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &moved).await), None, "up to date");
    let forced = grpc(
        &rig,
        FORCE,
        Some(OPERATOR),
        &proto::ForceRequest {
            service: "web".to_owned(),
            version: "1.0".to_owned(),
            reason: "back across the move".to_owned(),
            targets: Some(proto::force_request::Targets::Fleet(proto::FleetTarget {
                confirm_service: "web".to_owned(),
            })),
            include_held: false,
            host_group: String::new(),
        },
    )
    .await;
    assert_eq!(forced.code, 0, "{forced:?}");
    assert_eq!(
        ordered(&rig, &moved).await,
        Some(("force", "1.0".to_owned(), ARCHIVE.to_owned(), digest('f')))
    );
}

/// Proves: over gRPC, CI sets `hostkeeper` with its native entry alone and with the archive entry
/// after it, and `web` with an `apt` entry whose hook has a boot step and no command; the set
/// with hostkeeper's archive entry first is refused `target_invalid`.
/// Catches: a release of the move refused by the coordinator, or hostkeeper's list taken in an
/// order no daemon updates itself from.
#[tokio::test]
async fn the_releases_of_the_move_are_accepted() {
    let services = format!(
        "{SERVICES}\n[[service]]\nname = \"hostkeeper\"\nroles = [\"daemon\"]\nstages = [100]\n"
    );
    let rig = Rig::new(
        "move-releases",
        Spec::recorded()
            .with_services(&services)
            .with_oidc_services(&["web"])
            .without_legacy(),
    )
    .routed();
    let own = || {
        let mut own = repository_entry("hostkeeper", "2.0-1", 'a');
        own.manifest.as_mut().expect("a manifest").run = vec!["hostkeeper.service".to_owned()];
        own
    };
    let archive = || entry("hostkeeper", ARCHIVE, 'f', 1_000);
    let operator = |version: &str, entries: Vec<Target>| {
        support::targets::set(
            "hostkeeper",
            version,
            BTreeMap::from([("linux-x86_64".to_owned(), list(entries))]),
        )
    };
    for (version, entries) in [("2.0", vec![own()]), ("2.1", vec![own(), archive()])] {
        let answered = grpc(
            &rig,
            SET_DESIRED,
            Some(OPERATOR),
            &operator(version, entries),
        )
        .await;
        assert_eq!(answered.code, 0, "{version}: {answered:?}");
    }
    let refused = grpc(
        &rig,
        SET_DESIRED,
        Some(OPERATOR),
        &operator("2.2", vec![archive(), own()]),
    )
    .await;
    assert_eq!(refused.refusal(), (3, "target_invalid"));

    let mut booted = apt("2.0-1", 'a');
    let hook = booted.manifest.as_mut().and_then(|m| m.hook.as_mut());
    let hook = hook.expect("a hook");
    assert_eq!(hook.command[0], "/usr/bin/web");
    hook.boot = Some("restore".to_owned());
    set(&rig, "2.0", x86(vec![booted, web(ARCHIVE, 'f')])).await;
}

/// The release `service` (`hello` or `hostkeeper`) sets for the Mac and the Linux host at once:
/// the `macos-arm64` list is the signed `.pkg` alone, as `cargo xtask release targets` writes it
/// (one artifact entry of `macos-pkg` at a `komira-testing` location, with a digest and a size,
/// no attestation, a manifest naming the package `dev.hostkeeper.<service>` and its installer
/// bounds, `run` the launchd labels of the service); `linux-x86_64` is the `apt` entry.
fn mac_release(service: &str, run: &[&str]) -> (BTreeMap<String, TargetList>, Target, Target) {
    let mut pkg = entry(service, "macos-pkg", 'd', 52_000_000);
    pkg.locations = vec![Location {
        store: "komira-testing".to_owned(),
        locator: format!("{service}/2.0/{service}.pkg"),
    }];
    let manifest = pkg.manifest.as_mut().expect("a manifest");
    manifest.package = Some(format!("dev.hostkeeper.{service}"));
    manifest.run = run.iter().map(|label| (*label).to_owned()).collect();
    let deb = repository_entry(service, "2.0-1", 'a');
    let targets = BTreeMap::from([
        ("linux-x86_64".to_owned(), list(vec![deb.clone()])),
        ("macos-arm64".to_owned(), list(vec![pkg.clone()])),
    ]);
    (targets, pkg, deb)
}

/// Proves: with a release whose `macos-arm64` list is the `macos-pkg` entry alone, for `hello`
/// (two run labels) and for `hostkeeper` (none): a Mac whose daemon announces
/// `[hostkeeper-archive, macos-pkg]` and runs no `service` is ordered `install` of the `.pkg`
/// entry, whole, with the `.pkg`'s digest as the package digest; one that runs it from
/// `hostkeeper-archive` is ordered `update` of the same entry (the move off the archive); a
/// Linux host of the same release is still ordered its `apt` entry.
/// Catches: a Mac held back though its daemon announces the installer, the move off the archive
/// not made to a list that has no archive entry to fall back to, or the Linux list disturbed by
/// the Mac's.
#[tokio::test]
async fn a_mac_that_announces_macos_pkg_is_picked_the_pkg_entry_and_a_linux_host_its_apt_entry() {
    for (service, run) in [
        ("hello", &["hello.a", "hello.b"][..]),
        ("hostkeeper", &[][..]),
    ] {
        let rig = mac_rig(&format!("mac-pkg-{service}"));
        let (targets, pkg, deb) = mac_release(service, run);
        set_mac(&rig, service, "2.0", targets).await;
        rig.at(LATER);

        let fresh = Daemon::new("mac1", MAC, &[ARCHIVE, "macos-pkg"]);
        let order = said(&grpc_beat(&rig, &fresh).await).expect("an install");
        assert_eq!(
            (order.0, order.2.as_str(), order.3.as_str(), order.5),
            ("install", "2.0", digest('d').as_str(), Some(pkg.clone())),
            "{service}: a Mac running nothing"
        );

        let archived =
            Daemon::new("mac2", MAC, &[ARCHIVE, "macos-pkg"]).running(runs(service, "1.0", None));
        let order = said(&grpc_beat(&rig, &archived).await).expect("an update");
        assert_eq!(
            (order.0, order.2.as_str(), order.3.as_str(), order.5),
            ("update", "2.0", digest('d').as_str(), Some(pkg.clone())),
            "{service}: a Mac running from archives"
        );

        let linux = Daemon::new("lin1", X86, &[ARCHIVE, "apt"]).running(runs(service, "1.0", None));
        let order = said(&grpc_beat(&rig, &linux).await).expect("an update");
        assert_eq!(
            (order.0, order.2.as_str(), order.3.as_str(), order.5),
            ("update", "2.0", digest('a').as_str(), Some(deb)),
            "{service}: a Linux host"
        );
    }
}

/// Proves: with the same release, a Mac whose daemon announces `hostkeeper-archive` alone (no
/// signers file, so no `macos-pkg`) is ordered nothing: `no_build_for_host` when it does not run
/// the service, `installer_dropped` when it runs it from archives, the reason `Releases.Status`
/// gives and, for the second, the host `Converge` names under `installer_dropped`.
/// Catches: a Mac sent an installer its daemon does not list, or one kept waiting with no reason
/// given.
#[tokio::test]
async fn a_mac_that_announces_no_macos_pkg_is_picked_nothing_with_the_reason_the_pick_gives() {
    for (service, run) in [
        ("hello", &["hello.a", "hello.b"][..]),
        ("hostkeeper", &[][..]),
    ] {
        let rig = mac_rig(&format!("mac-no-pkg-{service}"));
        let (targets, _, _) = mac_release(service, run);
        set_mac(&rig, service, "2.0", targets).await;
        rig.at(LATER);

        // `mac3` runs nothing, `mac4` runs the service from archives.
        let fresh = Daemon::new("mac3", MAC, &[ARCHIVE]);
        assert_eq!(said(&grpc_beat(&rig, &fresh).await), None, "{service}");
        let archived = Daemon::new("mac4", MAC, &[ARCHIVE]).running(runs(service, "1.0", None));
        assert_eq!(said(&grpc_beat(&rig, &archived).await), None, "{service}");
        assert_eq!(
            reason(&rig, service, "mac4").await,
            proto::GateReason::InstallerDropped,
            "{service}"
        );
        assert_eq!(
            reason(&rig, service, "mac3").await,
            proto::GateReason::NoBuildForHost,
            "{service}"
        );
        let convergence = converge(&rig, service).await;
        assert_eq!(convergence.installer_dropped, vec!["mac4".to_owned()]);
    }
}

/// The rig of the Mac tests: `hello` and `hostkeeper`, each a service of the `daemon` role.
fn mac_rig(name: &str) -> Rig {
    let services = format!(
        "{SERVICES}\n[[service]]\nname = \"hello\"\nroles = [\"daemon\"]\nstages = [100]\n\n\
         [[service]]\nname = \"hostkeeper\"\nroles = [\"daemon\"]\nstages = [100]\n"
    );
    Rig::new(
        name,
        Spec::recorded()
            .with_services(&services)
            .with_oidc_services(&["web"])
            .without_legacy(),
    )
    .routed()
}

/// Sets `service` at `version` with `targets`, as an operator does, with room for every host to
/// update at once.
async fn set_mac(rig: &Rig, service: &str, version: &str, targets: BTreeMap<String, TargetList>) {
    let mut request = support::targets::set(service, version, targets);
    request
        .desired
        .as_mut()
        .expect("a desired state")
        .max_updating = [("daemon".to_owned(), 16)].into_iter().collect();
    let answered = grpc(rig, SET_DESIRED, Some(OPERATOR), &request).await;
    assert_eq!(answered.code, 0, "{answered:?}");
}
