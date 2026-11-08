#![cfg(all(any(target_os = "linux", target_os = "macos"), feature = "privileged"))]
//! hostkeeper's update of itself across releases, as root, from the published release that
//! `previous-release.toml` pins to this build: on Linux under the real systemd of a hosted runner
//! (`.github/workflows/privileged.yml`'s job `privileged`), on a Mac under the real launchd
//! (its job `macos-device`), and in pr-device.yml's copies of both jobs on a pull request (the
//! macOS one when the pull request changes a path the macOS jobs build or read).
//! The rig (`across`, on the end-to-end rig of `e2e`) runs the released daemon as the host's
//! daemon, the real coordinator in process ordering hostkeeper, and this build's hostkeeper
//! packages from a release channel on loopback, checked by the production attestation verifier.
//!
//! Each test starts from the released daemon converged on its own version's package
//! (`across::Rig::installed`: the coordinator's install of the version that runs). The
//! coordinator is set as CI's set does it, by targets and the release index (`across::Rig::order`):
//! the released daemon, which announces no `target-v1`, is ordered the package and the digest of
//! the index and fetches the index and the archive from the channel; this build, which does, is
//! ordered the one `hostkeeper-archive` entry and fetches through it. What each proves:
//! - the released daemon updates to this build (probe, hook, switch, clean exit), the unit or job
//!   starts this build, which settles the update `Succeeded`, reports it, and the coordinator
//!   reads the host converged on it;
//! - this build made to fail its start is fallen back from by the released binary's own
//!   fallback (its fallback unit under systemd, its fallback job under launchd): the released
//!   binary runs again, settles the update `Failed`, `self_update_fallback`, and lists the version
//!   bad, and its heartbeat reports that verdict;
//! - this build, once the released daemon updated to it, updates to itself under a newer version
//!   name;
//! - the released binary, put back by an operator on a host this build updated twice (stopped,
//!   `current` pointed at the released binary, started), starts on the packages this build staged
//!   through an entry, hostkeeper's own sealed version with its `bin/hostkeeper` and the unit
//!   linked from `current/units/`: it reports the version they hold, leaves them as they are, and
//!   updates the host again;
//! - the released binary started on a host that also holds versions of another installer than
//!   `hostkeeper-archive` (a `target.json`, an `artifact` and a `tree/`, as a later daemon stages
//!   them), sealed and not sealed, runs on, reports itself and hostkeeper, updates the host, and
//!   leaves those versions byte for byte as they were.
//!
//! The mixed cases of the design stop at hostkeeper's own release: the rig's coordinator manages
//! one service, so rolling back to and uninstalling a second service's archive version with the
//! released binary is not run here (a service the rig cannot order is only laid down, in the last
//! test).
//!
//! A pinned tarball that is missing or differs from the pin fails every test by name before the
//! host is touched (`across::released_binary`). Every wait is a bounded count of tries of an
//! explicit state; nothing is asserted about time.

mod across;
mod e2e;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use across::{Build, Built, FALLBACK_REASON, HK, NAMED, Rig, newer};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::record::{Record, State};
use hostkeeper_steps::{Layout, Seal, tree_digest};

/// The record of an update to `version` that ended with `verdict` and `reason`, and waits for
/// nothing.
fn assert_finished(rec: &Record, version: &str, verdict: Verdict, reason: Option<&str>) {
    assert_eq!(rec.target_version, version, "{rec:?}");
    assert_eq!(
        rec.state,
        State::Finished {
            verdict,
            reason: reason.map(str::to_owned),
        },
        "{rec:?}"
    );
    assert_eq!(rec.progress.awaiting_restart, None, "{rec:?}");
}

fn succeeded() -> Verdict {
    Verdict::Succeeded {
        promotion_pending: false,
    }
}

/// How many times the channel was asked for the index of `built`, for its archive and for its
/// bundle.
fn fetches(rig: &Rig, built: &Built) -> (u64, u64, u64) {
    let archive = format!("/{}", built.archive_locator());
    let bundle = format!("/{}", built.bundle_locator());
    (
        rig.channel.count(&built.index_path()),
        rig.channel.count(&archive),
        rig.channel.count(&bundle),
    )
}

/// Updates the rig's host to this build at `version` and checks that the update went through as
/// a self-update does: the coordinator reads the host converged on it, the daemon's process runs
/// the binary directory's file of `version`, `current` names it and `previous` names `from`, the
/// step record says `Succeeded`, and the host reported the update `Succeeded` with its order.
fn update_to_this_build(rig: &Rig, from: &str, version: &str) -> Built {
    let built = rig.publish(version, Build::This);
    rig.order(&built);
    rig.wait_converged(version);
    rig.wait_running(version);
    assert_eq!(
        rig.links(),
        (Some(version.to_owned()), Some(from.to_owned())),
        "{}",
        rig.host.logs()
    );
    let rec = rig.settled(version);
    assert_finished(&rec, version, succeeded(), None);
    let update = rig.wait_reported(version, "OUTCOME_SUCCEEDED");
    assert!(update["orderId"].is_string(), "{update}");
    built
}

/// Catches a release of this build that a daemon of the pinned release cannot update to: a probe
/// answer it does not accept, a hook it cannot run, a package or attestation it refuses, a new
/// binary that does not start under the old daemon's unit or job, or a settlement or report the
/// coordinator does not read as converged. The released daemon, converged on its own version, is
/// ordered this build's next version; the host converges on it, the unit or job runs it, the
/// binary directory names it `current` and the release `previous`, and the update is
/// `Succeeded`. The set is by targets and the index, and the released daemon, which knows no
/// entry, reads the index of the version and fetches its archive and bundle from the channel:
/// the previous binary updated by a coordinator that sets by targets.
#[test]
fn the_released_daemon_updates_to_this_build_and_this_build_settles_it_succeeded() {
    let rig = Rig::installed("x1upd");
    let released = rig.released.clone();
    rig.wait_running(&released);
    assert_eq!(rig.links(), (Some(released.clone()), None));
    let built = update_to_this_build(&rig, &released, &newer(&released, 1));
    let (index, archive, bundle) = fetches(&rig, &built);
    assert!(
        index > 0 && archive > 0 && bundle > 0,
        "the released daemon fetched the index, the archive and the bundle: {:#?}",
        rig.channel.requests()
    );
}

/// Catches a release of this build that cannot start leaving the host without a daemon, or
/// fallen back from by anything but the released binary's own fallback: the released daemon is
/// ordered a version of this build that exits 1 at its start; the released binary runs again,
/// `current` and `previous` both name it (the fallback moves `current` back and leaves
/// `previous`), the step record says `Failed` after the commit, of class release,
/// `self_update_fallback`, the version is on the local bad list, and the host's heartbeat reports
/// that verdict to the coordinator; on Linux systemd ran the fallback unit to success. The set is
/// by targets and the index, the released daemon fetching the failing version through the
/// channel's index.
#[test]
fn this_build_made_to_fail_its_start_is_fallen_back_from_to_the_released_binary() {
    let rig = Rig::installed("x1fbk");
    let released = rig.released.clone();
    let failing = newer(&released, 1);
    let built = rig.publish(&failing, Build::Failing);
    rig.order(&built);

    let update = rig.wait_reported(&failing, "OUTCOME_FAILED");
    assert_eq!(update["reason"], FALLBACK_REASON, "{update}");
    assert_eq!(update["class"], "FAILURE_CLASS_RELEASE", "{update}");
    rig.wait_running(&released);
    assert_eq!(
        rig.links(),
        (Some(released.clone()), Some(released.clone())),
        "{}",
        rig.host.logs()
    );
    let rec = rig.settled(&failing);
    assert_finished(
        &rec,
        &failing,
        Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release,
        },
        Some(FALLBACK_REASON),
    );
    assert!(rig.is_bad(&failing), "{failing} is on the bad list");
    #[cfg(target_os = "linux")]
    assert!(
        rig.host.fallback_ran(),
        "systemd ran the released binary's fallback unit\n{}",
        rig.host.logs()
    );

    rig.wait_running(&released);
}

/// Catches a release of this build that installs over the release but cannot update itself
/// again: the released daemon updates to this build's next version, and that version, ordered
/// the version after it, updates to it (this build staged under a newer name); the host converges
/// on each, `previous` names the version it came from, and each update is `Succeeded`. The set is
/// by targets and the index, and the two updates take different ways: the released daemon reads
/// the index of its version from the channel, while this build, ordered the entry, fetches the
/// archive and the bundle at the entry's locations and asks the channel for no index of its
/// version. Catches: this build ignoring the entry it is ordered and updating by the index, as
/// the released daemon does, or not announcing `target-v1` so that the coordinator orders it as
/// it orders the released one.
#[test]
fn this_build_updates_to_itself_under_a_newer_version_name() {
    let rig = Rig::installed("x1self");
    let released = rig.released.clone();
    let next = newer(&released, 1);
    let after = newer(&released, 2);
    let first = update_to_this_build(&rig, &released, &next);
    let (index, ..) = fetches(&rig, &first);
    assert!(
        index > 0,
        "the released daemon fetched the index of {next}: {:#?}",
        rig.channel.requests()
    );
    let second = update_to_this_build(&rig, &next, &after);
    let (index, archive, bundle) = fetches(&rig, &second);
    assert_eq!(
        index,
        0,
        "this build fetched no index of {after}: {:#?}",
        rig.channel.requests()
    );
    assert!(
        archive > 0 && bundle > 0,
        "this build fetched the archive and the bundle of {after} at the entry's locations: {:#?}",
        rig.channel.requests()
    );
}

/// Checks that `version` of hostkeeper is on the host as this build stages a release it was
/// ordered: the package directory's `current` names it, its seal is beside it, its
/// `bin/hostkeeper` and the binary directory's file of the version are this build's bytes, and on
/// Linux the unit the host links is the one under `current/units/`. Gives the tree's digest.
fn assert_staged_by_this_build(rig: &Rig, version: &str) -> String {
    let packages = rig.host.root().packages().join(HK);
    let layout = Layout::package(&packages);
    assert_eq!(
        layout.current().expect("read current").as_deref(),
        Some(version),
        "{}",
        rig.host.logs()
    );
    assert!(
        layout.seal_path(version).is_file(),
        "{version} is sealed in {}",
        packages.display()
    );
    let this_build = fs::read(NAMED).expect("this build's binary");
    assert!(
        fs::read(layout.version_path(version).join("bin/hostkeeper")).expect("bin/hostkeeper")
            == this_build,
        "the sealed tree of {version} holds this build's bin/hostkeeper"
    );
    assert!(
        fs::read(rig.layout().version_path(version)).expect("the staged binary") == this_build,
        "the binary directory holds this build's file as {version}"
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        fs::read_link(across::linux::LINKED_UNIT).ok(),
        Some(packages.join("current/units/hostkeeper.service")),
        "the host links the unit of the package's current"
    );
    tree_digest(&layout.version_path(version)).expect("the tree digest")
}

/// Catches a version this build staged and sealed through an entry that the released binary
/// cannot read when an operator puts it back on the host (a seal or a tree it reads as
/// unreadable, a manifest it refuses, a link it removes, a record it rewrites), and a released
/// binary that, started on them, reports nothing or cannot update again. The released daemon
/// updates to this build, this build (ordered the entry) installs the version after it, and the
/// operator goes back to the released binary: the binary directory's `current` names it and
/// `previous` names the version this build staged; its heartbeat reports itself, and hostkeeper
/// at its own version (the service `hostkeeper` runs the binary `current` names); the sealed
/// tree, its seal, its `bin/hostkeeper` staging, the linked unit (Linux) and the record that says
/// that update `Succeeded` are as this build left them; and ordered the version after, by the
/// channel, it updates to it and this build runs again.
#[test]
fn the_released_binary_put_back_on_the_packages_this_build_staged_reports_them_and_updates_again() {
    let rig = Rig::installed("x1back");
    let released = rig.released.clone();
    let (first, staged, last) = (
        newer(&released, 1),
        newer(&released, 2),
        newer(&released, 3),
    );
    update_to_this_build(&rig, &released, &first);
    let by_entry = update_to_this_build(&rig, &first, &staged);
    let (index, ..) = fetches(&rig, &by_entry);
    assert_eq!(
        index,
        0,
        "this build installed {staged} through the entry: {:#?}",
        rig.channel.requests()
    );
    let digest = assert_staged_by_this_build(&rig, &staged);

    rig.go_back_to_released();
    rig.wait_running(&released);
    assert_eq!(
        rig.links(),
        (Some(released.clone()), Some(staged.clone())),
        "{}",
        rig.host.logs()
    );
    rig.wait_host_reports(&released, &released);
    assert_eq!(assert_staged_by_this_build(&rig, &staged), digest);
    let rec = rig.settled(&staged);
    assert_finished(&rec, &staged, succeeded(), None);

    update_to_this_build(&rig, &released, &last);
    assert_staged_by_this_build(&rig, &last);
}

/// The entries below `root`, each as a line of its path, its kind and mode, and its bytes or its
/// link, in path order: what "byte for byte" compares. A directory that is not there gives no
/// entries.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let rel = format!("{rel}/{}", name.to_string_lossy());
            let meta = fs::symlink_metadata(&path).expect("an entry's metadata");
            let mode = meta.permissions().mode() & 0o7777;
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).expect("a link");
                out.push((
                    format!("{rel} link"),
                    target.as_os_str().as_encoded_bytes().to_vec(),
                ));
            } else if meta.is_dir() {
                out.push((format!("{rel} dir {mode:o}"), Vec::new()));
                walk(&path, &rel, out);
            } else {
                out.push((
                    format!("{rel} file {mode:o}"),
                    fs::read(&path).expect("a file"),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

/// A file of `bytes`, mode `mode`.
fn put(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// A directory, mode `mode`.
fn directory(path: &Path, mode: u32) {
    fs::create_dir_all(path).unwrap_or_else(|e| panic!("make {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// The package directory of the service `service` under `packages` as a daemon that runs a
/// tarball installer leaves it: version `1.0.0` holding a `target.json`, the `artifact` it was
/// made from and a `tree/` with a file, and `current` naming it; with `sealed`, a seal beside the
/// version (of this build's format: the digest of the version directory) too. The released
/// binary knows none of these files.
fn lay_down_other_installer_version(packages: &Path, service: &str, sealed: bool) {
    let dir = packages.join(service);
    let version = dir.join("1.0.0");
    directory(&dir, 0o755);
    directory(&version.join("tree"), 0o755);
    put(
        &version.join("target.json"),
        br#"{"installer":"tarball","service":"hostkeeper-ci-other","version":"1.0.0"}"#,
        0o444,
    );
    put(
        &version.join("artifact"),
        b"an artifact of another installer",
        0o444,
    );
    put(
        &version.join("tree/hello.txt"),
        b"hello from the tree\n",
        0o444,
    );
    if sealed {
        let digest = tree_digest(&version).expect("the version's digest");
        let seal = Seal::new(&digest, &"ab".repeat(32)).expect("a seal");
        put(&dir.join("1.0.0.seal"), seal.to_json().as_bytes(), 0o644);
    }
    std::os::unix::fs::symlink("1.0.0", dir.join("current")).expect("link current");
}

/// Catches an earlier binary that, started on a host holding a version of another installer than
/// `hostkeeper-archive` (a `target.json`, an `artifact` and a `tree/` where it expects a
/// `manifest.toml`), refuses to start, stops reporting, uninstalls or prunes the version it does
/// not know, or rewrites a file of it. Two services' versions are laid down in the packages
/// directory while the daemon is stopped, one sealed and one not, each `current`; the released
/// binary starts on them, runs, reports itself and hostkeeper at the released version, takes the
/// update to this build and reports that; every entry of both directories (kind, mode, bytes,
/// links) is as it was laid down, after the released binary's start and after the update.
#[test]
fn the_released_binary_started_on_versions_of_another_installer_runs_on_and_leaves_them_untouched()
{
    let rig = Rig::installed("x1other");
    let released = rig.released.clone();
    rig.wait_running(&released);
    let packages = rig.host.root().packages();
    let others = ["other-sealed", "other-bare"];

    rig.host.stop();
    lay_down_other_installer_version(&packages, others[0], true);
    lay_down_other_installer_version(&packages, others[1], false);
    let before = others.map(|service| snapshot(&packages.join(service)));
    assert_eq!(before[0].len(), 7, "{:?}", before[0]);
    assert_eq!(before[1].len(), 6, "{:?}", before[1]);
    rig.host.start_again();

    rig.wait_running(&released);
    rig.wait_host_reports(&released, &released);
    assert_eq!(rig.links(), (Some(released.clone()), None));
    assert_eq!(
        others.map(|service| snapshot(&packages.join(service))),
        before
    );

    let next = newer(&released, 1);
    update_to_this_build(&rig, &released, &next);
    rig.wait_host_reports(&next, &next);
    assert_eq!(
        others.map(|service| snapshot(&packages.join(service))),
        before,
        "the versions of the other installer are as they were laid down\n{}",
        rig.host.logs()
    );
}
