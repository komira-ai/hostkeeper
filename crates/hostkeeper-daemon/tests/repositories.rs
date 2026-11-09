//! The repository configuration at the daemon's start and the entry's kind on receipt
//! (packages-repositories.md sections 2.3, 3.1 and 4.1; packages-tests.md, "The repository
//! configuration" and "Which installers a daemon announces"). The directory of repository
//! entries is owned by root on a host, and these tests run as the user that runs them: the
//! defects below are ones that root's own check refuses whoever the user is.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE};
use hostkeeper_daemon::cli::{EXIT_CONFIG, HostFiles, host_ports};
use hostkeeper_daemon::repositories;
use hostkeeper_daemon::start::StartError;
use hostkeeper_daemon::{Daemon, Ports, Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_install::repository::announce::{APT_TOOLS, DPKG_STATUS, native};
use hostkeeper_install::repository::config::{self, REPOSITORIES_DIR, Repositories};
use hostkeeper_install::{Announcement, Family, announce};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_proto::hostkeeper::agent::v1::{Order, order::Verb};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Location, Manifest, StepBounds, Target,
};
use hostkeeper_release::Installer;
use support::{OWN, Rig, config as daemon_config, d, install};

const SERVICE: &str = "mdm";
const VERSION: &str = "1.0.0";

/// The host files of a Linux host under `root`, with `repositories` as its entries' directory.
fn files(root: &Path, repositories: Option<PathBuf>) -> HostFiles {
    HostFiles {
        repositories,
        os_root: root.join("os"),
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        stores: root.join("no-stores.toml"),
        pki: root.join("pki"),
        console: None,
        managed_config: None,
    }
}

/// The result of the start of the host's ports over `host`.
fn start(rig: &Rig, host: &HostFiles) -> Result<Ports, StartError> {
    let adapters = Adapters::linux(
        &Platform::linux(),
        RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
    );
    host_ports(&daemon_config(), &rig.settings(), host, OWN, adapters)
}

/// Makes a Debian host under `root` whose apt tools are present and whose installed apt is
/// `version`.
fn debian_with_apt(root: &Path, version: &str) {
    let put = |file: &str, text: &str| {
        let path = root.join(file);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
        fs::write(path, text).expect("the file");
    };
    put("etc/os-release", "ID=debian\n");
    for tool in APT_TOOLS {
        put(tool, "");
    }
    put(
        DPKG_STATUS,
        &format!("Package: apt\nStatus: install ok installed\nVersion: {version}\n\n"),
    );
}

/// One valid repository entry named `main`, as the loader would hold it.
fn one_repository(dir: &Path) -> Repositories {
    let text = format!(
        "Types: deb\nURIs: https://repo.example/debian\nSuites: stable\nComponents: main\nSigned-By: {}/main.gpg\n",
        dir.display()
    );
    Repositories::new(vec![
        config::parse("main", &text, dir).expect("a sound stanza"),
    ])
}

/// Proves a start refuses a repositories directory it cannot trust and names it: a directory
/// that is writable by others (and so, as the test's user, not owned by root either) is
/// `StartError::Repositories` naming that directory, its text starting with the path, with the
/// config's exit status, and the packages directory is not touched. A path that is a regular
/// file is refused the same way.
///
/// Catches: a start that goes on past a repository configuration it cannot trust.
#[test]
fn an_unsound_repositories_directory_refuses_the_start_naming_it() {
    let rig = Rig::new("repositories-unsound");
    let dir = rig.scratch.path().join("repositories");
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("its mode");
    let file = rig.scratch.path().join("repositories-file");
    fs::write(&file, "").expect("a file");
    for path in [dir, file] {
        let host = files(rig.scratch.path(), Some(path.clone()));
        let error = start(&rig, &host).err().expect("the start is refused");
        assert!(
            matches!(&error, StartError::Repositories { path: named, .. } if *named == path),
            "{error}"
        );
        assert!(
            error.to_string().starts_with(&path.display().to_string()),
            "{error}"
        );
        assert_eq!(error.exit_code(), EXIT_CONFIG);
        assert!(
            !rig.packages.exists(),
            "the packages directory is not touched"
        );
    }
}

/// Proves a host with no repositories directory starts, and announces `hostkeeper-archive`
/// alone: a missing directory is no repository.
///
/// Catches: a missing directory being a defect that keeps a host from starting.
#[test]
fn a_missing_repositories_directory_starts() {
    let rig = Rig::new("repositories-missing");
    fs::create_dir_all(&rig.packages).expect("the packages directory");
    let host = files(
        rig.scratch.path(),
        Some(rig.scratch.path().join("no-repositories")),
    );
    let ports = start(&rig, &host).expect("the ports");
    assert_eq!(ports.announcement.names(), ["hostkeeper-archive"]);
}

/// Proves only a Linux host has a repositories path: systemd and OpenRC hosts name
/// `/etc/hostkeeper/repositories`, a Mac names none and reads none (no entries, and no error
/// for a path that is not there).
///
/// Catches: a Mac reading a directory it has no use for.
#[test]
fn only_a_linux_host_has_a_repositories_path() {
    let path = |kind| HostFiles::for_platform(kind, |_| false).repositories;
    let linux = Some(PathBuf::from(REPOSITORIES_DIR));
    assert_eq!(path(PlatformKind::Linux), linux);
    assert_eq!(path(PlatformKind::Openrc), linux);
    assert_eq!(path(PlatformKind::Macos), None);
    let rig = Rig::new("repositories-mac");
    let mac = files(rig.scratch.path(), None);
    assert!(repositories::load(&mac).expect("no directory").is_empty());
}

/// Proves a Debian host that could run apt (its tools, an installed apt 2.6 and a valid
/// repository entry) still announces `hostkeeper-archive` alone, because this build registers no
/// steps of apt, while the host's own check finds apt; a host with no repository and one with
/// apt below 2.2 give the same list.
///
/// Catches: an announcement of an installer whose entry this daemon would defer.
#[test]
fn a_debian_host_that_could_run_apt_announces_the_archive_alone() {
    let rig = Rig::new("repositories-announce");
    let root = rig.scratch.path().join("os");
    debian_with_apt(&root, "2.6.1");
    let base = announce(hostkeeper_install::announce::OsHost {
        macos: false,
        root: &root,
    });
    assert_eq!(base.family, Some(Family::Deb));
    let entries = one_repository(&rig.scratch.path().join("repositories"));
    assert_eq!(
        native(base.family, &root, &entries),
        Some(Installer::Apt),
        "the host could run apt"
    );
    let announced = repositories::announced(base.clone(), &root, &entries);
    assert_eq!(announced.names(), ["hostkeeper-archive"]);
    assert_eq!(announced.family, Some(Family::Deb));

    let none = repositories::announced(base.clone(), &root, &Repositories::default());
    assert_eq!(none.names(), ["hostkeeper-archive"]);
    debian_with_apt(&root, "2.1.9");
    assert_eq!(native(base.family, &root, &entries), None);
}

/// A manifest of `apt` for the package `mdm`.
fn apt_manifest() -> Manifest {
    Manifest {
        min_hostkeeper: "0.1.0".to_owned(),
        package: Some("mdm".to_owned()),
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

/// The entry of `apt` naming the repository `main`.
fn apt_entry() -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: d(),
        manifest: Some(apt_manifest()),
        repository: Some("main".to_owned()),
        package_version: Some("1.0.0-1".to_owned()),
        ..Target::default()
    }
}

/// `order` with `entry` as its target.
fn with_entry(mut order: Order, entry: &Target) -> Order {
    match order.verb.as_mut() {
        Some(Verb::Install(a)) => a.target = Some(entry.clone()),
        other => panic!("not an install: {other:?}"),
    }
    order
}

/// A daemon over the rig whose announcement lists `installers`.
fn daemon_announcing(rig: &Rig, installers: Vec<Installer>) -> Daemon {
    let ports = Ports {
        announcement: Announcement {
            installers,
            family: Some(Family::Deb),
        },
        ..rig.ports()
    };
    Daemon::new(daemon_config(), rig.dirs(), ports, OWN)
}

/// Nothing was asked of the package port, no step ran and nothing was written for the service.
fn untouched(rig: &Rig) {
    assert!(rig.store.asked().is_empty(), "nothing is fetched");
    assert_eq!(rig.launcher.steps(SERVICE), Vec::<Step>::new());
    assert!(rig.record(SERVICE).is_none(), "no step record");
    assert!(!rig.packages.join(SERVICE).exists());
}

/// Proves the entry's kind is judged on receipt, after the installer is known to be listed:
/// with a daemon that lists `apt`, an apt entry that names no repository is deferred
/// `coordinator_too_old` for 300 seconds, an apt entry that names a location, bytes or an
/// attestation is refused `order_invalid`, and a `hostkeeper-archive` entry that names a
/// repository or a package version is refused `order_invalid`; none fetches or writes anything.
///
/// Catches: an entry of the wrong kind reaching the package port.
#[test]
fn an_entry_of_the_wrong_kind_is_judged_on_receipt() {
    let rig = Rig::new("repositories-kind");
    let daemon = daemon_announcing(&rig, vec![Installer::HostkeeperArchive, Installer::Apt]);
    let order = |entry: &Target| with_entry(install(SERVICE, VERSION, &d(), 1), entry);

    let no_repository = Target {
        repository: None,
        ..apt_entry()
    };
    assert_eq!(
        daemon.order(&order(&no_repository)),
        Reply::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(300)
            },
            reason: Some("coordinator_too_old".to_owned()),
            resent: false,
        }
    );
    let located = Target {
        locations: vec![Location {
            store: "channel".to_owned(),
            locator: "pkg/mdm.deb".to_owned(),
        }],
        ..apt_entry()
    };
    let sized = Target {
        bytes: 5,
        ..apt_entry()
    };
    let archive = Target {
        installer: "hostkeeper-archive".to_owned(),
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            ..Manifest::default()
        }),
        package_version: None,
        ..apt_entry()
    };
    let versioned = Target {
        repository: None,
        package_version: Some("1.0.0-1".to_owned()),
        ..archive.clone()
    };
    for entry in [located, sized, archive, versioned] {
        let reply = daemon.order(&order(&entry));
        assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    }
    untouched(&rig);
}

/// Proves the listing is judged before the kind: with a daemon that lists `hostkeeper-archive`
/// alone, an apt entry (whatever its kind defects) is `installer_not_listed`, deferred for one
/// second, not `coordinator_too_old` or `order_invalid`; a sound apt entry reaches no step
/// either.
///
/// Catches: the kind check answering for an installer the daemon does not list.
#[test]
fn an_apt_entry_not_listed_is_still_installer_not_listed() {
    let rig = Rig::new("repositories-not-listed");
    let daemon = daemon_announcing(&rig, vec![Installer::HostkeeperArchive]);
    let not_listed = Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(1),
        },
        reason: Some("installer_not_listed".to_owned()),
        resent: false,
    };
    let no_repository = Target {
        repository: None,
        ..apt_entry()
    };
    for entry in [apt_entry(), no_repository] {
        let order = with_entry(install(SERVICE, VERSION, &d(), 1), &entry);
        assert_eq!(daemon.order(&order), not_listed);
    }
    untouched(&rig);
}
