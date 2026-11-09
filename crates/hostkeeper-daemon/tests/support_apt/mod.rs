//! A daemon whose host has apt: the repository installer's fake of apt, dpkg and dpkg-deb
//! (`hostkeeper-install`'s, one copy for both crates' tests) answers every tool run of the
//! daemon's step launcher, through the launcher's own call ([`bridge`]), so the daemon's tools
//! port ([`hostkeeper_install::repository::launcher::LauncherTools`]) is the one a host runs.
//! The host's one repository entry, `main`, is a `file:` repository.

#![allow(
    dead_code,
    reason = "each part of the support is used by some of the test files"
)]

#[path = "../../../hostkeeper-install/tests/support/apt_fake.rs"]
pub mod apt_fake;

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
pub const REPO: &str = "main";
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

/// The host's repository entries: `main`, at [`URI`], its key in `keys`.
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
        AptRig { rig, host, events }
    }

    /// The daemon's ports: apt announced, the entry `main` configured.
    pub fn ports(&self) -> Ports {
        let keys = self.rig.scratch.path().join("keys");
        Ports {
            announcement: Announcement {
                installers: vec![Installer::HostkeeperArchive, Installer::Apt],
                family: Some(Family::Deb),
            },
            repository: self.rig.repository_ports(repositories(&keys)),
            ..self.rig.ports()
        }
    }

    pub fn daemon(&self) -> Daemon {
        Daemon::new(config(), self.rig.dirs(), self.ports(), OWN)
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
