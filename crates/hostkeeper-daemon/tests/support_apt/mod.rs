//! A daemon whose host has apt: the repository installer's fake of apt, dpkg and dpkg-deb
//! (`hostkeeper-install`'s, one copy for both crates' tests) answers every tool run of the
//! daemon's step launcher, through the launcher's own call ([`bridge`]), so the daemon's tools
//! port ([`hostkeeper_install::repository::launcher::LauncherTools`]) is the one a host runs.
//! The host's one repository entry, `komira-stable` (the channel of the coordinator's `komira`
//! with the suite `stable`), is a `file:` repository.

#![allow(
    dead_code,
    unused_imports,
    reason = "each part of the support is used by some of the test files"
)]

// The fake is loaded once, by `support::apt`, which the self-update rig of `support` shares.
pub use crate::support::apt::apt_fake;
pub mod repository_host;
pub mod services;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::testing::ToolAnswer;
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_install::repository::config::{Repositories, parse};
use hostkeeper_install::repository::{Network, Run, Tool, Tools};
use hostkeeper_install::{Announcement, Family};
use hostkeeper_proto::hostkeeper::agent::v1::{Order, order::Verb};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, StepBounds, Target,
};
use hostkeeper_release::Installer;
use hostkeeper_steps::{LaunchError, ToolCall, ToolExit, ToolNetwork};

use crate::support::{OWN, Rig, config};
pub use apt_fake::{Deb, Events, FakeHost, Health, Inst};

/// The repository the host's entry names.
pub const REPO: &str = "komira-stable";
/// Its URI: a directory of the host.
pub const URI: &str = "file:/srv/hostkeeper-apt";
/// The service and its package.
pub const SERVICE: &str = "web";
/// The unit the service runs.
pub const UNIT: &str = "web.service";
/// What the package installs as its program.
pub const PROGRAM: &str = "usr/bin/web";

/// The launcher's answer to each tool run: the run as the tools port gave it, answered by `host`.
pub fn bridge(host: Arc<FakeHost>) -> ToolAnswer {
    Arc::new(move |call: &ToolCall<'_>| {
        let tool = Tool::ALL
            .into_iter()
            .find(|tool| call.program == std::path::Path::new(tool.path()))
            .ok_or_else(|| LaunchError::NoStart(format!("no tool {}", call.program.display())))?;
        let args = call
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let run = Run {
            tool,
            args,
            network: match call.network {
                ToolNetwork::Offline => Network::Offline,
                ToolNetwork::Fetch => Network::Fetch,
            },
            writable: call
                .writable
                .map(|paths| paths.iter().map(|p| p.to_path_buf()).collect()),
            working_dir: call.working_dir.map(|dir| dir.to_path_buf()),
            env: call
                .env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            bound_secs: call.timeout_secs,
        };
        host.run(&run)
            .map(|output| ToolExit {
                code: output.code,
                stdout: output.stdout,
                stderr: output.stderr,
                cut: output.cut,
            })
            .map_err(|error| LaunchError::Launcher(error.0))
    })
}

/// The host's repository entries: `komira-stable`, at [`URI`], its key in `keys`.
pub fn repositories(keys: &std::path::Path) -> Repositories {
    fs::create_dir_all(keys).expect("the keys directory");
    let text = format!(
        "Types: deb\nURIs: {URI}\nSuites: stable\nComponents: main\nSigned-By: {}\n",
        keys.join("main.gpg").display()
    );
    Repositories::new(vec![parse(REPO, &text, keys).expect("a repository entry")])
}

/// A rig whose daemon announces apt, with the fake answering its tool runs.
pub struct AptRig {
    pub rig: Rig,
    pub host: Arc<FakeHost>,
    pub events: Events,
}

impl AptRig {
    pub fn new(name: &str) -> AptRig {
        let rig = Rig::new(name);
        // The packages directory, as the daemon's start makes it.
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        let events = Events::default();
        let lock = rig.scratch.path().join("installer.lock");
        let host = Arc::new(FakeHost::new(&lock, Arc::clone(&events)));
        rig.launcher.answer_tools(bridge(Arc::clone(&host)));
        host.watch_units(&rig.host("/units"), "/units");
        AptRig { rig, host, events }
    }

    /// The daemon's ports: apt announced, the entry `main` configured.
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

    pub fn daemon(&self) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.ports(), OWN)
    }

    /// The daemon of [`AptRig::daemon`], whose steps wait `secs` at most for the package
    /// manager's lock (`--package-lock-wait-secs`).
    pub fn daemon_waiting(&self, secs: u64) -> Daemon {
        let ports = self.ports();
        let repository = (*ports.repository).clone().with_lock_wait_secs(secs);
        let ports = Ports {
            repository: Arc::new(repository),
            ..ports
        };
        Daemon::new(config(), self.rig.dirs(), ports, OWN)
    }

    /// Publishes `deb` in the repository, answers its pin.
    pub fn publish(&self, deb: Deb) -> String {
        self.host.publish(URI, deb)
    }

    /// The version directory of `version` of the service.
    pub fn version_dir(&self, version: &str) -> PathBuf {
        self.rig.packages.join(SERVICE).join(version)
    }
}

/// The package of the service at `version`: its program, which prints `version`, and its unit,
/// which starts the program and is wanted by `multi-user.target`.
pub fn package(version: &str) -> Deb {
    Deb::new(SERVICE, version, "amd64")
        .file(PROGRAM, 0o755, &format!("#!/bin/sh\necho {version}\n"))
        .file(
            &format!("usr/lib/systemd/system/{UNIT}"),
            0o644,
            "[Unit]\nDescription=web\n[Service]\nExecStart=/usr/bin/web\n[Install]\nWantedBy=multi-user.target\n",
        )
        .link("usr/lib/web/current", "../../bin/web")
}

/// Where the hooked package installs its hook program.
pub const HOOK_PROGRAM: &str = "usr/lib/web/web";

/// The package of the service at `version` whose hook program is [`HOOK_PROGRAM`], and which
/// installs `/usr/bin/web` as a link to it (so the manifest may name either path).
pub fn hooked_package(version: &str) -> Deb {
    package(version)
        .without(PROGRAM)
        .file(HOOK_PROGRAM, 0o755, "#!/bin/sh\nexit 0\n")
        .link(PROGRAM, "/usr/lib/web/web")
}

/// The package of the service at `version` with no `[Install]` section in its unit.
pub fn unlinked_package(version: &str) -> Deb {
    package(version).file(
        &format!("usr/lib/systemd/system/{UNIT}"),
        0o644,
        "[Unit]\nDescription=web\n[Service]\nExecStart=/usr/bin/web\n",
    )
}

/// The manifest of an entry of the service: its unit, the built-in steps (no hook command), the
/// installer's bounds.
pub fn manifest() -> Manifest {
    Manifest {
        min_hostkeeper: "0.1.0".to_owned(),
        package: Some(SERVICE.to_owned()),
        run: vec![UNIT.to_owned()],
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

/// The apt entry of `package_version`, pinned to `pin`, from the repository `main`.
pub fn entry(package_version: &str, pin: &str) -> Target {
    Target {
        installer: Installer::Apt.as_str().to_owned(),
        digest: pin.to_owned(),
        manifest: Some(manifest()),
        repository: Some(REPO.to_owned()),
        package_version: Some(package_version.to_owned()),
        ..Target::default()
    }
}

/// [`entry`] whose manifest names `command` as its hook.
pub fn hooked_entry(package_version: &str, pin: &str, command: &[&str]) -> Target {
    let mut target = entry(package_version, pin);
    if let Some(hook) = target.manifest.as_mut().and_then(|m| m.hook.as_mut()) {
        hook.command = command.iter().map(|word| (*word).to_owned()).collect();
    }
    target
}

/// `order` with `entry` as its target.
pub fn with_entry(mut order: Order, entry: &Target) -> Order {
    match order.verb.as_mut() {
        Some(Verb::Install(a)) => a.target = Some(entry.clone()),
        Some(Verb::Update(a)) => a.target = Some(entry.clone()),
        Some(Verb::ForceRollback(a)) => a.target = Some(entry.clone()),
        Some(Verb::Force(a)) => a.target = Some(entry.clone()),
        other => panic!("no target to set on {other:?}"),
    }
    order
}

/// Makes every directory under `dir` writable by its owner: an uninstall deletes the sealed,
/// read-only version directories, which root does through their mode on a host and the test's
/// own user cannot.
pub fn open_up(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    for entry in fs::read_dir(dir).expect("a directory").flatten() {
        open_up(&entry.path());
    }
}

/// What the apt tests read of the daemon and of the host after an order.
pub mod seen {
    use std::fs;
    use std::sync::Arc;

    use hostkeeper_config::names::ServiceName;
    use hostkeeper_daemon::{Daemon, Reply};
    use hostkeeper_hook::{Class, Phase, Verdict};
    use hostkeeper_placed::testing::{Event, UnitCall};
    use hostkeeper_proto::hostkeeper::agent::v1::Order;

    use super::{AptRig, Inst, SERVICE, entry, hooked_entry, with_entry};
    use crate::support::{install, update};

    /// A reply that ran to `Succeeded` now, with no reason.
    pub fn succeeded() -> Reply {
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false,
            },
            reason: None,
            resent: false,
        }
    }

    /// A reply that ran to `Failed` before the commit point, blamed on `class`, with `reason`.
    pub fn failed(class: Class, reason: &str) -> Reply {
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::BeforeCommit,
                class,
            },
            reason: Some(reason.to_owned()),
            resent: false,
        }
    }

    /// A reply that ran to `Deferred` for the stage's 60 seconds, with `reason`.
    pub fn deferred(reason: &str) -> Reply {
        Reply::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(60),
            },
            reason: Some(reason.to_owned()),
            resent: false,
        }
    }

    /// The package as installed and held at `version`.
    pub fn held(version: &str) -> Option<Inst> {
        Some(Inst {
            version: version.to_owned(),
            state: 'i',
            held: true,
        })
    }

    pub fn web() -> ServiceName {
        ServiceName::new(SERVICE).expect("a name")
    }

    /// The version `link` (`current` or `previous`) of the service names.
    pub fn linked(apt: &AptRig, link: &str) -> Option<String> {
        fs::read_link(apt.rig.packages.join(SERVICE).join(link))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// The `installed` entry of the service in `placed.json`.
    pub fn recorded(daemon: &Daemon) -> Option<hostkeeper_placed::InstalledEntry> {
        let record = daemon
            .ctx()
            .placer()
            .store()
            .load()
            .expect("placed.json reads");
        record.installed.get(SERVICE).cloned()
    }

    /// The units the daemon asked the service manager to stop and start, in order.
    pub fn unit_calls(apt: &AptRig) -> Vec<UnitCall> {
        apt.rig
            .placed_log
            .events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Unit(call @ (UnitCall::Stop(_) | UnitCall::Start(_))) => Some(call),
                _ => None,
            })
            .collect()
    }

    /// The order to install `version` from `package_version`, pinned to `pin`.
    pub fn install_of(version: &str, package_version: &str, pin: &str, order_try: u64) -> Order {
        with_entry(
            install(SERVICE, version, pin, order_try),
            &entry(package_version, pin),
        )
    }

    /// The order to update to `version` from `package_version`, pinned to `pin`.
    pub fn update_of(version: &str, package_version: &str, pin: &str, order_try: u64) -> Order {
        with_entry(
            update(SERVICE, version, pin, order_try),
            &entry(package_version, pin),
        )
    }

    /// [`install_of`] whose manifest names `command` as its hook.
    pub fn install_hooked(
        version: &str,
        package_version: &str,
        pin: &str,
        order_try: u64,
        command: &[&str],
    ) -> Order {
        with_entry(
            install(SERVICE, version, pin, order_try),
            &hooked_entry(package_version, pin, command),
        )
    }

    /// [`update_of`] whose manifest names `command` as its hook.
    pub fn update_hooked(
        version: &str,
        package_version: &str,
        pin: &str,
        order_try: u64,
        command: &[&str],
    ) -> Order {
        with_entry(
            update(SERVICE, version, pin, order_try),
            &hooked_entry(package_version, pin, command),
        )
    }

    /// The labels of the fake's tool runs from call number `from` on.
    pub fn labels_since(apt: &AptRig, from: usize) -> Vec<String> {
        apt.host.calls()[from..]
            .iter()
            .map(|call| call.label.clone())
            .collect()
    }

    /// The same daemon behind an `Arc`, for the observer.
    pub fn shared(daemon: Daemon) -> Arc<Daemon> {
        Arc::new(daemon)
    }
}
