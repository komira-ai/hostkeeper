//! What the tests of hostkeeper's own update share: a rig whose host runs hostkeeper at [`OLD`]
//! with the release of [`NEW`] in the package store, a probe that answers as told, and the
//! replies and records of a self-update.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_daemon::selfupdate::{
    ProbeError, ProtocolProbe, REASON_AWAITING_RESTART, RELEASE_BINARY, binary_dir,
};
use hostkeeper_daemon::{Daemon, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_steps::record::{HookSel, Progress, SCHEMA, State};
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

/// `text`, a `major.minor.patch`, as a daemon's own version.
pub fn own(text: &str) -> HostkeeperVersion {
    HostkeeperVersion::parse(text).expect("a hostkeeper version")
}

type Answer = Box<dyn Fn() -> Result<u32, ProbeError> + Send + Sync>;

/// A probe that answers as the test says, and keeps the binaries it was asked about.
pub struct FakeProbe {
    answer: Mutex<Answer>,
    asked: Mutex<Vec<PathBuf>>,
}

impl FakeProbe {
    /// A probe that answers `protocol`.
    pub fn answering(protocol: u32) -> Arc<FakeProbe> {
        Arc::new(FakeProbe {
            answer: Mutex::new(Box::new(move || Ok(protocol))),
            asked: Mutex::default(),
        })
    }

    /// From now on, the probe answers with `answer`.
    pub fn set(&self, answer: impl Fn() -> Result<u32, ProbeError> + Send + Sync + 'static) {
        *self.answer.lock().expect("the answer") = Box::new(answer);
    }

    /// Every binary the probe was asked about, in order.
    pub fn asked(&self) -> Vec<PathBuf> {
        self.asked.lock().expect("the asked").clone()
    }
}

impl ProtocolProbe for FakeProbe {
    fn protocol(&self, binary: &Path) -> Result<u32, ProbeError> {
        self.asked
            .lock()
            .expect("the asked")
            .push(binary.to_path_buf());
        (self.answer.lock().expect("the answer"))()
    }
}

/// A rig whose host runs hostkeeper at [`OLD`]: its package installed (`current` names it), its
/// binary directory with [`OLD`] staged and `current`, and the release of [`NEW`], with its
/// binary, in the package store ([`SelfRig::new`]); or the same with no package of hostkeeper
/// installed ([`SelfRig::bootstrapped`]).
pub struct SelfRig {
    pub rig: Rig,
    pub probe: Arc<FakeProbe>,
}

impl SelfRig {
    pub fn new(name: &str) -> SelfRig {
        let s = SelfRig::bootstrapped(name);
        s.rig.running(HK, OLD);
        s
    }

    /// A rig whose host runs hostkeeper at [`OLD`] as the deb or the pkg installs it: its binary
    /// directory with [`OLD`] staged and `current`, as `self-update-seed` leaves it at the first
    /// start, and no package of hostkeeper installed (nothing under the packages directory names
    /// it `current`). The release of [`NEW`], with its binary, is in the package store.
    pub fn bootstrapped(name: &str) -> SelfRig {
        let rig = Rig::new(name);
        let layout = Layout::new(&binary_dir(&rig.state));
        let old = rig.scratch.path().join("old-binary");
        fs::write(&old, format!("#!hostkeeper {OLD}\n")).expect("the old binary");
        layout.stage(OLD, &old).expect("stage the old binary");
        layout.switch(OLD).expect("switch to it");
        let s = SelfRig {
            rig,
            probe: FakeProbe::answering(1),
        };
        s.release(NEW);
        s
    }

    /// The release of hostkeeper at `version`, with its binary, in the package store.
    pub fn release(&self, version: &str) -> PathBuf {
        let dir = self.rig.stage(HK, version);
        let binary = dir.join(RELEASE_BINARY);
        fs::write(&binary, format!("#!hostkeeper {version}\n")).expect("the binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("its mode");
        dir
    }

    /// A daemon of version `version` on the rig, its probe the rig's.
    pub fn daemon_at(&self, version: &str) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.rig.ports(), own(version))
            .with_probe(self.probe.clone())
    }

    /// The daemon of [`OLD`].
    pub fn daemon(&self) -> Daemon {
        self.daemon_at(OLD)
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
