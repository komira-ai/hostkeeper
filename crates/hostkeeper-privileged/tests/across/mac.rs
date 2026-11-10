//! The Mac host of the cross-release rig: the daemon's own job `dev.hostkeeper.hostkeeper` under
//! the host's launchd, as the pkg leaves a Mac.
//!
//! **Archive orders still.** A Mac announces `macos-pkg` only with the signers file, which this
//! host does not have, and the pinned release on no Mac, so this host takes no native entry: this host and the Mac half of the rig (`super::archive`) are as they were before the
//! Linux half moved to native `.deb` orders (`super::deb`), and hostkeeper's package here is the
//! archive, set by its entry and the release index and taken through the entry.
//!
//! **The job.** Its plist is the daemon's own (`hostkeeper_launchd::daemon_plist` of
//! `DaemonJob::standard`'s label: `RunAtLoad` and `KeepAlive` true, so that the clean exit of a
//! self-update starts `current` again), at the place the pkg puts it ([`DAEMON_PLIST`]), its
//! program `current` of the binary directory, with the test's directories (the end-to-end rig's
//! tree, `super::super::e2e::mac_host`) and `--heartbeat-secs 1` after the verb, its output in two
//! files of the test's tree. hostkeeper's package carries no unit on a Mac, as a release's does
//! not.
//!
//! **The fallback.** The hand-over has no fallback job (docs/design/two-programs.md, section
//! 4.3): when this build fails its start, the released daemon execs it three times and then
//! settles the update failed, under the same job.
//!
//! These are fixed places of the Mac, which is why the rig runs only on a throwaway runner:
//! [`Host::new`] boots out a daemon job another test left and moves aside its plist (with the
//! end-to-end rig's own places, which [`MacHost`] takes over); the drop boots the job out,
//! removes its plist and puts back what was moved aside, failed or not. A failing test first
//! prints the daemon's output (the drop of [`MacHost`] prints it too, with the rest of its tree).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_launchd::launchctl::{JobStatus, bootout_args, bootstrap_args, print};
use hostkeeper_launchd::procid::darwin::DarwinPidInfo;
use hostkeeper_launchd::{DaemonJob, Label, Launchctl, PidInfo, daemon_plist};
use hostkeeper_steps::Layout;

use super::super::e2e::mac_host::{LAUNCHD_DIR, MacHost, launchctl};
use super::super::e2e::scenario::LOCAL;
use super::super::e2e::wait_for;

/// The daemon's own job.
fn daemon_label() -> Label {
    DaemonJob::standard().label
}

/// Where the pkg puts the daemon's plist.
fn daemon_plist_path() -> PathBuf {
    Path::new(LAUNCHD_DIR).join(format!("{}.plist", daemon_label()))
}

/// Boots `label` out of the system domain; errors are ignored (a job that is not loaded is
/// done).
fn bootout_quiet(label: &Label) {
    let _ = launchctl().run(&bootout_args(label));
}

/// The Mac host of one test (the module says what it is).
pub struct Host {
    root: MacHost,
    /// The daemon's plist, when one was there already, with where it was moved aside.
    saved: Option<(PathBuf, PathBuf)>,
}

impl Host {
    /// Boots out a daemon job another test left, takes over the end-to-end rig's places and the
    /// daemon's plist.
    ///
    /// # Panics
    /// The test is not root, or a place cannot be moved aside.
    pub fn new(tag: &str) -> Host {
        bootout_quiet(&daemon_label());
        let root = MacHost::new(tag);
        let place = daemon_plist_path();
        let saved = fs::symlink_metadata(&place).ok().map(|_| {
            let aside = PathBuf::from(format!(
                "{}.hk-x1-saved-{tag}-{}",
                place.display(),
                std::process::id()
            ));
            fs::rename(&place, &aside).unwrap_or_else(|e| {
                panic!("move {} aside to {}: {e}", place.display(), aside.display())
            });
            (place, aside)
        });
        Host { root, saved }
    }

    /// The end-to-end rig's tree and fixed places.
    pub fn root(&self) -> &MacHost {
        &self.root
    }

    /// hostkeeper's package carries no unit on a Mac.
    pub fn package_unit(_root: &MacHost) -> Option<String> {
        None
    }

    /// Stages `first` as version `version` of the binary directory, `current` naming it, puts the
    /// daemon's plist where the pkg does, bootstraps it and waits until
    /// launchd runs it and it has recorded the boot id ([`MacHost::wait_started`]).
    ///
    /// # Panics
    /// The binary cannot be staged, the job cannot be loaded, or the daemon never starts.
    pub fn start(&self, first: &Path, version: &str) {
        let layout = Layout::new(&binary_dir(&self.root.state()));
        layout
            .stage(version, first)
            .expect("stage the first binary");
        layout.switch(version).expect("switch to it");
        let job = DaemonJob {
            label: daemon_label(),
            program: layout.current_path(),
            stdout: self.root.base().join("daemon.out.log"),
            stderr: self.root.base().join("daemon.err.log"),
        };
        let rendered = daemon_plist(&job).expect("the daemon's plist");
        let verb = "<string>daemon</string>";
        assert_eq!(
            rendered.matches(verb).count(),
            1,
            "the daemon's plist runs `current daemon`:\n{rendered}"
        );
        let arg = |text: &str| format!("<string>{text}</string>");
        let path = |path: PathBuf| path.display().to_string();
        let mut argv = vec![arg("daemon")];
        for (flag, value) in [
            ("--config", path(self.root.config_path())),
            ("--state-dir", path(self.root.state())),
            ("--packages-dir", path(self.root.packages())),
            (
                "--facts-file",
                path(self.root.state().with_file_name("facts.json")),
            ),
            ("--heartbeat-secs", "1".to_owned()),
        ] {
            argv.push(arg(flag));
            argv.push(arg(&value));
        }
        let plist = daemon_plist_path();
        fs::write(&plist, rendered.replacen(verb, &argv.join("\n\t\t"), 1))
            .unwrap_or_else(|e| panic!("write {}: {e}", plist.display()));
        fs::set_permissions(&plist, fs::Permissions::from_mode(0o644)).expect("its mode");
        let loaded = launchctl()
            .run(&bootstrap_args(&plist))
            .expect("launchctl bootstrap ran");
        assert_eq!(
            loaded.code,
            Some(0),
            "bootstrap {}: {}",
            daemon_label(),
            loaded.stderr_text()
        );
        self.root.wait_started(&daemon_label());
    }

    /// Boots the daemon's job out, as an operator does before it changes the binary directory,
    /// and waits until the job has no process.
    ///
    /// # Panics
    /// The job still has a process after the tries.
    pub fn stop(&self) {
        bootout_quiet(&daemon_label());
        wait_for("the daemon job has no process", LOCAL, || {
            self.running().is_none()
        });
    }

    /// Bootstraps the daemon's plist again, on whatever `current` of the binary directory names
    /// now, and waits until launchd runs it and it has recorded the boot id
    /// ([`MacHost::wait_started`], as [`Host::start`] does).
    ///
    /// # Panics
    /// The job cannot be loaded, or the daemon never starts.
    pub fn start_again(&self) {
        let plist = daemon_plist_path();
        let loaded = launchctl()
            .run(&bootstrap_args(&plist))
            .expect("launchctl bootstrap ran");
        assert_eq!(
            loaded.code,
            Some(0),
            "bootstrap {}: {}",
            daemon_label(),
            loaded.stderr_text()
        );
        self.root.wait_started(&daemon_label());
    }

    /// What the daemon job's process runs, while it has one.
    pub fn running(&self) -> Option<PathBuf> {
        match print(&launchctl(), &daemon_label()) {
            Ok(JobStatus::Loaded(info)) => info.pid.and_then(|pid| DarwinPidInfo.path(pid).ok()),
            _ => None,
        }
    }

    /// The daemon's output so far.
    pub fn logs(&self) -> String {
        let read = |name: &str| {
            let path = self.root.base().join(name);
            fs::read_to_string(&path).unwrap_or_else(|e| format!("({}: {e})", path.display()))
        };
        format!(
            "--- daemon stdout:\n{}\n--- daemon stderr:\n{}",
            read("daemon.out.log"),
            read("daemon.err.log")
        )
    }
}

impl Drop for Host {
    /// Boots the daemon's job out and puts the plist back; then [`MacHost`]'s
    /// drop does the same for its places. Every step goes on whatever the one before it did.
    fn drop(&mut self) {
        bootout_quiet(&daemon_label());
        let _ = fs::remove_file(daemon_plist_path());
        if let Some((place, aside)) = &self.saved {
            let _ = fs::rename(aside, place);
        }
    }
}
