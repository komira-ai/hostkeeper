//! What the tests of hostkeeper's own update share: a rig whose host runs hostkeeper at [`OLD`]
//! (its version of the packages directory, staged for `apt` as the repository installer leaves
//! it) with the `.deb` of [`NEW`] published in the host's repository, the fake of apt and dpkg
//! answering the daemon's tool runs ([`super::apt`]), and the replies and records of a
//! self-update. An order carries the entry of the release it names, pinned to the digest the
//! repository answered for its package.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_daemon::observer::{OwnService, observer_with};
use hostkeeper_daemon::selfupdate::{APT_RELEASE_BINARY, REASON_AWAITING_RESTART, binary_dir};
use hostkeeper_daemon::{Daemon, Ports, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_install::repository::tree::hook_path;
use hostkeeper_install::{Announcement, Family};
use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::{ActiveState, ExeStat};
use hostkeeper_placed::InstalledEntry;
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, StepBounds, Target,
};
use hostkeeper_release::Installer;
use hostkeeper_steps::record::{HookSel, Loaded, Progress, SCHEMA, State};
use hostkeeper_steps::{Layout, Record, RecordStore};

use super::apt::{Deb, FakeHost, REPO, URI, bridge, repositories, with_entry};
use super::versions::entry_of;
use super::{Rig, config, d, force, force_rollback, install, update};

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
/// The unit hostkeeper's package runs.
pub const DEB_UNIT_NAME: &str = "hostkeeper.service";
/// Where the unit is in hostkeeper's package.
const DEB_UNIT_PATH: &str = "usr/lib/systemd/system/hostkeeper.service";

/// `text`, a `major.minor.patch`, as a daemon's own version.
pub fn own(text: &str) -> HostkeeperVersion {
    HostkeeperVersion::parse(text).expect("a hostkeeper version")
}

/// The package version of the release `version` in the repository.
pub fn pv(version: &str) -> String {
    format!("{version}-1")
}

/// The text of hostkeeper's binary of `version` in the tests.
pub fn binary_text(version: &str) -> String {
    format!("#!hostkeeper {version}\n")
}

/// hostkeeper's `.deb` of `version`: its binary and the unit `unit`, with the maintainer scripts
/// its own package keeps.
pub fn hk_deb(version: &str, unit: &str) -> Deb {
    Deb::new(HK, &pv(version), "amd64")
        .file("usr/bin/hostkeeper", 0o755, &binary_text(version))
        .file(DEB_UNIT_PATH, 0o644, unit)
        .member("postinst")
        .member("prerm")
}

/// The unit a test `.deb` of hostkeeper carries: it starts the daemon and is wanted by the
/// multi-user target.
const TEST_UNIT: &str = "[Unit]\nDescription=hostkeeper\n[Service]\nExecStart=/usr/bin/hostkeeper daemon\n[Install]\nWantedBy=multi-user.target\n";

/// The manifest of hostkeeper's entry: its unit, no hook program (its steps are built in), the
/// floor `floor`, the installer's bounds.
pub fn hk_manifest(floor: &str) -> Manifest {
    Manifest {
        min_hostkeeper: floor.to_owned(),
        package: Some(HK.to_owned()),
        run: vec![DEB_UNIT_NAME.to_owned()],
        hook: Some(Hook {
            timeout_secs: Some(StepBounds {
                prepare: 60,
                cutover: 60,
                verify: 60,
                rollback: 60,
                cleanup: 60,
                ..StepBounds::default()
            }),
            verify_deadline_secs: 300,
            ..Hook::default()
        }),
        installer_bounds: Some(InstallerBounds {
            stage_secs: 60,
            activate_secs: 60,
            remove_secs: 60,
        }),
        ..Manifest::default()
    }
}

/// The apt entry of `manifest`, at the package version of `version`, pinned to `pin`, from the
/// repository `main`.
fn apt_entry(manifest: Manifest, version: &str, pin: &str) -> Target {
    Target {
        installer: Installer::Apt.as_str().to_owned(),
        digest: pin.to_owned(),
        manifest: Some(manifest),
        repository: Some(REPO.to_owned()),
        package_version: Some(pv(version)),
        ..Target::default()
    }
}

/// The manifest of a service the rig publishes: the daemon's test double ([`manifest`]) with its
/// one run unit, which the repository installer's entries require.
///
/// [`manifest`]: hostkeeper_daemon::testing::manifest
fn service_manifest(service: &str, version: &str) -> String {
    format!(
        "{}\n[units]\nrun = [\"{service}.service\"]\n",
        hostkeeper_daemon::testing::manifest(service, version)
    )
}

/// The unit file of a service the rig publishes: it starts the program under the service's
/// package and is wanted by the multi-user target.
fn service_unit(service: &str) -> String {
    format!(
        "[Unit]\nDescription={service}\n[Service]\nExecStart=/usr/bin/{service}\n[Install]\nWantedBy=multi-user.target\n"
    )
}

/// A rig whose host runs hostkeeper at [`OLD`]: its package installed (`current` names it), its
/// binary directory with [`OLD`] staged and `current`, and the release of [`NEW`], with its
/// binary, published in the repository ([`SelfRig::new`]); or the same with no package of
/// hostkeeper installed ([`SelfRig::bootstrapped`]).
pub struct SelfRig {
    pub rig: Rig,
    /// The fake of apt, dpkg and dpkg-deb the daemon's tool runs go to.
    pub host: Arc<FakeHost>,
    /// The entry of each release published, by service and version.
    entries: Mutex<BTreeMap<(String, String), Target>>,
}

impl SelfRig {
    pub fn new(name: &str) -> SelfRig {
        let s = SelfRig::bootstrapped(name);
        s.installed_hk(OLD);
        s
    }

    /// The version `version` of hostkeeper in the packages directory as the repository installer
    /// leaves one, `current` naming it.
    fn installed_hk(&self, version: &str) {
        let entry = apt_entry(hk_manifest("0.1.0"), version, &d());
        let dir = self.rig.store.add_entry(HK, version, &entry, false);
        let binary = dir.join(APT_RELEASE_BINARY);
        fs::create_dir_all(binary.parent().expect("the bin directory")).expect("the bin directory");
        fs::write(&binary, binary_text(version)).expect("the binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("its mode");
        self.rig.store.seal(HK, version, &d());
        self.rig.store.link(HK, "current", version);
    }

    /// [`SelfRig::new`] on a host whose `placed.json` also records hostkeeper's package, as the
    /// installer wrote it when it installed [`OLD`] (and the package manager holds it at that
    /// version): the host reports `apt` as the installer hostkeeper runs with, so that the
    /// coordinator orders it an update.
    pub fn recorded(name: &str) -> SelfRig {
        let s = SelfRig::new(name);
        s.host.set_installed(HK, &pv(OLD), 'i', true);
        let mut record = s.rig.placed_record();
        let mut entry = InstalledEntry::new(Installer::Apt.as_str());
        entry.package = Some(HK.to_owned());
        entry.version = Some(pv(OLD));
        entry.repository = Some(REPO.to_owned());
        record.installed.insert(HK.to_owned(), entry);
        s.rig.save_placed(&record);
        s
    }

    /// A rig whose host runs hostkeeper at [`OLD`] as the deb or the pkg installs it: its binary
    /// directory with [`OLD`] staged and `current`, as the packaged start's seed leaves it at the
    /// first start, and no package of hostkeeper installed (nothing under the packages directory names
    /// it `current`). The release of [`NEW`], with its binary, is published in the repository.
    pub fn bootstrapped(name: &str) -> SelfRig {
        let rig = Rig::new(name);
        // The packages directory, as the daemon's start makes it.
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        let events = super::apt::Events::default();
        let lock = rig.scratch.path().join("installer.lock");
        let host = Arc::new(FakeHost::new(&lock, events));
        rig.launcher.answer_tools(bridge(Arc::clone(&host)));
        let layout = Layout::new(&binary_dir(&rig.state));
        let old = rig.scratch.path().join("old-binary");
        fs::write(&old, binary_text(OLD)).expect("the old binary");
        layout.stage(OLD, &old).expect("stage the old binary");
        layout.switch(OLD).expect("switch to it");
        let s = SelfRig {
            rig,
            host,
            entries: Mutex::default(),
        };
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

    /// The ports of the daemon on the rig: apt announced, the repository entry `main` configured.
    pub fn ports(&self) -> Ports {
        let keys = self.rig.scratch.path().join("keys");
        Ports {
            announcement: Announcement {
                installers: vec![Installer::Apt],
                family: Some(Family::Deb),
            },
            repository: self.rig.repository_ports(repositories(&keys)),
            ..self.rig.ports()
        }
    }

    /// Publishes `deb` in the repository in place of the package of its name and version that was
    /// published before, if any (a repository lists a version once); answers its pin.
    fn publish_replacing(&self, deb: Deb) -> String {
        let same = |package: &str, version: &str| package == deb.package && version == deb.version;
        self.host.edit(|state| {
            state.debs.retain(|old| !same(&old.package, &old.version));
            for repo in state.repos.values_mut() {
                repo.entries.retain(|old| !same(&old.package, &old.version));
            }
        });
        self.host.publish(URI, deb)
    }

    /// Publishes `deb` of `service` at `version` in the repository, with the manifest `manifest`:
    /// the entry the orders of that release carry.
    fn publish_as(&self, service: &str, version: &str, deb: Deb, manifest: Manifest) -> Target {
        let pin = self.publish_replacing(deb);
        let entry = apt_entry(manifest, version, &pin);
        self.entries
            .lock()
            .expect("the entries")
            .insert((service.to_owned(), version.to_owned()), entry.clone());
        entry
    }

    /// The release of hostkeeper at `version`, with its binary, published in the repository.
    pub fn release(&self, version: &str) -> Target {
        self.release_with_floor(version, "0.1.0")
    }

    /// The release of hostkeeper at `version` whose package declares the floor `floor`, with its
    /// binary, published in the repository.
    pub fn release_with_floor(&self, version: &str, floor: &str) -> Target {
        self.publish_as(HK, version, hk_deb(version, TEST_UNIT), hk_manifest(floor))
    }

    /// The release of hostkeeper at `version` published with no hostkeeper binary.
    pub fn release_without_binary(&self, version: &str) -> Target {
        self.publish_as(
            HK,
            version,
            hk_deb(version, TEST_UNIT).without("usr/bin/hostkeeper"),
            hk_manifest("0.1.0"),
        )
    }

    /// The release of hostkeeper at `version` as CI packages it: its binary and the unit
    /// hostkeeper's package carries, `usr/lib/systemd/system/hostkeeper.service`, which its
    /// manifest runs.
    pub fn shipped_release(&self, version: &str) -> Target {
        self.publish_as(
            HK,
            version,
            hk_deb(version, SHIPPED_UNIT),
            hk_manifest("0.1.0"),
        )
    }

    /// The package of `service` at `version`, published in the repository, with the manifest the
    /// daemon's test doubles write ([`hostkeeper_daemon::testing::manifest`]): a hook program in
    /// its tree, which the rig's launcher answers for.
    pub fn publish_service(&self, service: &str, version: &str) -> Target {
        let (entry, hook) = entry_of(service, version, &service_manifest(service, version));
        let mut deb = Deb::new(service, &pv(version), "amd64").file(
            &format!("usr/lib/systemd/system/{service}.service"),
            0o644,
            &service_unit(service),
        );
        if hook {
            deb = deb.file(&hook_path(service), 0o755, "#!hook\n");
        }
        let pin = self.publish_replacing(deb);
        let entry = Target {
            digest: pin,
            ..entry
        };
        self.entries
            .lock()
            .expect("the entries")
            .insert((service.to_owned(), version.to_owned()), entry.clone());
        entry
    }

    /// `service` runs at [`super::BASE`] with a package the installer staged, `current` naming it.
    pub fn installed_service(&self, service: &str) {
        let (entry, hook) = entry_of(
            service,
            super::BASE,
            &service_manifest(service, super::BASE),
        );
        self.rig.store.add_entry(service, super::BASE, &entry, hook);
        self.rig.store.seal(service, super::BASE, &d());
        self.rig.store.link(service, "current", super::BASE);
    }

    /// The entry of the release of `service` at `version` published on the rig.
    pub fn entry_of(&self, service: &str, version: &str) -> Target {
        self.entries
            .lock()
            .expect("the entries")
            .get(&(service.to_owned(), version.to_owned()))
            .cloned()
            .unwrap_or_else(|| panic!("no release of {service} {version} was published"))
    }

    /// The pin of the release of hostkeeper at `version` the repository answered.
    pub fn pin(&self, version: &str) -> String {
        self.entry_of(HK, version).digest
    }

    /// An `update` of `service` to `version`, try `order_try`, carrying the entry of its release.
    pub fn update_of(&self, service: &str, version: &str, order_try: u64) -> Order {
        let entry = self.entry_of(service, version);
        with_entry(update(service, version, &entry.digest, order_try), &entry)
    }

    /// An `install` of hostkeeper at `version`, try `order_try`.
    pub fn install_order(&self, version: &str, order_try: u64) -> Order {
        let entry = self.entry_of(HK, version);
        with_entry(install(HK, version, &entry.digest, order_try), &entry)
    }

    /// A `force_rollback` of hostkeeper to `version`, try `order_try`.
    pub fn rollback_order(&self, version: &str, order_try: u64) -> Order {
        let entry = self.entry_of(HK, version);
        with_entry(
            force_rollback(HK, version, &entry.digest, order_try),
            &entry,
        )
    }

    /// A `force` of hostkeeper at `version`, try `order_try`, halt generation `generation`.
    pub fn force_order(&self, version: &str, order_try: u64, generation: u64) -> Order {
        let entry = self.entry_of(HK, version);
        with_entry(
            force(HK, version, &entry.digest, order_try, generation),
            &entry,
        )
    }

    /// The tool runs of the fake of apt and dpkg so far: what staging and activating a version
    /// cost, none for an order that stopped before them.
    pub fn tool_runs(&self) -> usize {
        self.host.calls().len()
    }

    /// A daemon of version `version` on the rig.
    pub fn daemon_at(&self, version: &str) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.ports(), own(version))
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
            ..self.ports()
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
        self.update_of(HK, version, order_try)
    }

    /// Every step of hostkeeper the launcher ran, in order. The steps of hostkeeper's own package
    /// are built in (it has no hook program), so the launcher runs none of them.
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
