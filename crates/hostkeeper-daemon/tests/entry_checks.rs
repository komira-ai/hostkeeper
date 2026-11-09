//! The entry an order carries and the files the host build reads (packages.md sections 3.3, 4.4
//! and 7.4): an order's entry is judged before anything is staged, an order with none is refused
//! before any port, the installers the daemon announces are the ones its receipt check lists, and
//! the start refuses a `macos-pkg.toml` it cannot trust.

mod support;
mod support_apt;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE};
use hostkeeper_daemon::cli::{EXIT_CONFIG, HostFiles, SIGNERS_FILE, host_ports, load_signers};
use hostkeeper_daemon::entry::MACOS_PKG_ATTESTED;
use hostkeeper_daemon::start::StartError;
use hostkeeper_daemon::{Daemon, Ports, Refusal, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_install::{Announcement, Family};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Manifest, Target};
use hostkeeper_release::{Installer, grammar::check_locator};
use support::{OWN, Rig, config, d, force, force_rollback, install, my_uid, update};
use support_apt::{AptRig, SERVICE, entry, with_entry};

const VERSION: &str = "1.5.0";

fn location(store: &str, locator: &str) -> Location {
    Location {
        store: store.to_owned(),
        locator: locator.to_owned(),
    }
}

fn not_listed() -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(1),
        },
        reason: Some("installer_not_listed".to_owned()),
        resent: false,
    }
}

/// A host that announces apt, an entry of the service whose digest is the order's, and the
/// order's package digest.
struct World {
    apt: AptRig,
    entry: Target,
}

impl World {
    fn new(name: &str) -> World {
        let apt = AptRig::new(name);
        let entry = entry(&format!("{VERSION}-1"), &d());
        World { apt, entry }
    }

    fn daemon(&self) -> Daemon {
        self.apt.daemon()
    }

    /// An `install` of the service carrying `entry`.
    fn install(&self, entry: &Target) -> Order {
        with_entry(install(SERVICE, VERSION, &d(), 1), entry)
    }

    /// Nothing was run, staged or written for the order: no tool of the package manager ran, no
    /// step record and no package directory were made.
    fn untouched(&self) {
        assert_eq!(self.apt.host.calls().len(), 0, "no tool ran");
        assert!(self.apt.rig.record(SERVICE).is_none(), "no step record");
        assert!(
            !self.apt.rig.packages.join(SERVICE).exists(),
            "no package directory"
        );
    }
}

use hostkeeper_proto::hostkeeper::agent::v1::Order;

/// Catches an entry whose digest is not the order's `package_digest` being staged or run: it is
/// refused `order_invalid`, and nothing is run, staged or written.
#[test]
fn an_entry_whose_digest_is_not_the_orders_package_digest_is_order_invalid() {
    let world = World::new("entries-digest");
    let daemon = world.daemon();
    let wrong = Target {
        digest: support::digest("cd"),
        ..world.entry.clone()
    };
    let reply = daemon.order(&world.install(&wrong));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    world.untouched();
}

/// Catches a `macos-pkg` entry that carries an attestation being staged, or deferred as though it
/// could be checked: it is refused `order_invalid`, nothing run or written; the same entry with
/// no attestation is not `order_invalid` (this daemon does not list `macos-pkg`, so it is
/// deferred `installer_not_listed`).
#[test]
fn a_macos_pkg_entry_with_an_attestation_is_order_invalid_and_one_without_is_not() {
    let world = World::new("entries-macos-pkg-attested");
    let daemon = world.daemon();
    let mut attested = world.entry.clone();
    attested.installer = "macos-pkg".into();
    attested.attestation = Some(hostkeeper_proto::hostkeeper::release::v1::Blob {
        digest: d(),
        bytes: 1,
        locations: vec![location("channel", "pkg/x.sigstore.json")],
    });
    let reply = daemon.order(&world.install(&attested));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    let Reply::Refused { detail, .. } = &reply else {
        panic!("a refusal: {reply:?}")
    };
    assert_eq!(detail, MACOS_PKG_ATTESTED);
    world.untouched();
    let mut bare = attested;
    bare.attestation = None;
    let reply = daemon.order(&world.install(&bare));
    assert_ne!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    world.untouched();
}

/// Catches a location that leaves its store being used: a locator with `..`, `%`, a scheme, `//`,
/// a query or 1025 bytes, in the artifact's locations or the attestation's, and a store name that
/// is not one, each refused `order_invalid` with the grammar's own words, nothing run.
#[test]
fn a_location_that_breaks_the_locator_or_store_grammar_is_order_invalid_and_nothing_is_run() {
    use hostkeeper_proto::hostkeeper::release::v1::Blob;
    let long = "a".repeat(1025);
    let locators = [
        "a/../b",
        "a%2Fb",
        "https:evil.example/x",
        "a//b",
        "a?x=1",
        long.as_str(),
    ];
    let world = World::new("entries-locators");
    let daemon = world.daemon();
    let with_blob = Target {
        attestation: Some(Blob {
            digest: d(),
            bytes: 1,
            locations: vec![location("channel", "pkg/x.sigstore.json")],
        }),
        ..world.entry.clone()
    };
    for (n, locator) in locators.into_iter().enumerate() {
        let mut artifact = with_blob.clone();
        artifact.locations.push(location("channel", locator));
        let mut bundle = with_blob.clone();
        bundle
            .attestation
            .as_mut()
            .expect("an attestation")
            .locations
            .push(location("channel", locator));
        for entry in [artifact, bundle] {
            let order = with_entry(install(SERVICE, VERSION, &d(), n as u64 + 1), &entry);
            let reply = daemon.order(&order);
            assert_eq!(
                reply.refusal(),
                Some(Refusal::OrderInvalid),
                "{locator}: {reply:?}"
            );
            let Reply::Refused { detail, .. } = &reply else {
                panic!("a refusal: {reply:?}")
            };
            let why = check_locator(locator).expect_err("the locator breaks a rule");
            assert!(detail.contains(&why.to_string()), "{detail}");
            world.untouched();
        }
    }
    let mut entry = world.entry.clone();
    entry.locations.push(location("Bad_Store", "a/b"));
    let reply = daemon.order(&world.install(&entry));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    world.untouched();
}

/// Catches an installer the daemon does not list being judged as a bad manifest or staged: an
/// entry of `hostkeeper-archive` (an A24 coordinator still lists it), `tarball`, `deb` and of a
/// name no daemon knows are each `Deferred` for a second with the reason `installer_not_listed`,
/// not `manifest_invalid`, nothing run and nothing written.
#[test]
fn an_installer_the_daemon_does_not_list_is_deferred_and_nothing_is_run() {
    let world = World::new("entries-unlisted");
    let daemon = world.daemon();
    for installer in ["tarball", "deb", "some-future-installer"] {
        let mut entry = world.entry.clone();
        entry.installer = installer.to_owned();
        let reply = daemon.order(&world.install(&entry));
        assert_eq!(reply, not_listed(), "{installer}");
        world.untouched();
    }
    // An artifact entry of the archive installer, as an A24 coordinator still lists it.
    let archive = Target {
        installer: "hostkeeper-archive".to_owned(),
        digest: d(),
        bytes: 1,
        manifest: Some(Manifest {
            min_hostkeeper: "0.1.0".to_owned(),
            ..Manifest::default()
        }),
        ..Target::default()
    };
    assert_eq!(daemon.order(&world.install(&archive)), not_listed());
    world.untouched();
}

/// Catches an entry with no manifest reaching the stage: it is refused `manifest_invalid` before
/// anything is run.
#[test]
fn an_entry_with_no_manifest_is_manifest_invalid_before_any_stage() {
    let world = World::new("entries-manifest");
    let daemon = world.daemon();
    let mut none = world.entry.clone();
    none.manifest = None;
    let reply = daemon.order(&world.install(&none));
    assert_eq!(reply.refusal(), Some(Refusal::ManifestInvalid), "{reply:?}");
    world.untouched();
}

/// Catches a daemon older than the manifest's `min_hostkeeper` staging the package: the order is
/// refused `daemon_too_old` before anything is run.
#[test]
fn a_manifest_that_needs_a_newer_daemon_is_daemon_too_old_before_any_stage() {
    let world = World::new("entries-too-old");
    let daemon = world.daemon();
    let mut entry = world.entry.clone();
    entry.manifest = Some(Manifest {
        min_hostkeeper: "9.0.0".to_owned(),
        ..entry.manifest.expect("a manifest")
    });
    let reply = daemon.order(&world.install(&entry));
    assert_eq!(reply.refusal(), Some(Refusal::DaemonTooOld), "{reply:?}");
    world.untouched();
}

/// Catches a verb that skips the entry's checks: `update`, `force_rollback`, `install` and `force`
/// each refuse an entry whose digest is not the order's with `order_invalid` and defer an entry of
/// an installer not listed, nothing run for any of them.
#[test]
fn every_verb_that_names_a_package_judges_its_entry_before_it_stages() {
    let world = World::new("entries-verbs");
    world.apt.rig.add("web", "2.0.0");
    let calls = world.apt.host.calls().len();
    let daemon = world.daemon();
    let wrong_digest = Target {
        digest: support::digest("cd"),
        ..world.entry.clone()
    };
    let tarball = Target {
        installer: "tarball".to_owned(),
        ..world.entry.clone()
    };
    let orders = |entry: &Target, n: u64| {
        [
            update("web", "2.0.0", &d(), n),
            force_rollback("web", "2.0.0", &d(), n),
            install("fresh", "1.0.0", &d(), n),
            force("web", "2.0.0", &d(), n, 0),
        ]
        .map(|order| with_entry(order, entry))
    };
    for order in orders(&wrong_digest, 1) {
        let reply = daemon.order(&order);
        assert_eq!(
            reply.refusal(),
            Some(Refusal::OrderInvalid),
            "{order:?}: {reply:?}"
        );
    }
    for order in orders(&tarball, 1) {
        assert_eq!(daemon.order(&order), not_listed(), "{order:?}");
    }
    assert_eq!(world.apt.host.calls().len(), calls, "no tool ran");
}

/// Catches an order with no entry reaching a port: `update`, `force_rollback`, `install` and
/// `force`, and the update of hostkeeper itself, are each refused `order_invalid` before any tool
/// of the package manager runs and before anything is staged, written or recorded.
#[test]
fn an_order_without_an_entry_is_refused_before_any_port() {
    let world = World::new("entries-none");
    world.apt.rig.add("web", "2.0.0");
    world.apt.rig.add("hostkeeper", "0.2.0");
    let calls = world.apt.host.calls().len();
    let before = support::snapshot(&world.apt.rig.packages);
    let daemon = world.daemon();
    let orders = [
        update("web", "2.0.0", &d(), 1),
        force_rollback("web", "2.0.0", &d(), 2),
        install("fresh", "1.0.0", &d(), 3),
        force("web", "2.0.0", &d(), 4, 0),
        update("hostkeeper", "0.2.0", &d(), 5),
    ];
    for order in orders {
        let reply = daemon.order(&order);
        assert_eq!(
            reply.refusal(),
            Some(Refusal::OrderInvalid),
            "{order:?}: {reply:?}"
        );
    }
    assert_eq!(world.apt.host.calls().len(), calls, "no tool ran");
    assert_eq!(support::snapshot(&world.apt.rig.packages), before);
    assert!(world.apt.rig.record("web").is_none());
    assert!(world.apt.rig.record("fresh").is_none());
}

/// Catches an entry of an installer the daemon announces but has no backend of being staged by
/// another: a daemon that announces `macos-pkg` (a Mac's, once it is wired) and is ordered such an
/// entry on this build refuses it `order_invalid`, naming the installer, nothing run.
#[test]
fn an_announced_installer_with_no_backend_is_order_invalid() {
    let world = World::new("entries-no-backend");
    let ports = Ports {
        announcement: Announcement {
            installers: vec![Installer::MacosPkg],
            family: None,
        },
        ..world.apt.ports()
    };
    let daemon = Daemon::new(config(), world.apt.rig.dirs(), ports, OWN);
    let mut mac = world.entry.clone();
    mac.installer = "macos-pkg".to_owned();
    mac.repository = None;
    mac.package_version = None;
    let reply = daemon.order(&world.install(&mac));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    let Reply::Refused { detail, .. } = &reply else {
        panic!("a refusal: {reply:?}")
    };
    assert!(detail.contains("macos-pkg"), "{detail}");
    world.untouched();
}

/// The host files of a host with the stores file `stores`, under `root`.
fn files(root: &Path, stores: PathBuf) -> HostFiles {
    HostFiles {
        repositories: None,
        os_root: root.join("os"),
        stores,
        macos_pkg_signers: None,
        pki: root.join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    }
}

/// Catches the heartbeat and the receipt check listing different installers, and a start that
/// does not decide them from the host: the host's ports announce no installer on their own (a
/// host with no repository entry announces none, its family `deb` from the `os-release` under the
/// files' `os_root`, and on a Mac none, which never reads the file; no file: no family), the
/// daemon holds that announcement for every heartbeat of its start, and the receipt check judges
/// an entry against the same list (`apt`, which the host did not announce, is
/// `installer_not_listed`).
#[test]
fn the_installers_announced_are_the_ones_the_receipt_check_lists() {
    let rig = Rig::new("entries-announce");
    let host = files(
        rig.scratch.path(),
        rig.scratch.path().join("no-stores.toml"),
    );
    let ports = |host: &HostFiles| {
        let adapters = Adapters::linux(
            &Platform::linux(),
            RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
        );
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        host_ports(&rig.settings(), host, OWN, adapters).expect("the ports")
    };
    let none = ports(&host).announcement;
    assert!(none.names().is_empty());
    assert_eq!(none.family, None);
    let etc = host.os_root.join("etc");
    fs::create_dir_all(&etc).expect("etc under the root");
    fs::write(etc.join("os-release"), "ID=debian\n").expect("os-release");
    let debian = ports(&host).announcement;
    assert!(debian.names().is_empty());
    // A Mac never reads os-release, so it has no family whatever the file says.
    let family = (!cfg!(target_os = "macos")).then_some(Family::Deb);
    assert_eq!(debian.family, family);

    let world = World::new("entries-announce-daemon");
    let daemon = world.daemon();
    assert_eq!(daemon.ctx().announcement.names(), ["apt"]);
    let bare = Daemon::new(config(), world.apt.rig.dirs(), world.apt.rig.ports(), OWN);
    assert!(bare.ctx().announcement.names().is_empty());
    assert_eq!(bare.order(&world.install(&world.entry)), not_listed());
    world.untouched();
}

/// The host files of a Mac with `signers` as its signers file, under `root`.
fn mac_files(root: &Path, signers: PathBuf) -> HostFiles {
    HostFiles {
        macos_pkg_signers: Some(signers),
        ..files(root, root.join("stores.toml"))
    }
}

/// The signers file text of one signer.
const ONE_SIGNER: &str =
    "signers = [\"0000000000000000000000000000000000000000000000000000000000000001\"]\n";

/// Catches a start that goes on past a `macos-pkg.toml` it cannot trust: a signers file writable
/// by others is `StartError::MacosPkg` naming that file, with the config's exit status, before the
/// packages directory is touched.
#[test]
fn an_unsound_signers_file_refuses_the_start_naming_the_file() {
    let rig = Rig::new("signers-start");
    let path = rig.scratch.path().join("macos-pkg.toml");
    fs::write(&path, ONE_SIGNER).expect("the signers file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("its mode");
    let adapters = Adapters::linux(
        &Platform::linux(),
        RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
    );
    let host = mac_files(rig.scratch.path(), path.clone());
    let error = host_ports(&rig.settings(), &host, OWN, adapters)
        .err()
        .expect("the start is refused");
    assert!(
        matches!(&error, StartError::MacosPkg { path: named, .. } if *named == path),
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

/// Catches a start that refuses a Mac with no signers file, or that drops a sound one: no file
/// loads as none, and a sound file owned by the expected uid loads its signer.
#[test]
fn a_missing_signers_file_starts_and_a_sound_one_loads() {
    let rig = Rig::new("signers-load");
    let path = rig.scratch.path().join("macos-pkg.toml");
    let host = mac_files(rig.scratch.path(), path.clone());
    assert!(
        load_signers(&host, my_uid())
            .expect("no file starts")
            .is_none()
    );
    fs::write(&path, ONE_SIGNER).expect("the signers file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("its mode");
    let signers = load_signers(&host, my_uid())
        .expect("a sound file loads")
        .expect("its signers");
    assert_eq!(signers.fingerprints().len(), 1);
    let wrong = load_signers(&host, my_uid() + 1).expect_err("another owner is refused");
    assert_eq!(wrong.exit_code(), EXIT_CONFIG);
}

/// Catches a Linux host reading the Mac's signers file: Linux and OpenRC have no macos-pkg path,
/// a Mac has `SIGNERS_FILE`, and a host with no path loads no signers.
#[test]
fn only_a_mac_has_a_signers_file_path() {
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let host = HostFiles::for_platform(kind);
        assert_eq!(host.macos_pkg_signers, None);
        assert!(load_signers(&host, 0).expect("nothing to read").is_none());
    }
    let mac = HostFiles::for_platform(PlatformKind::Macos);
    assert_eq!(mac.macos_pkg_signers, Some(PathBuf::from(SIGNERS_FILE)));
}
