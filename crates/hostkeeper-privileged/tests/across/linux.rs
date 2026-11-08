//! The Linux host of the cross-release rig: the daemon's own unit `hostkeeper.service` under the
//! host's systemd, as the deb leaves a host.
//!
//! **The unit.** The text is the package unit, `packaging/linux/units/hostkeeper.service`
//! ([`REAL_UNIT`]; the unit that starts `current`, because the released binary the rig pins
//! predates the packaged start, so the deb's bootstrap unit would not start it), with three
//! changes ([`unit_text`]): `ExecStart=` runs the same `current` of the binary directory with the
//! same flags, at the test's directories (the end-to-end rig's tree, `super::super::e2e::host_root`),
//! with the admin socket in the test's tree (`--admin-socket`, which the real line does not pass:
//! the older release would bind the default `/run/hostkeeper/admin.sock`; the daemon of this
//! checkout ignores the flag and binds nothing) and with `--heartbeat-secs 1`;
//! the seed (`ExecStartPre=`: the test stages the first binary itself) is left out. A change to
//! the real `ExecStart=` that this does not know fails the test by name. The unit is installed
//! where the deb installs it ([`DEB_UNIT`]) and enabled by the link `systemctl enable` makes
//! ([`WANTS_LINK`]); each hostkeeper package of the rig carries the same text as
//! `units/hostkeeper.service`, which the daemon links into `/etc/systemd/system` ([`LINKED_UNIT`])
//! in its place, as a release's unit takes the deb's place on a host.
//!
//! **The fallback.** The daemon writes its fallback unit (`hostkeeper-fallback.service`) and the
//! drop-in that names it into `/etc/systemd/system` at its start, as on any host; the released
//! binary's own texts are what systemd runs when this build fails its start.
//!
//! These are fixed places of the host, which is why the rig runs only on a throwaway runner:
//! [`Host::new`] moves aside whatever is at them (with the end-to-end rig's own, which
//! [`HostRoot`] takes over) and the drop stops the units, removes what the test and the daemon
//! put there and puts back what was moved aside, failed or not. A failing test first prints the
//! journal of both units and of hostkeeper's step units, and keeps them with the end-to-end
//! rig's artifacts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_steps::Layout;

use super::super::e2e::host_root::{HostRoot, systemctl};

/// The unit file of hostkeeper's package, which starts `current` (the bootstrap unit the deb, the
/// rpm and the boot image install starts the packaged binary).
pub const REAL_UNIT: &str = include_str!("../../../../packaging/linux/units/hostkeeper.service");
/// The `ExecStart=` line of [`REAL_UNIT`] that [`unit_text`] knows how to point at the test.
const REAL_EXEC_START: &str = "ExecStart=/var/lib/updater/state/hostkeeper/bin/current daemon --config /etc/hostkeeper/hostkeeper.toml --state-dir /var/lib/updater/state --packages-dir /var/lib/hostkeeper/packages --notify-systemd";
/// The daemon's own unit.
pub const MAIN: &str = "hostkeeper.service";
/// The fallback unit the daemon writes.
pub const FALLBACK: &str = "hostkeeper-fallback.service";
/// Where the deb installs [`MAIN`].
pub const DEB_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";
/// Where the daemon links the unit of hostkeeper's package.
pub const LINKED_UNIT: &str = "/etc/systemd/system/hostkeeper.service";
/// The link `systemctl enable` makes for [`DEB_UNIT`].
pub const WANTS_LINK: &str = "/etc/systemd/system/multi-user.target.wants/hostkeeper.service";
/// The glob of the transient units the daemon runs hostkeeper's hook steps in.
const STEP_UNITS: &str = "updater-hostkeeper-*.service";
/// The transient unit the released daemon probes a new binary's protocol in.
const PROBE: &str = "hostkeeper-self-update-protocol.service";
/// Where `journalctl` is.
const JOURNALCTL: &str = "/usr/bin/journalctl";

/// The places of the host this host takes over beside [`HostRoot`]'s.
fn taken_places() -> [PathBuf; 3] {
    [DEB_UNIT, LINKED_UNIT, WANTS_LINK].map(PathBuf::from)
}

/// `path` as text, for a unit file.
fn plain(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
        .to_owned()
}

/// The text of the daemon's unit for the end-to-end rig's tree `root` (the module says what
/// changes from [`REAL_UNIT`]).
///
/// # Panics
/// [`REAL_UNIT`]'s `ExecStart=` is not [`REAL_EXEC_START`].
pub fn unit_text(root: &HostRoot) -> String {
    let lines: Vec<&str> = REAL_UNIT.lines().collect();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("ExecStart="))
            .collect::<Vec<_>>(),
        [&REAL_EXEC_START],
        "packaging/linux/units/hostkeeper.service starts the daemon another way than this rig knows"
    );
    let exec_start = format!(
        "ExecStart={} daemon --config {} --state-dir {} --packages-dir {} --admin-socket {} \
         --notify-systemd --heartbeat-secs 1",
        plain(&Layout::new(&binary_dir(&root.state())).current_path()),
        plain(&root.config_path()),
        plain(&root.state()),
        plain(&root.packages()),
        plain(&root.admin_socket()),
    );
    let mut text = String::new();
    for line in lines {
        if [
            "ExecStartPre=",
            "RuntimeDirectory=",
            "RuntimeDirectoryMode=",
        ]
        .iter()
        .any(|key| line.starts_with(key))
        {
            continue;
        }
        text.push_str(if line == REAL_EXEC_START {
            &exec_start
        } else {
            line
        });
        text.push('\n');
    }
    text
}

/// The journal of `unit` (a name or a glob), or why it could not be read.
fn journal(unit: &str) -> String {
    match Command::new(JOURNALCTL)
        .args(["--no-pager", "--unit", unit])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => format!("cannot run {JOURNALCTL}: {e}"),
    }
}

/// The value of `property` of `unit`, as `systemctl show` prints it.
fn property(unit: &str, property: &str) -> String {
    let out = systemctl(&["show", "--value", &format!("--property={property}"), unit]);
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The Linux host of one test (the module says what it is).
pub struct Host {
    root: HostRoot,
    /// The places taken over that were there already, each with where it was moved aside.
    saved: Vec<(PathBuf, PathBuf)>,
}

impl Host {
    /// Stops any daemon unit another test left, takes over the end-to-end rig's places and this
    /// host's ([`taken_places`]).
    ///
    /// # Panics
    /// The test is not root under systemd, or a place cannot be moved aside.
    pub fn new(tag: &str) -> Host {
        let _ = systemctl(&["stop", MAIN, FALLBACK]);
        let root = HostRoot::new(tag);
        let pid = std::process::id();
        let saved = taken_places()
            .into_iter()
            .filter(|place| fs::symlink_metadata(place).is_ok())
            .map(|place| {
                let aside = PathBuf::from(format!("{}.hk-x1-saved-{tag}-{pid}", place.display()));
                fs::rename(&place, &aside).unwrap_or_else(|e| {
                    panic!("move {} aside to {}: {e}", place.display(), aside.display())
                });
                (place, aside)
            })
            .collect();
        Host { root, saved }
    }

    /// The end-to-end rig's tree and fixed places.
    pub fn root(&self) -> &HostRoot {
        &self.root
    }

    /// The unit hostkeeper's package carries: [`unit_text`].
    pub fn package_unit(root: &HostRoot) -> Option<String> {
        Some(unit_text(root))
    }

    /// Stages `first` as version `version` of the binary directory, `current` naming it, installs
    /// and enables the unit where the deb does, and starts it: `systemctl start` returns once the
    /// daemon is ready.
    ///
    /// # Panics
    /// The binary cannot be staged, or the unit does not start.
    pub fn start(&self, first: &Path, version: &str) {
        let layout = Layout::new(&binary_dir(&self.root.state()));
        layout
            .stage(version, first)
            .expect("stage the first binary");
        layout.switch(version).expect("switch to it");
        fs::write(DEB_UNIT, unit_text(&self.root))
            .unwrap_or_else(|e| panic!("write {DEB_UNIT}: {e}"));
        fs::set_permissions(DEB_UNIT, fs::Permissions::from_mode(0o644)).expect("its mode");
        let wants = Path::new(WANTS_LINK);
        fs::create_dir_all(wants.parent().expect("the wants directory"))
            .expect("the wants directory");
        std::os::unix::fs::symlink(DEB_UNIT, wants).expect("the deb's enable link");
        let _ = systemctl(&["daemon-reload"]);
        let started = systemctl(&["start", MAIN]);
        assert!(
            started.status.success(),
            "systemctl start {MAIN} failed: {}\n{}",
            String::from_utf8_lossy(&started.stderr),
            self.logs()
        );
    }

    /// What the daemon's unit's main process runs (its `/proc/<pid>/exe`), while it has one.
    pub fn running(&self) -> Option<PathBuf> {
        let pid = property(MAIN, "MainPID");
        match pid.parse::<u32>() {
            Ok(pid) if pid != 0 => fs::read_link(format!("/proc/{pid}/exe")).ok(),
            _ => None,
        }
    }

    /// Whether systemd ran the daemon's fallback unit, to its end with success.
    pub fn fallback_ran(&self) -> bool {
        !property(FALLBACK, "InvocationID").is_empty() && property(FALLBACK, "Result") == "success"
    }

    /// The journal of the daemon's unit, of its fallback, of the probe and of hostkeeper's step
    /// units.
    pub fn logs(&self) -> String {
        [MAIN, FALLBACK, PROBE, STEP_UNITS]
            .map(|unit| format!("--- journal of {unit}:\n{}", journal(unit)))
            .join("\n")
    }
}

impl Drop for Host {
    /// Puts the host back as the test found it; then [`HostRoot`]'s drop does the same for its
    /// places. Every step goes on whatever the one before it did.
    fn drop(&mut self) {
        if std::thread::panicking() {
            let logs = self.logs();
            eprintln!("the host when the test failed:\n{logs}");
            let dir = self.root.artifact_dir();
            if fs::create_dir_all(&dir).is_ok() {
                let _ = fs::write(dir.join("journal-hostkeeper.txt"), logs);
            }
        }
        // The fallback last: its `ExecStartPost=` starts the daemon's unit.
        let _ = systemctl(&["stop", MAIN, STEP_UNITS, PROBE, FALLBACK]);
        let _ = systemctl(&["stop", MAIN]);
        for place in taken_places() {
            let _ = fs::remove_file(&place);
        }
        for (place, aside) in &self.saved {
            let _ = fs::rename(aside, place);
        }
        let _ = systemctl(&["daemon-reload"]);
        let _ = systemctl(&["reset-failed", MAIN, FALLBACK]);
    }
}
