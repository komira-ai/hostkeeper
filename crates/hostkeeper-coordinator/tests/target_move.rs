//! The native installers and the end of the move (docs/design/packages-native.md sections 10.6
//! and 10.7, build items A18 and A25): the pick of a host that runs a service from a native
//! installer, never an entry of another installer; the archive a host may still run a service
//! from is no installer a list is picked by, so such a host is picked nothing
//! (`installer_dropped`) and no pick ever returns an entry of `hostkeeper-archive`, whatever the
//! release lists and whatever the daemon lists; and the releases of the move, which a stored
//! release with archive entries still loads, and hostkeeper's own list with one is refused.
//!
//! The same pick for a Mac, whose list is the signed `.pkg` alone (`macos-pkg`, build item of
//! `release targets` for macOS): ordered to a Mac whose daemon announces `macos-pkg`, nothing to
//! one that does not.
//!
//! `apt` is the native installer here (a repository entry); `deb` and `tarball` are installers
//! of other formats, which are picked a host that runs them and no other.

mod support;

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target, TargetList};
use support::beats::{Daemon, FORCE, converge, grpc_beat, json_beat, reason, runs, said};
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

/// Proves: a host running `web` from `hostkeeper-archive` (its daemon names no installer for it)
/// is picked nothing though its daemon lists `apt`, whether the list names `apt` before or after
/// the archive entry: it is held `installer_dropped` and named by `Converge`; a host that runs
/// `web` from `apt` is ordered the `apt` entry of the next version, and a host that does not run
/// `web` and lists `apt` is ordered it as its install, none of them the archive entry.
/// Catches: a host picked the archive entry it runs, or picked another installer's entry than the
/// one it runs, which only an uninstall changes.
#[tokio::test]
async fn a_host_running_from_archives_is_picked_nothing_and_one_running_apt_is_picked_the_apt_entry()
 {
    for (name, entries) in [
        ("native-first", vec![apt("2.0-1", 'a'), web(ARCHIVE, 'f')]),
        ("native-after", vec![web(ARCHIVE, 'f'), apt("2.0-1", 'a')]),
    ] {
        let rig = rig(name);
        put_komira(&rig).await;
        set(&rig, "2.0", x86(entries)).await;
        rig.at(LATER);
        let archived = Daemon::new("m1", X86, &[ARCHIVE, "apt"]).running(runs("web", "1.0", None));
        assert_eq!(said(&grpc_beat(&rig, &archived).await), None, "{name}");
        assert_eq!(
            reason(&rig, "web", "m1").await,
            proto::GateReason::InstallerDropped,
            "{name}"
        );
        let convergence = converge(&rig, "web").await;
        assert_eq!(
            convergence.installer_dropped,
            vec!["m1".to_owned()],
            "{name}"
        );

        let running_apt =
            Daemon::new("m2", X86, &[ARCHIVE, "apt"]).running(runs("web", "1.0", Some("apt")));
        assert_eq!(
            ordered(&rig, &running_apt).await,
            Some(("update", "2.0".to_owned(), "apt".to_owned(), digest('a'))),
            "{name}"
        );
        let new = Daemon::new("i1", X86, &[ARCHIVE, "apt"]);
        assert_eq!(
            ordered(&rig, &new).await,
            Some(("install", "2.0".to_owned(), "apt".to_owned(), digest('a'))),
            "{name}"
        );
    }
}

/// Proves: no pick returns the archive entry of a stored release, whatever the daemon lists and
/// the host runs: with the list `[apt, hostkeeper-archive]`, a host whose daemon lists only
/// `hostkeeper-archive`, one that lists `deb` and `tarball`, one that lists `macos-pkg`, and a
/// daemon before `target-v1` (which counts as listing the archive alone) are each picked nothing
/// (`no_build_for_host` for the hosts that do not run `web`, `installer_dropped` for those that
/// run it from archives), and a host that does not run `web` and lists the archive and `apt` is
/// ordered the `apt` entry.
/// Catches: the archive entry of a release set before the archives ended picked for a daemon
/// that still lists the installer, which no daemon installs with now.
#[tokio::test]
async fn an_archive_entry_is_never_picked_whatever_the_daemon_lists() {
    let rig = rig("never-archive");
    put_komira(&rig).await;
    set(&rig, "2.0", x86(vec![apt("2.0-1", 'a'), web(ARCHIVE, 'f')])).await;
    rig.at(LATER);
    for (host, installers) in [
        ("n1", vec![ARCHIVE, "deb", "tarball"]),
        ("n2", vec![ARCHIVE, "macos-pkg"]),
        ("n3", vec![ARCHIVE]),
    ] {
        let running = Daemon::new(host, X86, &installers).running(runs("web", "1.0", None));
        assert_eq!(said(&grpc_beat(&rig, &running).await), None, "{host} runs");
        assert_eq!(
            reason(&rig, "web", host).await,
            proto::GateReason::InstallerDropped,
            "{host} runs"
        );
        let absent = format!("{host}-absent");
        let new = Daemon::new(&absent, X86, &installers);
        assert_eq!(said(&grpc_beat(&rig, &new).await), None, "{host} does not");
        assert_eq!(
            reason(&rig, "web", &absent).await,
            proto::GateReason::NoBuildForHost,
            "{host} does not"
        );
    }
    let old = Daemon::old("o1", X86).running(runs("web", "1.0", None));
    assert_eq!(said(&grpc_beat(&rig, &old).await), None);
    assert_eq!(
        reason(&rig, "web", "o1").await,
        proto::GateReason::NoBuildForHost
    );
    let new = Daemon::new("i1", X86, &[ARCHIVE, "apt"]);
    assert_eq!(
        ordered(&rig, &new).await,
        Some(("install", "2.0".to_owned(), "apt".to_owned(), digest('a')))
    );
}

/// Proves: with lists that hold no native entry its daemon lists (`[deb, hostkeeper-archive]`
/// for a daemon listing `deb`, `[tarball, hostkeeper-archive]` for one listing `tarball`), a
/// host running `web` from archives is picked nothing: an installer of another format never moves
/// it, and the archive entry is never picked.
/// Catches: a host moved to an installer other than the one it runs, or kept on the archive entry
/// it can no longer be updated with.
#[tokio::test]
async fn a_list_with_no_native_entry_its_daemon_lists_picks_an_archive_host_nothing() {
    for (name, other) in [("other-deb", "deb"), ("other-tarball", "tarball")] {
        let rig = rig(name);
        put_komira(&rig).await;
        set(&rig, "2.0", x86(vec![web(other, 'a'), web(ARCHIVE, 'f')])).await;
        rig.at(LATER);
        let daemon = Daemon::new("k1", X86, &[ARCHIVE, other]).running(runs("web", "1.0", None));
        assert_eq!(said(&grpc_beat(&rig, &daemon).await), None, "{name}");
        assert_eq!(
            reason(&rig, "web", "k1").await,
            proto::GateReason::InstallerDropped,
            "{name}"
        );
    }
}

/// Proves: a host running `web` from `apt` is never ordered the archive entry of a newer
/// version: with `[hostkeeper-archive, apt]` it is ordered the `apt` entry, and with
/// `[hostkeeper-archive]` alone it is ordered nothing, held `installer_dropped` and named by
/// `Converge`.
/// Catches: a moved host sent back to archives by an update.
#[tokio::test]
async fn a_host_running_apt_is_never_picked_the_archive_entry_of_a_newer_version() {
    let rig = rig("move-no-way-back");
    put_komira(&rig).await;
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

/// Proves: a host running `web` 2.0 from `apt`, when `web` goes back to 1.0 whose list has no
/// native entry (`[hostkeeper-archive]`), is picked nothing and held `installer_dropped`, over
/// gRPC and on the JSON route, which carries no order; when it goes back to 1.5, whose list has an
/// `apt` entry, it is ordered that one; a host whose daemon no longer lists `apt` is held
/// `installer_dropped` going back to 1.5.
/// Catches: a rollback that sends a host back to archives, which no version can be activated
/// from any more.
#[tokio::test]
async fn a_rollback_to_a_version_with_no_native_entry_picks_the_host_nothing() {
    let rig = rig("move-rollback");
    put_komira(&rig).await;
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

    back_to(&rig, "1.0", one).await;
    assert_eq!(said(&grpc_beat(&rig, &moved).await), None);
    assert_eq!(
        reason(&rig, "web", "b1").await,
        proto::GateReason::InstallerDropped
    );
    let answer = json_beat(&rig, &json).await;
    assert_eq!(answer["reply"], "nothing", "{answer}");

    back_to(&rig, "1.5", one_five).await;
    let moved_again =
        Daemon::new("b3", X86, &[ARCHIVE, "apt"]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(
        ordered(&rig, &moved_again).await,
        Some(("update", "1.5".to_owned(), "apt".to_owned(), digest('b')))
    );
    let unlisted = Daemon::new("b5", X86, &[ARCHIVE]).running(runs("web", "2.0", Some("apt")));
    assert_eq!(said(&grpc_beat(&rig, &unlisted).await), None);
    assert_eq!(
        reason(&rig, "web", "b5").await,
        proto::GateReason::InstallerDropped
    );
}

/// Proves: `Releases.Force` of `web` back to 1.0, whose list has no native entry, orders a host
/// running 2.0 from `apt` nothing: the host is skipped `installer_dropped`, and no order reaches
/// it.
/// Catches: the operator's way back to an archive entry, which no daemon installs.
#[tokio::test]
async fn a_force_back_to_a_version_with_no_native_entry_skips_the_host() {
    let rig = rig("move-force-back");
    put_komira(&rig).await;
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
            reason: "back to an archive".to_owned(),
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
    assert_eq!(skipped, [("f1", "installer_dropped")]);
    assert_eq!(said(&grpc_beat(&rig, &moved).await), None);
}

/// Proves: over gRPC, CI sets `hostkeeper` with its native entry alone, and `web` with an `apt`
/// entry whose hook has a boot step and no command beside an archive entry; a set of `hostkeeper`
/// with the archive entry after its native one, before it or alone is refused `target_invalid`
/// (A25 drops the archive half of its list).
/// Catches: hostkeeper's list taken with an archive entry, which no daemon installs, or its
/// native entry refused.
#[tokio::test]
async fn hostkeeper_is_set_with_its_native_entry_alone() {
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
    put_komira(&rig).await;
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
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(OPERATOR),
        &operator("2.0", vec![own()]),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    for (version, entries) in [
        ("2.1", vec![own(), archive()]),
        ("2.2", vec![archive(), own()]),
        ("2.3", vec![archive()]),
    ] {
        let refused = grpc(
            &rig,
            SET_DESIRED,
            Some(OPERATOR),
            &operator(version, entries),
        )
        .await;
        assert_eq!(refused.refusal(), (3, "target_invalid"), "{version}");
    }

    let mut booted = apt("2.0-1", 'a');
    let hook = booted.manifest.as_mut().and_then(|m| m.hook.as_mut());
    let hook = hook.expect("a hook");
    assert_eq!(hook.command[0], "/usr/bin/web");
    hook.boot = Some("restore".to_owned());
    set(&rig, "2.0", x86(vec![booted, web(ARCHIVE, 'f')])).await;
}

/// Proves: a release of `hostkeeper` stored with an archive entry after its native one, as a
/// coordinator before A25 accepted it, is read back whole after a restart and by `GetRelease`:
/// the read path does not judge the list again; and the same list set anew is refused
/// `target_invalid`.
/// Catches: a stored release of the move refused or dropped at start, which would lose a
/// version an operator can still roll back to, or the list rule applied to what is read.
#[tokio::test]
async fn a_stored_release_of_hostkeeper_with_an_archive_entry_still_loads() {
    let services = format!(
        "{SERVICES}\n[[service]]\nname = \"hostkeeper\"\nroles = [\"daemon\"]\nstages = [100]\n"
    );
    let mut rig = Rig::new(
        "move-stored",
        Spec::recorded()
            .with_services(&services)
            .with_oidc_services(&["web"])
            .without_legacy(),
    )
    .routed();
    put_komira(&rig).await;
    let mut own = repository_entry("hostkeeper", "2.0-1", 'a');
    own.manifest.as_mut().expect("a manifest").run = vec!["hostkeeper.service".to_owned()];
    let archive = entry("hostkeeper", ARCHIVE, 'f', 1_000);
    let release = support::targets::set(
        "hostkeeper",
        "2.0",
        BTreeMap::from([("linux-x86_64".to_owned(), list(vec![own.clone()]))]),
    );
    let answered = grpc(&rig, SET_DESIRED, Some(OPERATOR), &release).await;
    assert_eq!(answered.code, 0, "{answered:?}");

    // The releases file as a coordinator before A25 wrote it: the archive entry after the native.
    let path = rig.dir.path().join("desired.json.releases");
    let mut book: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the releases file"))
            .expect("JSON");
    let entries = book["services"]["hostkeeper"]["2.0"]["targets"]["linux-x86_64"]["entries"]
        .as_array_mut()
        .expect("entries");
    let stored_archive = serde_json::json!({
        "installer": ARCHIVE,
        "digest": archive.digest,
        "bytes": archive.bytes,
        "locations": [{"store": "github", "locator": archive.locations[0].locator}],
        "attestation": {
            "digest": archive.attestation.as_ref().expect("an attestation").digest,
            "bytes": archive.attestation.as_ref().expect("an attestation").bytes,
            "locations": [{"store": "github", "locator": archive.attestation.as_ref()
                .expect("an attestation").locations[0].locator}],
        },
        "manifest": {"minHostkeeper": "0.4.0"},
    });
    entries.push(stored_archive);
    std::fs::write(&path, serde_json::to_string(&book).expect("JSON")).expect("written");

    rig.restart();
    let read = grpc(
        &rig,
        support::targets::GET_RELEASE,
        Some(OPERATOR),
        &proto::GetReleaseRequest {
            service: "hostkeeper".to_owned(),
            version: "2.0".to_owned(),
        },
    )
    .await;
    assert_eq!(read.code, 0, "{read:?}");
    let read: proto::GetReleaseResponse = read.decoded();
    let installers: Vec<&str> = read.targets["linux-x86_64"]
        .entries
        .iter()
        .map(|e| e.installer.as_str())
        .collect();
    assert_eq!(installers, ["apt", ARCHIVE]);

    let again = support::targets::set(
        "hostkeeper",
        "2.1",
        BTreeMap::from([("linux-x86_64".to_owned(), list(vec![own, archive]))]),
    );
    let refused = grpc(&rig, SET_DESIRED, Some(OPERATOR), &again).await;
    assert_eq!(refused.refusal(), (3, "target_invalid"));
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
/// `hostkeeper-archive` is ordered nothing and held `installer_dropped`, the archive being no
/// installer a pick moves a host off (the move is over); a Linux host that runs the service from
/// `apt` is still ordered `update` of its `apt` entry.
/// Catches: a Mac held back though its daemon announces the installer, a host running from
/// archives moved to the `.pkg` entry, or the Linux list disturbed by the Mac's.
#[tokio::test]
async fn a_mac_that_announces_macos_pkg_is_picked_the_pkg_entry_and_a_linux_host_its_apt_entry() {
    for (service, run) in [
        ("hello", &["hello.a", "hello.b"][..]),
        ("hostkeeper", &[][..]),
    ] {
        let rig = mac_rig(&format!("mac-pkg-{service}"));
        put_komira(&rig).await;
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
        assert_eq!(
            said(&grpc_beat(&rig, &archived).await),
            None,
            "{service}: a Mac running from archives"
        );
        assert_eq!(
            reason(&rig, service, "mac2").await,
            proto::GateReason::InstallerDropped,
            "{service}"
        );

        let linux =
            Daemon::new("lin1", X86, &[ARCHIVE, "apt"]).running(runs(service, "1.0", Some("apt")));
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
        put_komira(&rig).await;
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
