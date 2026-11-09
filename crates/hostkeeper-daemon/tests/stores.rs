//! The daemon's host stores and the entry an order carries (packages.md sections 3.3, 4.4, 7.1,
//! 7.2 and 7.4): the start refuses a `stores.toml` that is not sound, an order's entry is judged
//! before anything is fetched, and an entry that passes is fetched from its locations and the
//! host's mirrors, proven, and unpacked into the layout an older binary reads.

mod support;
mod support_archive;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::adapters::{Adapters, RunnerLauncher, SYSTEMCTL_DEADLINE};
use hostkeeper_daemon::cli::{EXIT_CONFIG, HostFiles, host_ports};
use hostkeeper_daemon::real_packages::{Log, RealPackages};
use hostkeeper_daemon::start::StartError;
use hostkeeper_daemon::{Daemon, Ports, Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_launchd::Platform;
use hostkeeper_link::{ANNOUNCED, Feature};
use hostkeeper_observe::fake::FakeCommandRunner;
use hostkeeper_package::stores::{self, Rule, Stores, StoresError};
use hostkeeper_proto::hostkeeper::agent::v1::{Order, order::Verb};
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Manifest, Target};
use hostkeeper_release::grammar::check_locator;
use support::{OWN, Rig, config, d, force, force_rollback, install, my_uid, update};
use support_archive::{
    Answer, Rig as ArchiveRig, SERVICE, VERSION, bare_archive, bundle, sha256_hex,
};

/// Where the fixture's artifact is in the channel store, and its bundle.
const PKG: &str = "pkg/tierstore-1.5.0.tar.zst";
const BUNDLE: &str = "pkg/tierstore-1.5.0.sigstore.json";

fn location(store: &str, locator: &str) -> Location {
    Location {
        store: store.to_owned(),
        locator: locator.to_owned(),
    }
}

/// A manifest of `hostkeeper-archive`: the version it needs and nothing else.
fn bare_manifest(min_hostkeeper: &str) -> Manifest {
    Manifest {
        min_hostkeeper: min_hostkeeper.to_owned(),
        ..Manifest::default()
    }
}

/// The entry of `archive`, which the channel store has under [`PKG`] with its bundle.
fn entry_of(archive: &[u8]) -> Target {
    let text = bundle(&sha256_hex(archive));
    Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: vec![location("channel", PKG)],
        digest: sha256_hex(archive),
        bytes: archive.len() as u64,
        attestation: Some(Blob {
            digest: sha256_hex(text.as_bytes()),
            bytes: text.len() as u64,
            locations: vec![location("channel", BUNDLE)],
        }),
        manifest: Some(bare_manifest("0.1.0")),
        repository: None,
        package_version: None,
    }
}

/// `order` with `entry` as the target it carries.
fn with_entry(mut order: Order, entry: &Target) -> Order {
    match order.verb.as_mut() {
        Some(Verb::Update(a)) => a.target = Some(entry.clone()),
        Some(Verb::ForceRollback(a)) => a.target = Some(entry.clone()),
        Some(Verb::Install(a)) => a.target = Some(entry.clone()),
        Some(Verb::Force(a)) => a.target = Some(entry.clone()),
        other => panic!("not an order that names a package: {other:?}"),
    }
    order
}

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
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

/// The real package store over the rig's packages directory, with fakes for the network and the
/// verifier, and a daemon over it.
struct World {
    rig: Rig,
    arch: ArchiveRig,
    archive: Vec<u8>,
    entry: Target,
    channel: Stores,
}

impl World {
    fn new(name: &str) -> World {
        let rig = Rig::new(name);
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        for dir in [rig.scratch.path(), &rig.packages] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        let arch = ArchiveRig::new(&format!("{name}-fakes"));
        let archive = bare_archive();
        let channel = Stores::channel(&arch.config.release_channel);
        World {
            entry: entry_of(&archive),
            rig,
            arch,
            archive,
            channel,
        }
    }

    /// The URL of `locator` in the store `name` of `stores` for the blob with SHA-256 `hex`.
    fn url(stores: &Stores, name: &str, locator: &str, hex: &str) -> String {
        let store = stores.get(name).expect("the store");
        store.blob_url(locator, hex).as_str().to_owned()
    }

    /// The artifact and the bundle of the entry as the channel store serves them.
    fn entry_routes(&self) -> Vec<(String, Vec<u8>)> {
        let text = bundle(&sha256_hex(&self.archive)).into_bytes();
        vec![
            (
                World::url(&self.channel, "channel", PKG, &self.entry.digest),
                self.archive.clone(),
            ),
            (
                World::url(&self.channel, "channel", BUNDLE, &sha256_hex(&text)),
                text,
            ),
        ]
    }

    /// The index, the package and the bundle as the release channel serves them.
    fn channel_routes(&self) -> Vec<(String, Vec<u8>)> {
        let bodies = [
            support_archive::index(&self.archive).into_bytes(),
            self.archive.clone(),
            bundle(&sha256_hex(&self.archive)).into_bytes(),
        ];
        self.arch.urls().into_iter().zip(bodies).collect()
    }

    fn open(&self, routes: Vec<(String, Vec<u8>)>) -> RealPackages {
        self.arch
            .open_at(&self.rig.packages, routes, Answer::Confirms)
            .expect("open the store")
    }

    fn daemon(&self, packages: RealPackages) -> Daemon {
        let ports = Ports {
            packages: Arc::new(packages),
            ..self.rig.ports()
        };
        Daemon::new(config(), self.rig.dirs(), ports, OWN)
    }

    /// What the fake network was asked, in order.
    fn fetched(&self) -> Vec<String> {
        self.arch.fetched.lock().expect("the log").clone()
    }

    /// An `install` of the fixture carrying `entry`.
    fn install(&self, entry: &Target) -> Order {
        with_entry(install(SERVICE, VERSION, &d(), 1), entry)
    }

    /// The order to install the fixture with the entry's own digest.
    fn good_install(&self) -> Order {
        with_entry(
            install(SERVICE, VERSION, &self.entry.digest, 1),
            &self.entry,
        )
    }

    /// Nothing was fetched, verified or written for the order.
    fn untouched(&self) {
        assert_eq!(self.fetched(), Vec::<String>::new(), "nothing is fetched");
        assert!(self.arch.verified.lock().expect("the log").is_empty());
        assert!(self.rig.record(SERVICE).is_none(), "no step record");
        assert!(
            !self.rig.packages.join(SERVICE).exists(),
            "no package directory"
        );
    }
}

/// A stores file `text` in `dir`, mode 0644.
fn put_stores(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("stores.toml");
    fs::write(&path, text).expect("write the stores file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("its mode");
    path
}

/// Catches an order with an entry that is fetched through the release index, a fetch from
/// anywhere but the entry's location, a proof of other bytes than the entry's, or a version
/// directory that differs from the one an older binary reads: with no `stores.toml` the entry's
/// `channel` location is the release channel, the fetch asks the artifact and then the bundle and
/// no index, the verifier is asked about the entry's digest, the hook runs, the order ends
/// `Succeeded`, and the version directory holds the archive's files and no `target.json`.
#[test]
fn an_order_with_an_entry_installs_hostkeeper_archive_through_the_entry() {
    let world = World::new("stores-install");
    let daemon = world.daemon(world.open(world.entry_routes()));
    let reply = daemon.order(&world.good_install());
    assert_eq!(reply, succeeded(), "{reply:?}");

    let routes = world.entry_routes();
    let urls: Vec<&str> = routes.iter().map(|(url, _)| url.as_str()).collect();
    assert_eq!(world.fetched(), urls);
    assert!(
        urls[0].starts_with("https://releases.example/pkg/"),
        "{urls:?}"
    );
    assert_eq!(
        *world.arch.verified.lock().expect("the log"),
        vec![world.entry.digest.clone()]
    );
    assert_eq!(
        world.rig.launcher.steps(SERVICE),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    let dir = world.rig.packages.join(SERVICE).join(VERSION);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("the version directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names, ["bin", "manifest.toml"], "today's layout");
    assert!(!dir.join("target.json").exists());
}

/// Catches a version staged through an entry that an older binary could not read, roll back to
/// or uninstall as it does a version staged through the channel: the same archive installed
/// once through its entry and once through the release index gives two version directories with
/// one tree digest and one seal, byte for byte, and the same `placed.json`, so every reader of
/// the channel's layout, the binaries before this one included, reads the first as the second.
#[test]
fn a_version_staged_through_the_entry_is_the_one_the_channel_stages() {
    let through_entry = World::new("stores-same-entry");
    let reply = through_entry
        .daemon(through_entry.open(through_entry.entry_routes()))
        .order(&through_entry.good_install());
    assert_eq!(reply, succeeded(), "{reply:?}");
    let through_channel = World::new("stores-same-channel");
    let order = install(SERVICE, VERSION, &sha256_hex(&through_channel.archive), 1);
    let reply = through_channel
        .daemon(through_channel.open(through_channel.channel_routes()))
        .order(&order);
    assert_eq!(reply, succeeded(), "{reply:?}");

    let staged = |world: &World| {
        let service = world.rig.packages.join(SERVICE);
        let digest = hostkeeper_steps::tree_digest(&service.join(VERSION)).expect("its digest");
        let seal = fs::read(service.join(format!("{VERSION}.seal"))).expect("its seal");
        (digest, seal, format!("{:?}", world.rig.placed_record()))
    };
    assert_eq!(staged(&through_entry), staged(&through_channel));
}

/// Catches an order with no target being sent through the entry path, or the channel path
/// changing because `target-v1` is announced: the daemon announces `target-v1`, and an `install`
/// with no target asks the release index first, then the package and its bundle, from the release
/// channel.
#[test]
fn an_order_without_a_target_takes_the_channel_path_though_target_v1_is_announced() {
    assert!(ANNOUNCED.contains(&Feature::TargetV1));
    let world = World::new("stores-channel");
    let daemon = world.daemon(world.open(world.channel_routes()));
    let order = install(SERVICE, VERSION, &sha256_hex(&world.archive), 1);
    let reply = daemon.order(&order);
    assert_eq!(reply, succeeded(), "{reply:?}");
    assert_eq!(world.fetched(), world.arch.urls());
    assert!(
        world.fetched()[0].ends_with("index.json"),
        "{:?}",
        world.fetched()
    );
}

/// Catches an entry whose digest is not the order's `package_digest` being fetched or run: it is
/// refused `order_invalid`, nothing is fetched, verified or written.
#[test]
fn an_entry_whose_digest_is_not_the_orders_package_digest_is_order_invalid() {
    let world = World::new("stores-digest");
    let daemon = world.daemon(world.open(world.entry_routes()));
    let reply = daemon.order(&world.install(&world.entry));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    world.untouched();
}

/// Catches a location that leaves its store being used: a locator with `..`, `%`, a scheme, `//`,
/// a query or 1025 bytes, in the artifact's locations or the attestation's, and a store name that
/// is not one, each refused `order_invalid` with the grammar's own words, nothing fetched.
#[test]
fn a_location_that_breaks_the_locator_or_store_grammar_is_order_invalid_and_nothing_is_fetched() {
    let long = "a".repeat(1025);
    let locators = [
        "a/../b",
        "a%2Fb",
        "https:evil.example/x",
        "a//b",
        "a?x=1",
        long.as_str(),
    ];
    let world = World::new("stores-locators");
    let daemon = world.daemon(world.open(world.entry_routes()));
    for (n, locator) in locators.into_iter().enumerate() {
        let mut artifact = world.entry.clone();
        artifact.locations.push(location("channel", locator));
        let mut bundle = world.entry.clone();
        bundle
            .attestation
            .as_mut()
            .expect("an attestation")
            .locations
            .push(location("channel", locator));
        for entry in [artifact, bundle] {
            let order = with_entry(
                install(SERVICE, VERSION, &world.entry.digest, n as u64 + 1),
                &entry,
            );
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
    let reply = daemon.order(&world.good_install_with(&entry));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    world.untouched();
}

impl World {
    fn good_install_with(&self, entry: &Target) -> Order {
        with_entry(install(SERVICE, VERSION, &self.entry.digest, 1), entry)
    }
}

/// Catches an installer the daemon does not list being judged as a bad manifest or fetched: an
/// entry of `tarball` (whose manifest would break that installer's rules), of `deb` and of a
/// name no daemon knows are each `Deferred` for a second with the reason `installer_not_listed`,
/// not `manifest_invalid`, nothing fetched and nothing written.
#[test]
fn an_installer_the_daemon_does_not_list_is_deferred_and_nothing_is_fetched() {
    let world = World::new("stores-unlisted");
    let daemon = world.daemon(world.open(world.entry_routes()));
    for installer in ["tarball", "deb", "some-future-installer"] {
        let mut entry = world.entry.clone();
        entry.installer = installer.to_owned();
        let reply = daemon.order(&world.good_install_with(&entry));
        assert_eq!(reply, not_listed(), "{installer}");
        world.untouched();
    }
}

/// Catches a manifest the installer refuses reaching the fetch: a `hostkeeper-archive` manifest
/// with a `run` entry (that installer's manifest is inside the archive), and an entry with no
/// manifest at all, are refused `manifest_invalid` before anything is fetched.
#[test]
fn a_manifest_the_installer_refuses_is_manifest_invalid_before_any_fetch() {
    let world = World::new("stores-manifest");
    let daemon = world.daemon(world.open(world.entry_routes()));
    let mut with_run = world.entry.clone();
    with_run.manifest = Some(Manifest {
        run: vec!["tierstore.service".to_owned()],
        ..bare_manifest("0.1.0")
    });
    let mut none = world.entry.clone();
    none.manifest = None;
    for entry in [with_run, none] {
        let reply = daemon.order(&world.good_install_with(&entry));
        assert_eq!(reply.refusal(), Some(Refusal::ManifestInvalid), "{reply:?}");
        world.untouched();
    }
}

/// Catches a daemon older than the manifest's `min_hostkeeper` fetching the package: the order
/// is refused `daemon_too_old` before any fetch, whatever the installer, since the version is
/// judged first.
#[test]
fn a_manifest_that_needs_a_newer_daemon_is_daemon_too_old_before_any_fetch() {
    let world = World::new("stores-too-old");
    let daemon = world.daemon(world.open(world.entry_routes()));
    for installer in ["hostkeeper-archive", "tarball"] {
        let mut entry = world.entry.clone();
        entry.installer = installer.to_owned();
        entry.manifest = Some(bare_manifest("9.0.0"));
        let reply = daemon.order(&world.good_install_with(&entry));
        assert_eq!(reply.refusal(), Some(Refusal::DaemonTooOld), "{reply:?}");
        world.untouched();
    }
}

/// Catches a verb that skips the entry's checks: `update`, `force_rollback`, `install` and `force`
/// each refuse an entry whose digest is not the order's with `order_invalid`, and defer an entry of
/// an installer not listed, and the package port is asked for none of them.
#[test]
fn every_verb_that_names_a_package_judges_its_entry_before_the_port() {
    let rig = Rig::new("stores-verbs");
    rig.add("web", "2.0.0");
    rig.stage("fresh", "1.0.0");
    let daemon = rig.daemon();
    let sound = Target {
        installer: "hostkeeper-archive".to_owned(),
        digest: d(),
        bytes: 1,
        manifest: Some(bare_manifest("0.1.0")),
        ..Target::default()
    };
    let wrong_digest = Target {
        digest: support::digest("cd"),
        ..sound.clone()
    };
    let tarball = Target {
        installer: "tarball".to_owned(),
        ..sound
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
    assert_eq!(rig.store.asked(), Vec::<(String, String)>::new());
}

/// Catches an entry dropped on its way to the package port: a sound entry reaches the port
/// unchanged with an `install`, and an order with no entry reaches it with none.
#[test]
fn a_sound_entry_reaches_the_package_port_and_an_order_without_one_reaches_it_with_none() {
    let rig = Rig::new("stores-port");
    rig.stage("fresh", "1.0.0");
    rig.stage("other", "1.0.0");
    let daemon = rig.daemon();
    let entry = Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: vec![location("channel", PKG)],
        digest: d(),
        bytes: 1,
        manifest: Some(bare_manifest("0.1.0")),
        ..Target::default()
    };
    let reply = daemon.order(&with_entry(install("fresh", "1.0.0", &d(), 1), &entry));
    assert_eq!(reply, succeeded(), "{reply:?}");
    let reply = daemon.order(&install("other", "1.0.0", &d(), 1));
    assert_eq!(reply, succeeded(), "{reply:?}");
    assert_eq!(rig.store.entries(), [Some(entry), None]);
}

/// Catches an entry naming a store the host does not define being fetched from somewhere else, or
/// failing with another token: a `stores.toml` with no `channel` store and no mirror gives
/// `no_store`, nothing fetched.
#[test]
fn an_entry_whose_stores_the_host_does_not_define_is_no_store() {
    let world = World::new("stores-no-store");
    let path = put_stores(
        world.rig.scratch.path(),
        "[[store]]\nname = \"other\"\nkind = \"https\"\nbase = \"https://other.example/\"\n",
    );
    let loaded = stores::load(&path, &world.arch.config.release_channel, my_uid()).expect("loads");
    let packages = world.open(world.entry_routes()).with_stores(loaded);
    let daemon = world.daemon(packages);
    let reply = daemon.order(&world.good_install());
    assert_eq!(reply.refusal(), Some(Refusal::NoStore), "{reply:?}");
    assert_eq!(world.fetched(), Vec::<String>::new());
}

/// A `stores.toml` with the release channel and the `lan` mirror, loaded for the running user.
fn with_mirror(world: &World) -> Stores {
    let path = put_stores(
        world.rig.scratch.path(),
        "[[store]]\nname = \"channel\"\nkind = \"https\"\nbase = \"https://releases.example/\"\n\
         [[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://lan.example/\"\n\
         mirror = true\nrepository = \"releases\"\n",
    );
    stores::load(&path, &world.arch.config.release_channel, my_uid()).expect("the stores load")
}

/// The mirror's URL for the blob with SHA-256 `hex`.
fn mirror_url(stores: &Stores, hex: &str) -> String {
    let url = stores.get("lan").and_then(|lan| lan.mirror_url(hex));
    url.expect("a mirror").as_str().to_owned()
}

/// Catches a mirror's wrong bytes being used, or passing unreported: the mirror is asked first for
/// the artifact and gives other bytes, the entry's location gives the right ones and wins, the
/// order succeeds, and one `store_digest_mismatch` line names the store `lan`.
#[test]
fn a_mirror_that_gives_other_bytes_is_logged_and_the_locations_right_bytes_win() {
    let world = World::new("stores-mirror");
    let stores = with_mirror(&world);
    let mut routes = world.entry_routes();
    routes.insert(
        0,
        (
            mirror_url(&stores, &world.entry.digest),
            b"other bytes".to_vec(),
        ),
    );
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let log: Log = Arc::new(move |line: &str| sink.lock().expect("the log").push(line.to_owned()));
    let packages = world.open(routes).with_stores(stores.clone()).with_log(log);
    let reply = world.daemon(packages).order(&world.good_install());
    assert_eq!(reply, succeeded(), "{reply:?}");

    let text = bundle(&sha256_hex(&world.archive));
    let asked = world.fetched();
    assert_eq!(asked.len(), 4, "{asked:?}");
    assert_eq!(asked[0], mirror_url(&stores, &world.entry.digest));
    assert_eq!(
        asked[1],
        World::url(&stores, "channel", PKG, &world.entry.digest)
    );
    assert_eq!(asked[2], mirror_url(&stores, &sha256_hex(text.as_bytes())));
    let lines = lines.lock().expect("the log");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("store_digest_mismatch"), "{lines:?}");
    assert!(lines[0].contains("lan"), "{lines:?}");
}

/// Catches a store with no log failing when a mirror gives other bytes: the line goes to standard
/// error and the order still succeeds.
#[test]
fn a_store_with_no_log_of_its_own_still_installs_when_a_mirror_gives_other_bytes() {
    let world = World::new("stores-mirror-stderr");
    let stores = with_mirror(&world);
    let mut routes = world.entry_routes();
    routes.insert(
        0,
        (
            mirror_url(&stores, &world.entry.digest),
            b"other bytes".to_vec(),
        ),
    );
    let packages = world.open(routes).with_stores(stores);
    let reply = world.daemon(packages).order(&world.good_install());
    assert_eq!(reply, succeeded(), "{reply:?}");
}

/// Catches a host without a `stores.toml` getting any other store than the release channel.
#[test]
fn no_stores_file_gives_the_channel_store() {
    let world = World::new("stores-absent");
    let path = world.rig.scratch.path().join("stores.toml");
    let loaded = stores::load(&path, &world.arch.config.release_channel, 0).expect("loads");
    assert_eq!(loaded, Stores::channel(&world.arch.config.release_channel));
    assert_eq!(loaded.all().len(), 1);
    assert_eq!(loaded.all()[0].name(), "channel");
}

/// The host files of a Linux host with `stores` as its stores file, under `root`.
fn files(root: &Path, stores: PathBuf) -> HostFiles {
    HostFiles {
        repositories: None,
        os_root: root.join("os"),
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        stores,
        pki: root.join("pki"),
        console: None,
        managed_config: None,
    }
}

/// Catches a start that goes on past a `stores.toml` it cannot trust: the stores file is
/// writable by others (refused whoever the user is), the start is `StartError::Stores` naming that
/// file, with the config's exit status, before the packages directory is touched.
#[test]
fn an_unsound_stores_file_refuses_the_start_naming_the_file() {
    let rig = Rig::new("stores-start");
    let path = put_stores(rig.scratch.path(), "not a stores file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("its mode");
    let adapters = Adapters::linux(
        &Platform::linux(),
        RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
    );
    let host = files(rig.scratch.path(), path.clone());
    let error = host_ports(&config(), &rig.settings(), &host, OWN, adapters)
        .err()
        .expect("the start is refused");
    assert!(
        matches!(&error, StartError::Stores { path: named, .. } if *named == path),
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

/// Catches a stores file rule that is not enforced at the start, and a refusal that does not name
/// the store and the rule or that carries a credential: an `http://` base, a mirror on an `https`
/// store and a credential file open to others are each refused, the text naming the store and the
/// rule, and the credential's secret nowhere in it.
#[test]
fn a_store_that_breaks_a_rule_is_refused_naming_the_store_and_never_the_credential() {
    let rig = Rig::new("stores-rules");
    let dir = rig.scratch.path();
    let secret = dir.join("lan.token");
    fs::write(&secret, "s3cr3t-token").expect("the credential file");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("its mode");
    let cases = [
        (
            "[[store]]\nname = \"plain\"\nkind = \"https\"\nbase = \"http://x.example/\"\n"
                .to_owned(),
            "plain",
        ),
        (
            "[[store]]\nname = \"lan\"\nkind = \"https\"\nbase = \"https://x.example/\"\nmirror = true\n"
                .to_owned(),
            "lan",
        ),
        (
            format!(
                "[[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://x.example/\"\nbearer_file = \"{}\"\n",
                secret.display()
            ),
            "lan",
        ),
    ];
    let channel = config().release_channel;
    for (text, store) in cases {
        let path = put_stores(dir, &text);
        let source = stores::load(&path, &channel, my_uid()).expect_err("the file is refused");
        let error = StartError::Stores {
            path: path.clone(),
            source,
        };
        let shown = error.to_string();
        assert!(shown.starts_with(&path.display().to_string()), "{shown}");
        assert!(shown.contains(store), "{shown}");
        assert!(!shown.contains("s3cr3t-token"), "{shown}");
        assert!(
            matches!(&error, StartError::Stores { source: StoresError::Store { rule, .. }, .. }
                if !matches!(rule, Rule::Name)),
            "{shown}"
        );
        assert_eq!(error.exit_code(), EXIT_CONFIG);
    }
}

/// Catches the update of hostkeeper itself skipping the entry's checks, or not passing the entry
/// to the same fetch: an entry whose digest is not the order's is `order_invalid` and one of an
/// installer not listed is deferred, with no step run and the package port not asked; a sound
/// entry reaches the port, and the update runs to the restart.
#[test]
fn the_update_of_hostkeeper_itself_goes_through_the_same_entry_checks_and_fetch() {
    use support::selfupdate::{NEW, SelfRig};
    let s = SelfRig::new("stores-self-update");
    let daemon = s.daemon();
    let sound = Target {
        installer: "hostkeeper-archive".to_owned(),
        digest: d(),
        bytes: 1,
        manifest: Some(bare_manifest("0.1.0")),
        ..Target::default()
    };
    let wrong_digest = Target {
        digest: support::digest("cd"),
        ..sound.clone()
    };
    let tarball = Target {
        installer: "tarball".to_owned(),
        ..sound.clone()
    };
    let reply = daemon.order(&with_entry(s.order(NEW, 1), &wrong_digest));
    assert_eq!(reply.refusal(), Some(Refusal::OrderInvalid), "{reply:?}");
    assert_eq!(
        daemon.order(&with_entry(s.order(NEW, 1), &tarball)),
        not_listed()
    );
    assert_eq!(s.rig.store.asked(), Vec::<(String, String)>::new());
    assert_eq!(s.hk_steps(), Vec::<Step>::new());

    let reply = daemon.order(&with_entry(s.order(NEW, 1), &sound));
    assert_eq!(reply, support::selfupdate::awaiting(false), "{reply:?}");
    assert_eq!(s.rig.store.entries(), [Some(sound)]);
}

/// Catches the heartbeat and the receipt check listing different installers, and a start that
/// does not decide them from the host: the host's ports announce what the `os-release` under the
/// files' `os_root` gives (a Debian host: `hostkeeper-archive` alone in this build, its family
/// `deb`; no file: no family), the daemon holds that announcement for every heartbeat of its
/// start, and the receipt check judges an entry against the same list (`apt`, which it does not
/// list, is `installer_not_listed`).
#[test]
fn the_installers_announced_are_the_ones_the_receipt_check_lists() {
    let rig = Rig::new("stores-announce");
    let stores = rig.scratch.path().join("no-stores.toml");
    let host = files(rig.scratch.path(), stores);
    let ports = |host: &HostFiles| {
        let adapters = Adapters::linux(
            &Platform::linux(),
            RunnerLauncher::new(FakeCommandRunner::default(), SYSTEMCTL_DEADLINE),
        );
        fs::create_dir_all(&rig.packages).expect("the packages directory");
        host_ports(&config(), &rig.settings(), host, OWN, adapters).expect("the ports")
    };
    let none = ports(&host).announcement;
    assert_eq!(none.names(), ["hostkeeper-archive"]);
    assert_eq!(none.family, None);
    let etc = host.os_root.join("etc");
    fs::create_dir_all(&etc).expect("etc under the root");
    fs::write(etc.join("os-release"), "ID=debian\n").expect("os-release");
    let debian = ports(&host).announcement;
    assert_eq!(debian.names(), ["hostkeeper-archive"]);
    assert_eq!(debian.family, Some(hostkeeper_install::Family::Deb));

    let world = World::new("stores-announce-daemon");
    let daemon = world.daemon(world.open(world.entry_routes()));
    assert_eq!(daemon.ctx().announcement.names(), ["hostkeeper-archive"]);
    let mut apt = world.entry.clone();
    apt.installer = "apt".to_owned();
    let order = with_entry(install(SERVICE, VERSION, &apt.digest, 1), &apt);
    assert_eq!(daemon.order(&order), not_listed());
    world.untouched();
}

/// Catches a store that stages an entry with the steps of another installer: an entry the
/// receipt check did not judge, of an installer this build has no steps of (`apt`) or of a name
/// that is no installer, is fetched and proven as any entry and then refused
/// `installer_not_listed`, deferred, with no version directory made; the same bytes as a
/// `hostkeeper-archive` entry are staged.
#[test]
fn the_store_stages_only_with_the_steps_of_a_registered_installer() {
    use hostkeeper_daemon::packages::Packages as _;
    use hostkeeper_hook::{Class, Outcome};
    let world = World::new("stores-registry");
    let packages = world.open(world.entry_routes());
    let digest =
        hostkeeper_daemon::verb::PackageDigest::new(&world.entry.digest).expect("a digest");
    let service = hostkeeper_config::names::ServiceName::new(SERVICE).expect("a name");
    let version = hostkeeper_config::names::Version::new(VERSION).expect("a version");
    for installer in ["apt", "not-an-installer"] {
        let mut entry = world.entry.clone();
        entry.installer = installer.to_owned();
        let error = packages
            .version_dir(&service, &version, &digest, None, Some(&entry))
            .expect_err("no steps");
        assert_eq!(
            (error.refusal(), error.outcome(), error.class()),
            (
                Refusal::InstallerNotListed,
                Outcome::Deferred,
                Class::Environment
            ),
            "{installer}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "this daemon has no installer {installer:?} to stage the entry with (installer_not_listed)"
            )
        );
        assert!(!world.rig.packages.join(SERVICE).join(VERSION).exists());
    }
    let dir = packages
        .version_dir(&service, &version, &digest, None, Some(&world.entry))
        .expect("staged");
    assert!(dir.dir().ends_with(format!("{SERVICE}/{VERSION}")));
    assert!(dir.manifest().is_some(), "the archive's manifest is kept");
}
