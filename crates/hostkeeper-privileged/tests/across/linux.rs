//! The Linux host of the cross-release rig: the daemon's own unit `hostkeeper.service` under the
//! host's systemd, as hostkeeper's `.deb` leaves a host.
//!
//! **The package.** hostkeeper is on this host as its `.deb` (`super::deb` builds it around the
//! binary of each version and installs the released version's with `dpkg -i`, which runs the
//! shipped `postinst`), so the unit, the tmpfiles rules, the pinned `gh` and the trusted root, the
//! template of the config and the issue link are the package's files at the places it installs
//! them, and the package is in the host's dpkg database ([`Host::package_version`],
//! [`Host::package_held`]). The drop purges it (the shipped `prerm` runs) and puts back what was
//! moved aside.
//!
//! **The unit.** The text is the package unit, `packaging/linux/units/hostkeeper.service`
//! ([`REAL_UNIT`]; the unit that starts `current`, because the rig stages the released binary as
//! `current` itself and does not run the `.deb`'s packaged start through `/usr/bin/hostkeeper`),
//! with one change ([`unit_text`]): `ExecStart=` runs the same `current` of the binary directory
//! with the same flags, at the test's directories (the end-to-end rig's tree,
//! `super::super::e2e::host_root`) and with `--heartbeat-secs 1`. A change to the real
//! `ExecStart=` that this does not know fails the test by name. Every `.deb` of the rig carries
//! this text as `usr/lib/systemd/system/hostkeeper.service` ([`DEB_UNIT`]), which `postinst`
//! enables ([`WANTS_LINK`]). hostkeeper's `.deb` is the only thing that installs a unit of
//! hostkeeper: the archive's unit is never linked into `/etc/systemd/system` ([`LINKED_UNIT`]),
//! so nothing is there.
//!
//! **The repository entries.** `/etc/hostkeeper/repositories/` is a fixed place of the daemon
//! (no flag): [`Host::new`] moves `/etc/hostkeeper` aside and `super::deb` makes the directory
//! with the rig's one entry and its keyring.
//!
//! **The fallback.** The hand-over has no fallback unit and no drop-in
//! (docs/design/two-programs.md, section 4.3): when this build fails its start, the released
//! daemon execs it three times and then settles the update failed, under the same unit.
//!
//! These are fixed places of the host, which is why the rig runs only on a throwaway runner:
//! [`Host::new`] moves aside whatever is at them (with the end-to-end rig's own, which
//! [`HostRoot`] takes over) and the drop stops the units, lets go of the package's hold,
//! purges the package, removes what the test, the package's scripts and the daemon put there and
//! puts back what was moved aside, failed or not. A failing test first prints the journal of the
//! daemon's unit and of hostkeeper's step units, and keeps them with the end-to-end rig's
//! artifacts.

use std::fs;
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
/// Where the deb installs [`MAIN`].
pub const DEB_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";
/// Where the daemon links the unit of hostkeeper's package.
pub const LINKED_UNIT: &str = "/etc/systemd/system/hostkeeper.service";
/// The link `systemctl enable` makes for [`DEB_UNIT`].
pub const WANTS_LINK: &str = "/etc/systemd/system/multi-user.target.wants/hostkeeper.service";
/// The package, as dpkg names it.
pub const PACKAGE: &str = "hostkeeper";
/// The directory the daemon reads its repository entries from, below the host's own
/// `/etc/hostkeeper`.
pub const REPOSITORIES_DIR: &str = hostkeeper_install::repository::config::REPOSITORIES_DIR;
/// The glob of the transient units the daemon runs hostkeeper's hook steps in.
const STEP_UNITS: &str = "updater-hostkeeper-*.service";
/// Where `journalctl` is.
const JOURNALCTL: &str = "/usr/bin/journalctl";

/// The places of the host this host takes over beside [`HostRoot`]'s: the package's unit and
/// its links, the daemon's configuration directory, and the directories `postinst`'s tmpfiles
/// rules make that no test of the rig uses.
fn taken_places() -> [PathBuf; 6] {
    [
        DEB_UNIT,
        LINKED_UNIT,
        WANTS_LINK,
        "/etc/hostkeeper",
        "/var/lib/updater",
        "/run/hostkeeper",
    ]
    .map(PathBuf::from)
}

/// Removes the file, the link or the directory tree at `place`, if there is one.
fn take_away(place: &Path) {
    match fs::symlink_metadata(place) {
        Ok(meta) if meta.is_dir() => {
            let _ = fs::remove_dir_all(place);
        }
        Ok(_) => {
            let _ = fs::remove_file(place);
        }
        Err(_) => {}
    }
}

/// Runs `program` with `args` as dpkg and apt tools are run in a script: no questions asked, the
/// locale the C one. Gives what it printed, and whether it ended 0.
fn run_tool(program: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"));
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
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
        "ExecStart={} daemon --config {} --state-dir {} --packages-dir {} \
         --notify-systemd --heartbeat-secs 1",
        plain(&Layout::new(&binary_dir(&root.state())).current_path()),
        plain(&root.config_path()),
        plain(&root.state()),
        plain(&root.packages()),
    );
    let mut text = String::new();
    for line in lines {
        if ["RuntimeDirectory=", "RuntimeDirectoryMode="]
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
        let _ = systemctl(&["stop", MAIN]);
        let (known, _) = run_tool("dpkg-query", &["-W", PACKAGE]);
        assert!(
            !known,
            "the host's dpkg database knows the package {PACKAGE}: the rig runs only on a host that \
             has never had hostkeeper's .deb"
        );
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

    /// Installs the `.deb` at `deb` with `dpkg -i`, which runs its `postinst`.
    ///
    /// # Panics
    /// `dpkg` fails; the message has what it printed.
    pub fn install_package(&self, deb: &Path) {
        let (ok, text) = run_tool("dpkg", &["-i", &plain(deb)]);
        assert!(ok, "dpkg -i {} failed:\n{text}", deb.display());
    }

    /// The version of the package when dpkg has it installed whole, `None` otherwise.
    pub fn package_version(&self) -> Option<String> {
        let (ok, text) = run_tool(
            "dpkg-query",
            &["-W", "-f=${Version}\\t${db:Status-Abbrev}\\n", PACKAGE],
        );
        let (version, status) = text.trim_end().split_once('\t').filter(|_| ok)?;
        // The abbreviation is the wanted state, the state and the error: `hi ` is installed and
        // held.
        (status.as_bytes().get(1) == Some(&b'i')).then(|| version.to_owned())
    }

    /// Whether the package is on apt's list of held packages.
    pub fn package_held(&self) -> bool {
        let (_, text) = run_tool("apt-mark", &["showhold"]);
        text.lines().any(|line| line == PACKAGE)
    }

    /// Stages `first` as version `version` of the binary directory, `current` naming it, and
    /// starts the unit the installed `.deb` enabled: `systemctl start` returns once the daemon is
    /// ready.
    ///
    /// # Panics
    /// The binary cannot be staged, or the unit does not start.
    pub fn start(&self, first: &Path, version: &str) {
        let layout = Layout::new(&binary_dir(&self.root.state()));
        layout
            .stage(version, first)
            .expect("stage the first binary");
        layout.switch(version).expect("switch to it");
        let _ = systemctl(&["daemon-reload"]);
        let started = systemctl(&["start", MAIN]);
        assert!(
            started.status.success(),
            "systemctl start {MAIN} failed: {}\n{}",
            String::from_utf8_lossy(&started.stderr),
            self.logs()
        );
    }

    /// Stops the daemon's unit, as an operator does before it changes the binary directory:
    /// `systemctl stop` returns once the daemon has exited.
    ///
    /// # Panics
    /// The unit does not stop.
    pub fn stop(&self) {
        let stopped = systemctl(&["stop", MAIN]);
        assert!(
            stopped.status.success(),
            "systemctl stop {MAIN} failed: {}\n{}",
            String::from_utf8_lossy(&stopped.stderr),
            self.logs()
        );
    }

    /// Starts the daemon's unit again, on whatever `current` of the binary directory names now:
    /// `systemctl reset-failed` first, so that the unit starts with no failure of an earlier run
    /// held against it, then `systemctl start`, which returns once the daemon is ready.
    ///
    /// # Panics
    /// The unit's count cannot be reset, or the unit does not start.
    pub fn start_again(&self) {
        let reset = systemctl(&["reset-failed", MAIN]);
        assert!(
            reset.status.success(),
            "systemctl reset-failed {MAIN} failed: {}\n{}",
            String::from_utf8_lossy(&reset.stderr),
            self.logs()
        );
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

    /// The journal of the daemon's unit and of hostkeeper's step units.
    pub fn logs(&self) -> String {
        [MAIN, STEP_UNITS]
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
        let _ = systemctl(&["stop", MAIN, STEP_UNITS]);
        // A package a stopped step left half configured is finished first, so that the purge
        // can run.
        let _ = run_tool("dpkg", &["--configure", "-a"]);
        let _ = run_tool("apt-mark", &["unhold", PACKAGE]);
        let _ = run_tool("dpkg", &["--purge", PACKAGE]);
        for place in taken_places() {
            take_away(&place);
        }
        for (place, aside) in &self.saved {
            let _ = fs::rename(aside, place);
        }
        let _ = systemctl(&["daemon-reload"]);
        let _ = systemctl(&["reset-failed", MAIN]);
    }
}
