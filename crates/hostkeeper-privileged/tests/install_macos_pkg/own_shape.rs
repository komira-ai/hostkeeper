//! A package shaped like hostkeeper's own (a job's plist in the payload, a manifest whose `run`
//! names no job) through the `macos-pkg` installer's `activate` on the Mac's real `installer`,
//! `pkgutil` and `launchctl`, and what launchd's override database says of its job after each
//! step.
//!
//! hostkeeper's Mac package ships the daemon's plist and names no `run`. An `activate` that
//! disabled every job of the kept version's payload that the new order's `run` did not name
//! marked that job disabled at each self-update over a kept version and at each rollback; the
//! running daemon kept running, and after a reboot launchd did not load it. These tests drive
//! that same `activate` (a kept previous version, `Replaced`) over real packages of that shape,
//! with a job of a label of their own, never the daemon's, and read `launchctl print-disabled
//! system` and `launchctl print system/<label>` after each step. A hosted runner cannot be
//! rebooted, so the override, which decides what a reboot loads, is what they read: they are
//! the release check, in `release.yml`'s `macos-device`, that a self-update leaves the job
//! enabled.
//!
//! - `a_package_of_hostkeepers_shape_leaves_its_job_not_disabled_and_loaded_across_updates_and_rollbacks`:
//!   1.0.0 installed, 2.0.0 over the kept 1.0.0, 3.0.0 over the kept 2.0.0, then back to 2.0.0
//!   and to 1.0.0, each over the kept version it replaces; after each step the job is not
//!   disabled and still loaded.
//! - `an_activate_leaves_a_job_the_manifest_never_ran_as_it_was_and_enable_clears_a_disable`: a
//!   job an earlier release's `activate` left disabled stays so across an `activate` of this
//!   shape (it ran no `launchctl` verb on it), and `MacosPkg::enable` makes it enabled.
//! - `a_job_the_previous_version_ran_stays_enabled_while_the_new_version_ships_its_plist`: a
//!   label the kept version's `run` named and the new one's does not is not disabled while the
//!   new version still installs its plist (the second test shows the reads here see a disabled
//!   job).

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::Tool;
use hostkeeper_install::macos_pkg::signers::{Fingerprint, Signers};
use hostkeeper_install::macos_pkg::{Bounds, Order, Replaced};
use hostkeeper_launchd::launchctl::{bootout_args, enable_args};
use hostkeeper_launchd::{Label, Launchctl, LaunchctlProcess};

use super::launchd_override::{Override, assert_not_disabled, is_loaded, override_of};
use super::{LEAF, Rig, TOOL_BOUND, receipt_version_of, require_root, run, scratch};

#[path = "../shared/pkg_build.rs"]
mod pkg_build;

use pkg_build::{PkgFile, PkgSource};

const SERVICE: &str = "hk-own-shape";
const IDENTIFIER: &str = "dev.hostkeeper.ci-own-shape";
const SUPPORT: &str = "/Library/Application Support/hk-own-shape";
/// The program of the job: a payload file, which the plist names.
const PROGRAM: &str = "/Library/Application Support/hk-own-shape/bin/program";

/// The label of the job of the test `tag`: of the process, so that a job a run left on a host
/// is never another run's.
fn job_label(tag: &str) -> Label {
    Label::parse(&format!(
        "dev.hostkeeper.ci-own-{tag}-{}",
        std::process::id()
    ))
    .expect("a job label")
}

/// Where `label`'s plist is installed.
fn plist_path(label: &Label) -> String {
    format!("/Library/LaunchDaemons/{label}.plist")
}

/// The plist of a job that is loaded and does not run: its program is a file of the package,
/// as in hostkeeper's own package, where it is the daemon's binary.
fn job_plist(label: &Label) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
         \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n<key>Label</key><string>{label}</string>\n\
         <key>ProgramArguments</key><array><string>{PROGRAM}</string></array>\n\
         <key>RunAtLoad</key><false/>\n<key>KeepAlive</key><false/>\n</dict></plist>\n"
    )
}

/// What the test changes on the host outside its scratch directory, undone on drop (a failed
/// test too): the job, its plist, the package's files and receipt. launchd keeps no way to
/// delete an override, so the job's, which the label (of this process) alone names, is left
/// enabled. Nothing here may panic.
struct Undo(Label);

impl Drop for Undo {
    fn drop(&mut self) {
        let launchctl = LaunchctlProcess::system(TOOL_BOUND);
        let _ = launchctl.run(&bootout_args(&self.0));
        let _ = launchctl.run(&enable_args(&self.0));
        let forget = ["--forget".to_owned(), IDENTIFIER.to_owned()];
        let _ = LaunchctlProcess::new(Path::new(Tool::Pkgutil.path()), TOOL_BOUND).run(&forget);
        let _ = fs::remove_dir_all(SUPPORT);
        let _ = fs::remove_file(plist_path(&self.0));
    }
}

/// The order of `version`, running `run`.
fn order(version: &str, run: &[Label]) -> Order {
    Order {
        service: ServiceName::new(SERVICE).expect("service name"),
        version: Version::new(version).expect("version"),
        identifier: IDENTIFIER.to_owned(),
        run: run.to_vec(),
        bounds: Bounds {
            stage: TOOL_BOUND,
            activate: TOOL_BOUND,
            remove: TOOL_BOUND,
        },
        hook: None,
    }
}

/// A `.pkg` of the shape of hostkeeper's own at `version`, built in `dir` with the Mac's
/// `pkgbuild` and `productbuild`: the program, and the plist of `label`'s job.
fn package(dir: &Path, version: &str, label: &Label) -> PathBuf {
    pkg_build::build(
        &dir.join(format!("build-{version}")),
        &PkgSource {
            identifier: IDENTIFIER.to_owned(),
            version: version.to_owned(),
            files: vec![
                PkgFile::new(
                    PROGRAM.trim_start_matches('/'),
                    format!("#!/bin/sh\nexit 0\n# {version}\n"),
                    0o755,
                ),
                PkgFile::new(
                    plist_path(label).trim_start_matches('/'),
                    job_plist(label),
                    0o644,
                ),
            ],
            postinstall: None,
        },
    )
}

/// A version staged: its order and its version directory, as a core keeps it.
struct Kept {
    order: Order,
    dir: PathBuf,
}

impl Kept {
    /// What an `activate` of the version that replaces `self` is given.
    fn replaced(&self) -> Replaced<'_> {
        Replaced {
            dir: &self.dir,
            run: &self.order.run,
        }
    }
}

/// The test's host: the installer's ports on the real Mac, the signers and the scratch
/// directory the packages are built in.
struct Host {
    rig: Rig,
    signers: Signers,
    scratch: PathBuf,
    label: Label,
}

impl Host {
    /// A host with none of the package's files, receipt or job (which the test's `Undo` takes
    /// away at its end).
    fn new(scratch: &Path, label: Label) -> Host {
        assert!(
            fs::symlink_metadata(SUPPORT).is_err()
                && fs::symlink_metadata(plist_path(&label)).is_err(),
            "the package is not on the host to begin with"
        );
        assert_eq!(
            receipt_version_of(IDENTIFIER),
            None,
            "no receipt of the package to begin with"
        );
        Host {
            rig: Rig::new(scratch),
            signers: Signers::new(vec![Fingerprint::parse(LEAF).expect("64 hex")]).expect("one"),
            scratch: scratch.to_owned(),
            label,
        }
    }

    /// Builds `version` of the package and stages it, its manifest naming `run`.
    fn stage(&self, version: &str, run: &[Label]) -> Kept {
        let package = package(&self.scratch, version, &self.label);
        self.rig.tools.answer_for(&package);
        let order = order(version, run);
        let dir = self
            .rig
            .stage(&self.signers, &order, &package)
            .unwrap_or_else(|(failure, _)| panic!("stage {version}: {failure}"));
        Kept { order, dir }
    }

    /// `activate` of `version` over `from` (a first install when `None`).
    fn activate(&self, version: &Kept, from: Option<&Kept>) {
        self.rig
            .installer(&self.signers)
            .activate(&version.order, &version.dir, from.map(Kept::replaced))
            .unwrap_or_else(|failure| {
                panic!("activate {}: {failure}", version.order.version.as_str())
            });
    }

    /// Loads the job from the plist the package installed, as the package's `postinstall`
    /// does on a Mac that does not hold it, and as a boot does.
    fn load_job(&self) {
        run(
            "/bin/launchctl",
            &["bootstrap", "system", &plist_path(&self.label)],
        );
        assert!(is_loaded(&self.label), "{} is loaded", self.label);
    }

    /// The package is installed at `version` with its plist and program, and the job is not
    /// disabled by launchd (no override, or an enabling one: the next boot loads it) and still
    /// loaded; `when` says what was done.
    fn assert_job_usable(&self, version: &str, when: &str) {
        assert_eq!(
            receipt_version_of(IDENTIFIER).as_deref(),
            Some(version),
            "{when}: the receipt"
        );
        let plist = fs::read_to_string(plist_path(&self.label))
            .unwrap_or_else(|e| panic!("{when}: the job's plist is not installed: {e}"));
        assert!(plist.contains(self.label.as_str()), "{when}: {plist}");
        assert_eq!(
            fs::read_to_string(PROGRAM).expect("the program is installed"),
            format!("#!/bin/sh\nexit 0\n# {version}\n"),
            "{when}: the program is the version's"
        );
        assert_not_disabled(&self.label, when);
        assert!(
            is_loaded(&self.label),
            "{when}: launchd no longer holds {}",
            self.label
        );
    }
}

/// Proves, on the Mac's real `installer`, `pkgutil` and `launchctl`, that a package of
/// hostkeeper's own shape (the plist of a job in the payload, a manifest that names no `run`)
/// installed, updated twice over the kept previous version and rolled back twice leaves its
/// job not disabled in launchd's override database (no mark, or an enabling one) and still
/// loaded after each step: the `activate` an update runs over a kept version and a rollback
/// runs marks no job disabled that the replaced version's `run` did not name.
///
/// Catches: an `activate` that disables each job of the kept version's payload the new order's
/// `run` does not name, which leaves hostkeeper's own job marked disabled by each self-update
/// over a kept version and each rollback, so that launchd does not load it at the next boot.
#[test]
fn a_package_of_hostkeepers_shape_leaves_its_job_not_disabled_and_loaded_across_updates_and_rollbacks()
 {
    require_root();
    let label = job_label("own");
    let _undo = Undo(label.clone());
    let dir = scratch("install-macos-pkg-own-shape");
    let host = Host::new(dir.path(), label);
    assert_not_disabled(&host.label, "before the first install");

    let v1 = host.stage("1.0.0", &[]);
    host.activate(&v1, None);
    host.load_job();
    host.assert_job_usable("1.0.0", "after the first install");

    let v2 = host.stage("2.0.0", &[]);
    host.activate(&v2, Some(&v1));
    host.assert_job_usable("2.0.0", "after the update from 1.0.0 to 2.0.0");

    let v3 = host.stage("3.0.0", &[]);
    host.activate(&v3, Some(&v2));
    host.assert_job_usable("3.0.0", "after the update from 2.0.0 to 3.0.0");

    host.activate(&v2, Some(&v3));
    host.assert_job_usable("2.0.0", "after the rollback from 3.0.0 to 2.0.0");

    host.activate(&v1, Some(&v2));
    host.assert_job_usable("1.0.0", "after the rollback from 2.0.0 to 1.0.0");
}

/// Proves that a job an earlier release's `activate` left disabled stays as it was across an
/// `activate` of a package of this shape over its kept version (the `activate` runs no
/// `launchctl` verb on a job no `run` names), that the job is then still loaded, and that
/// `MacosPkg::enable`, which the daemon runs
/// after the `activate` of its own package, makes launchd list it enabled (an override that
/// says so, not only no override).
///
/// Catches: an `enable` that exits 0 and leaves the job disabled, and an `activate` that
/// enables or disables a job its manifest never named.
#[test]
fn an_activate_leaves_a_job_the_manifest_never_ran_as_it_was_and_enable_clears_a_disable() {
    require_root();
    let label = job_label("dis");
    let _undo = Undo(label.clone());
    let dir = scratch("install-macos-pkg-own-shape-enable");
    let host = Host::new(dir.path(), label);

    let v1 = host.stage("1.0.0", &[]);
    host.activate(&v1, None);
    host.load_job();
    host.assert_job_usable("1.0.0", "after the first install");

    // What the `activate` of an earlier release left on a Mac of the farm.
    run(
        "/bin/launchctl",
        &["disable", &format!("system/{}", host.label)],
    );
    assert_eq!(
        override_of(&host.label),
        Override::Disabled,
        "launchctl disable marks the job disabled, and the listing says so"
    );

    let v2 = host.stage("2.0.0", &[]);
    host.activate(&v2, Some(&v1));
    assert_eq!(
        override_of(&host.label),
        Override::Disabled,
        "the update of a package that names no run left the override as it was"
    );
    assert!(is_loaded(&host.label), "a disabled job stays loaded");

    host.rig
        .installer(&host.signers)
        .enable(&host.label, TOOL_BOUND)
        .unwrap_or_else(|failure| panic!("enable: {failure}"));
    assert_eq!(
        override_of(&host.label),
        Override::Enabled,
        "enable clears the disable with an enabling override"
    );
    host.assert_job_usable("2.0.0", "after the enable");
}

/// Proves that a label the kept version's `run` named and the new version's does not is not
/// disabled by the `activate` over it while the new version still installs its plist: the
/// override stays the enabling one the first `activate` wrote.
///
/// Catches: an `activate` that disables a job whose plist is still installed because a `run`
/// list left it out, the shape that disabled the daemon's own job at each self-update.
#[test]
fn a_job_the_previous_version_ran_stays_enabled_while_the_new_version_ships_its_plist() {
    require_root();
    let label = job_label("run");
    let _undo = Undo(label.clone());
    let dir = scratch("install-macos-pkg-own-shape-run");
    let host = Host::new(dir.path(), label);

    let v1 = host.stage("1.0.0", std::slice::from_ref(&host.label));
    host.activate(&v1, None);
    assert_eq!(
        override_of(&host.label),
        Override::Enabled,
        "the activate enabled the job the order ran"
    );

    let v2 = host.stage("2.0.0", &[]);
    host.activate(&v2, Some(&v1));
    assert_eq!(
        override_of(&host.label),
        Override::Enabled,
        "the job version 1.0.0 ran and 2.0.0 still installs is not disabled"
    );
    assert_eq!(receipt_version_of(IDENTIFIER).as_deref(), Some("2.0.0"));
}
