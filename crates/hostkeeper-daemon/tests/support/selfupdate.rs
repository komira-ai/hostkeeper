//! What the tests of hostkeeper's own update share: a rig whose host runs hostkeeper at [`OLD`]
//! with the release of [`NEW`] in the package store, and the replies and records of a
//! self-update.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_daemon::observer::{OwnService, observer_with};
use hostkeeper_daemon::selfupdate::{APT_RELEASE_BINARY, REASON_AWAITING_RESTART, binary_dir};
use hostkeeper_daemon::{Daemon, Ports, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::{ActiveState, ExeStat};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_steps::record::{HookSel, Loaded, Progress, SCHEMA, State};
use hostkeeper_steps::{Layout, Record, RecordStore};

use super::{Rig, config, d, update};

/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// The version the host runs: the daemon under test ([`super::OWN`]).
pub const OLD: &str = "0.1.0";
/// The version it is ordered to.
pub const NEW: &str = "0.2.0";
/// A later version.
pub const LATER: &str = "0.3.0";

/// The unit file of the daemon, as the deb installs it and hostkeeper's package carries it.
pub const SHIPPED_UNIT: &str = include_str!("../../../../packaging/linux/units/hostkeeper.service");
/// Where the deb installs it.
pub const DEB_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";
/// The `WantedBy=` link of the deb's `systemctl enable`, in the rig's unit directory.
pub const DEB_WANTS: &str = "/units/multi-user.target.wants/hostkeeper.service";

/// `text`, a `major.minor.patch`, as a daemon's own version.
pub fn own(text: &str) -> HostkeeperVersion {
    HostkeeperVersion::parse(text).expect("a hostkeeper version")
}

/// A rig whose host runs hostkeeper at [`OLD`]: its package installed (`current` names it), its
/// binary directory with [`OLD`] staged and `current`, and the release of [`NEW`], with its
/// binary, in the package store ([`SelfRig::new`]); or the same with no package of hostkeeper
/// installed ([`SelfRig::bootstrapped`]).
pub struct SelfRig {
    pub rig: Rig,
}

impl SelfRig {
    pub fn new(name: &str) -> SelfRig {
        let s = SelfRig::bootstrapped(name);
        s.rig.running(HK, OLD);
        s
    }

    /// A rig whose host runs hostkeeper at [`OLD`] as the deb or the pkg installs it: its binary
    /// directory with [`OLD`] staged and `current`, as the packaged start's seed leaves it at the
    /// first start, and no package of hostkeeper installed (nothing under the packages directory names
    /// it `current`). The release of [`NEW`], with its binary, is in the package store.
    pub fn bootstrapped(name: &str) -> SelfRig {
        let rig = Rig::new(name);
        let layout = Layout::new(&binary_dir(&rig.state));
        let old = rig.scratch.path().join("old-binary");
        fs::write(&old, format!("#!hostkeeper {OLD}\n")).expect("the old binary");
        layout.stage(OLD, &old).expect("stage the old binary");
        layout.switch(OLD).expect("switch to it");
        let s = SelfRig { rig };
        s.release(NEW);
        s
    }

    /// A rig whose host runs hostkeeper at [`OLD`] as the deb leaves it ([`SelfRig::bootstrapped`]),
    /// its unit enabled the way the deb's `systemctl enable` enables it: the `WantedBy=` link
    /// in the unit directory points at the unit the deb installed under `/usr/lib`, and
    /// `placed.json` lists nothing. The release of [`NEW`] is hostkeeper's as CI packages it
    /// ([`SelfRig::shipped_release`]).
    pub fn deb(name: &str) -> SelfRig {
        let s = SelfRig::bootstrapped(name);
        s.shipped_release(NEW);
        let wants = s.rig.host(DEB_WANTS);
        fs::create_dir_all(wants.parent().expect("the wants directory")).expect("mkdir");
        std::os::unix::fs::symlink(DEB_UNIT, wants).expect("the deb's enable link");
        s
    }

    /// The release of hostkeeper at `version` as CI packages it: its binary, its hook
    /// `bin/hostkeeper update-hook` (which no daemon runs), and the unit hostkeeper's package
    /// carries, `units/hostkeeper.service`, which its manifest runs.
    pub fn shipped_release(&self, version: &str) -> PathBuf {
        let manifest = super::manifest(HK, version).replace(
            r#"command = ["bin/hook", "update-hook"]"#,
            r#"command = ["bin/hostkeeper", "update-hook"]"#,
        ) + "\n[units]\nrun = [\"hostkeeper.service\"]\n";
        let dir = self.rig.store.add(HK, version, &manifest);
        fs::create_dir_all(dir.join("units")).expect("the units directory");
        fs::write(dir.join("units/hostkeeper.service"), SHIPPED_UNIT).expect("the unit");
        let binary = dir.join(APT_RELEASE_BINARY);
        fs::create_dir_all(binary.parent().expect("the bin directory")).expect("the bin directory");
        fs::write(&binary, format!("#!hostkeeper {version}\n")).expect("the binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("its mode");
        dir
    }

    /// The release of hostkeeper at `version`, with its binary, in the package store.
    pub fn release(&self, version: &str) -> PathBuf {
        let dir = self.rig.stage(HK, version);
        let binary = dir.join(APT_RELEASE_BINARY);
        fs::create_dir_all(binary.parent().expect("the bin directory")).expect("the bin directory");
        fs::write(&binary, format!("#!hostkeeper {version}\n")).expect("the binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("its mode");
        dir
    }

    /// A daemon of version `version` on the rig.
    pub fn daemon_at(&self, version: &str) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.rig.ports(), own(version))
    }

    /// A daemon of version `version` on the rig, observed as the host
    /// observes it: the daemon's own observer ([`observer_with`]) over the rig's service manager
    /// and processes, the daemon running as `unit` from the rig's binary directory.
    pub fn daemon_running_as(&self, version: &str, unit: &str) -> Daemon {
        let own_service = OwnService {
            unit: unit.to_owned(),
            binary_dir: binary_dir(&self.rig.state),
        };
        let observer = observer_with(
            Box::new(self.rig.manager.clone()),
            Box::new(self.rig.process.clone()),
            &self.rig.packages,
            own(version),
            &own_service,
        );
        let ports = Ports {
            observer: Arc::new(observer),
            ..self.rig.ports()
        };
        Daemon::new(config(), self.rig.dirs(), ports, own(version))
    }

    /// The service manager runs `unit` as the process `pid`, whose executable is the binary
    /// directory's file of `version`.
    pub fn runs(&self, unit: &str, version: &str, pid: u32) {
        let file = fs::File::open(self.layout().version_path(version)).expect("the binary");
        let digest = digest_open_file(&file).expect("its digest");
        let stat = ExeStat {
            size: 1,
            mtime_secs: i64::from(pid),
            mtime_nanos: 0,
        };
        self.rig.process.set(pid, u64::from(pid), stat, digest);
        self.rig.manager.set(unit, ActiveState::Active, Some(pid));
    }

    /// The daemon of [`OLD`].
    pub fn daemon(&self) -> Daemon {
        self.daemon_at(OLD)
    }

    /// Turns the hand-over an order of hostkeeper to `version` left (`next` linked, the wait naming
    /// it with a start count) into what a release before the hand-over wrote: `current` switched to `version`, `previous` the old
    /// binary, no `next`, and a wait with no `next` and no count.
    pub fn switch_the_old_way(&self, version: &str) {
        let layout = self.layout();
        layout
            .switch(version)
            .expect("the old updater switched first");
        layout.remove_next().expect("it linked no next");
        let store = RecordStore::new(&self.hk_state());
        let Loaded::Current(mut rec) = store.load().expect("the record") else {
            panic!("no record");
        };
        let wait = rec.progress.awaiting_restart.as_mut().expect("a wait");
        (wait.next, wait.starts) = (None, None);
        store
            .save(&rec)
            .expect("the record as the old updater wrote it");
    }

    /// The binary directory.
    pub fn layout(&self) -> Layout {
        Layout::new(&binary_dir(&self.rig.state))
    }

    /// The version `current` of the binary directory names.
    pub fn current(&self) -> Option<String> {
        self.layout().current().expect("current")
    }

    /// The state directory of the service `hostkeeper`.
    pub fn hk_state(&self) -> PathBuf {
        self.rig.state.join(HK)
    }

    /// An `update` of hostkeeper to `version`, try `order_try`.
    pub fn order(&self, version: &str, order_try: u64) -> Order {
        update(HK, version, &d(), order_try)
    }

    /// Every step of hostkeeper the launcher ran, in order.
    pub fn hk_steps(&self) -> Vec<Step> {
        self.rig.launcher.steps(HK)
    }
}

/// The reply of an update whose new binary has not run yet.
pub fn awaiting(resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_AWAITING_RESTART.to_owned()),
        resent,
    }
}

/// The reply of an update that ran to `Failed` before the commit, blamed on `class`, with
/// `reason`, now.
pub fn refused_before_commit(class: Class, reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::BeforeCommit,
            class,
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

/// The verdict of a version whose new binary fell back.
pub fn fell_back() -> (Verdict, Option<String>) {
    (
        Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release,
        },
        Some("self_update_fallback".to_owned()),
    )
}

/// A step record of `service` in `state`, of the update to `2.0`, try 1.
pub fn record_of(service: &str, state: State) -> Record {
    Record {
        schema: SCHEMA,
        service: service.to_owned(),
        target_version: "2.0".to_owned(),
        artifact_digest: "ab".repeat(32),
        tarball_digest: d(),
        previous_digest: Some("ef".repeat(32)),
        generation: 1,
        params: format!("/state/{service}/params.toml").into(),
        step: Step::Verify,
        attempt: 1,
        state,
        hook: HookSel::New,
        progress: Progress::default(),
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

/// Writes `rec` as the step record of `service` under the rig's state directory.
pub fn put_record(rig: &Rig, service: &str, rec: &Record) {
    let dir = rig.state.join(service);
    fs::create_dir_all(&dir).expect("the service's state dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("its mode");
    RecordStore::new(&dir).save(rec).expect("the record");
}
