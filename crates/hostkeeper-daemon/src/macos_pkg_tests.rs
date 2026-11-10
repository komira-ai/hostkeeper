//! The daemon's side of `macos-pkg` that needs no daemon: the record of a `.pkg` service in
//! `placed.json`, a daemon that holds no `macos-pkg` installer, and the order of a removal.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_install::host::Ids;
use hostkeeper_install::repository::Records as _;
use hostkeeper_install::tools::{ToolError, ToolOutput};
use hostkeeper_placed::Roots;
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs, Log};
use hostkeeper_proto::hostkeeper::release::v1::Manifest;
use hostkeeper_testkit::Scratch;

use super::*;
use crate::testing::{FakeClock, FakePackages, ScriptedLauncher};

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn hello() -> ServiceName {
    ServiceName::new("hello").expect("a name")
}

/// Tools that run nothing: these tests never reach a tool.
struct NoTools;

impl Tools for NoTools {
    fn run(
        &self,
        tool: hostkeeper_install::Tool,
        _args: &[String],
        _deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        panic!("no tool runs here, not even {}", tool.path())
    }
}

/// A repository installer on a scratch host, with the `macos-pkg` ports `pkg`.
fn repository(scratch: &Scratch, pkg: Option<MacosPkgPorts>) -> Repository {
    repository_and_store(scratch, pkg).0
}

/// [`repository`], and its package store.
fn repository_and_store(
    scratch: &Scratch,
    pkg: Option<MacosPkgPorts>,
) -> (Repository, Arc<FakePackages>) {
    let packages = scratch.path().join("packages");
    fs::create_dir_all(&packages).expect("packages");
    fs::create_dir_all(scratch.path().join("placed")).expect("placed");
    let ports = crate::testing::repository_ports(&packages, scratch.path(), Default::default());
    let ports = RepositoryPorts::clone(&ports).with_macos_pkg(pkg);
    let store = FakePackages::new(&packages);
    let repository = Repository {
        launcher: ScriptedLauncher::new(),
        ports: Arc::new(ports),
        packages: store.clone(),
        clock: FakeClock::new(),
        roots: Roots::new(
            Path::new("/placed"),
            Path::new("/units"),
            Path::new("/packages"),
        ),
        fs: Arc::new(FaultFs::new(scratch.path(), Log::new())),
        units: Arc::new(FakeUnitControl::new(Log::new())),
        max_package_bytes: 1 << 20,
        repaired: Arc::default(),
        tool_runs: Arc::default(),
        log: Arc::new(|_: &str| {}),
    };
    (repository, store)
}

use crate::repository::RepositoryPorts;

/// The ports of a Mac whose root is `root`, with both tools and one signer.
fn mac_ports(root: &Path) -> MacosPkgPorts {
    for tool in ["usr/sbin/installer", "usr/sbin/pkgutil"] {
        let path = root.join(tool);
        fs::create_dir_all(path.parent().expect("a parent")).expect("its directory");
        fs::write(&path, b"").expect("the tool");
    }
    let signers = Signers::new(vec![
        hostkeeper_install::macos_pkg::signers::Fingerprint::parse(&"3d".repeat(32)).expect("hex"),
    ])
    .expect("one signer");
    let ids = Ids {
        root: 0,
        wheel: 0,
        admin: 80,
    };
    ports(
        true,
        Host::new(root, ids),
        Some(signers),
        Some(Arc::new(NoTools)),
    )
    .expect("announced")
}

/// A `macos-pkg` entry of hello with its manifest.
fn entry() -> Target {
    Target {
        installer: "macos-pkg".to_owned(),
        digest: "ab".repeat(32),
        bytes: 1,
        manifest: Some(Manifest {
            package: Some("dev.hostkeeper.hello".to_owned()),
            installer_bounds: Some(Default::default()),
            ..Manifest::default()
        }),
        ..Target::default()
    }
}

/// Catches: a `.pkg` service's record written to other keys of its `installed` entry than the
/// identifier as its package, its version, its created directories and `pending`, read back as
/// another, read from an entry of another installer, or not found as the holder of its
/// identifier by another service. A save keeps the keys of a newer writer and drops an
/// earlier repository's.
#[test]
fn a_pkg_record_is_the_services_installed_entry() {
    let scratch = scratch("pkg-record");
    let repository = repository(&scratch, None);
    let records = repository.records();
    let mut record = PkgRecord::new("dev.hostkeeper.hello", "1.0.0");
    record.created_dirs = ["/Library/Application Support/hello".to_owned()].into();
    record.pending = true;
    assert_eq!(Records::load(&records, &hello()).expect("reads"), None);
    Records::save(&records, &hello(), &record).expect("saved");
    assert_eq!(
        Records::load(&records, &hello()).expect("reads"),
        Some(record.clone())
    );
    let placed = records.placer().store().load().expect("placed.json reads");
    let entry = &placed.installed["hello"];
    assert_eq!(entry.installer, "macos-pkg");
    assert_eq!(entry.package.as_deref(), Some("dev.hostkeeper.hello"));
    assert_eq!(entry.version.as_deref(), Some("1.0.0"));
    assert_eq!(entry.repository, None);
    assert!(entry.pending);
    assert_eq!(entry.created_dirs, record.created_dirs);

    let other = ServiceName::new("other").expect("a name");
    assert_eq!(
        Records::holder(&records, "dev.hostkeeper.hello", &other).expect("reads"),
        Some(hello())
    );
    assert_eq!(
        Records::holder(&records, "dev.hostkeeper.hello", &hello()).expect("reads"),
        None
    );

    records
        .change(|installed| {
            let apt = installed.get_mut("hello").expect("the entry");
            apt.installer = "apt".to_owned();
            apt.repository = Some("main".to_owned());
        })
        .expect("changed");
    assert_eq!(
        Records::load(&records, &hello()).expect("reads"),
        None,
        "an apt entry is no .pkg record"
    );
    record.pending = false;
    Records::save(&records, &hello(), &record).expect("saved");
    let placed = records.placer().store().load().expect("placed.json reads");
    assert_eq!(placed.installed["hello"].repository, None);
    assert_eq!(placed.installed["hello"].installer, "macos-pkg");
    records.clear(&hello()).expect("cleared");
    let mut nameless = InstalledEntry::new("macos-pkg");
    nameless.version = Some("1.0.0".to_owned());
    assert_eq!(pkg_record(&nameless), None, "an entry with no identifier");
}

/// Catches: a daemon with no `macos-pkg` installer (no Mac, or no signers file) that runs a
/// `macos-pkg` step anyway, or fails one as other than `io_error` with [`NO_MACOS_PKG`]. Its
/// stage, activate, removal and drift are each that failure, and nothing is recorded.
#[test]
fn a_daemon_without_the_installer_runs_no_pkg_step() {
    let scratch = scratch("pkg-no-ports");
    let (repository, store) = repository_and_store(&scratch, None);
    let version = Version::new("1.0.0").expect("a version");
    let none = Failure::new(Reason::IoError, NO_MACOS_PKG);
    let dir = scratch.path().join("packages/hello/1.0.0");
    assert_eq!(
        repository.activate_pkg(&hello(), &version, &dir, &entry()),
        Err(none.clone())
    );
    assert_eq!(repository.drift_pkg(&hello(), &entry()), Err(none.clone()));
    assert_eq!(
        repository.stage_pkg(&hello(), &version, &entry(), &|_| panic!(
            "nothing is fetched"
        )),
        Err(crate::repository::finished(&none))
    );
    assert!(store.asked().is_empty(), "nothing is fetched");
    Records::save(
        &repository.records(),
        &hello(),
        &PkgRecord::new("dev.hostkeeper.hello", "1.0.0"),
    )
    .expect("saved");
    assert_eq!(repository.remove_pkg(&hello(), &entry()), Err(none));
    assert!(
        Records::load(&repository.records(), &hello())
            .expect("reads")
            .is_some(),
        "the record stays"
    );
}

/// Catches: a removal that runs with no record (nothing of the package is left: the record is
/// dropped last), or that cannot run for an entry with no manifest (its `current` is gone). The
/// first removes nothing and runs no tool; the second's order is the record's identifier and
/// version, with no job and the bound a removal falls back to.
#[test]
fn a_removal_takes_its_order_from_the_record() {
    let scratch = scratch("pkg-removal-order");
    let root = scratch.path().join("mac");
    fs::create_dir(&root).expect("the Mac's root");
    let repository = repository(&scratch, Some(mac_ports(&root)));
    assert_eq!(repository.remove_pkg(&hello(), &entry()), Ok(()));
    assert_eq!(repository.removal_order(&hello(), &entry()), Ok(None));

    let mut record = PkgRecord::new("dev.hostkeeper.hello", "1.2.0");
    record.pending = true;
    Records::save(&repository.records(), &hello(), &record).expect("saved");
    let bare = Target {
        installer: "macos-pkg".to_owned(),
        ..Target::default()
    };
    let order = repository
        .removal_order(&hello(), &bare)
        .expect("an order")
        .expect("one");
    assert_eq!(order.identifier, "dev.hostkeeper.hello");
    assert_eq!(order.version.as_str(), "1.2.0");
    assert!(order.run.is_empty());
    assert_eq!(
        order.bounds.remove,
        Duration::from_secs(REMOVE_SECS_WITHOUT_BOUNDS)
    );
    assert_eq!(order.hook, None);
    let named = repository
        .removal_order(&hello(), &entry())
        .expect("an order")
        .expect("one");
    assert_eq!(named.version.as_str(), "1.2.0", "the record's version");
}

/// Catches: an `activate` given as `from` a version `current` names that is not a `.pkg` (an
/// archive version, the move), or the version itself; or a `remove` that walks a version that
/// is no `.pkg`. Only a `macos-pkg` version is replaced or kept.
#[test]
fn only_a_pkg_version_is_replaced_or_kept() {
    let scratch = scratch("pkg-replaced");
    let repository = repository(&scratch, None);
    let packages = scratch.path().join("packages/hello");
    for (version, installer) in [("1.0.0", None), ("1.1.0", Some("macos-pkg"))] {
        let dir = packages.join(version);
        fs::create_dir_all(&dir).expect("a version");
        if let Some(installer) = installer {
            let mut sealed = entry();
            sealed.installer = installer.to_owned();
            target::write(&dir, &sealed).expect("its target.json");
        }
    }
    let link = |name: &str, version: &str| {
        let path = packages.join(name);
        let _ = fs::remove_file(&path);
        std::os::unix::fs::symlink(version, &path).expect("the link");
    };
    let v = |text: &str| Version::new(text).expect("a version");
    assert_eq!(
        repository.replaced(&hello(), &v("1.2.0")),
        Ok(None),
        "no current"
    );
    link("current", "1.0.0");
    assert_eq!(
        repository.replaced(&hello(), &v("1.2.0")),
        Ok(None),
        "an archive version"
    );
    link("current", "1.1.0");
    assert_eq!(
        repository.replaced(&hello(), &v("1.1.0")),
        Ok(None),
        "the version itself"
    );
    assert_eq!(
        repository.replaced(&hello(), &v("1.2.0")),
        Ok(Some(packages.join("1.1.0")))
    );
    link("previous", "1.0.0");
    assert_eq!(repository.kept(&hello()), Ok(vec![packages.join("1.1.0")]));
    link("previous", "1.1.0");
    assert_eq!(
        repository.kept(&hello()),
        Ok(vec![packages.join("1.1.0")]),
        "each once"
    );
    fs::write(packages.join("1.0.0/target.json"), b"not json").expect("a bad target.json");
    link("current", "1.0.0");
    assert!(repository.replaced(&hello(), &v("1.2.0")).is_err());
}

/// Catches: the ports of a Mac said in a log line with what they hold beyond the host and the
/// signers (the tools, what the observer saw).
#[test]
fn the_ports_say_their_host_and_signers() {
    let scratch = scratch("pkg-debug");
    let root = scratch.path().join("mac");
    fs::create_dir(&root).expect("the Mac's root");
    let said = format!("{:?}", mac_ports(&root));
    assert!(said.starts_with("MacosPkgPorts { host: "), "{said}");
    assert!(said.contains("signers: Signers("), "{said}");
    assert!(said.ends_with(", .. }"), "{said}");
}

/// Catches: a failed `activate` of hostkeeper's own `.pkg` whose pending record is never
/// written whole again (the drift the heartbeat reports would stay hidden), or one written back
/// with its version, identifier or created directories lost. `unpend` clears `pending` alone,
/// once; an entry not pending, and a service with none, are left as they are.
#[test]
fn unpend_clears_a_pkg_records_pending_alone() {
    let scratch = scratch("pkg-unpend");
    let repository = repository(&scratch, None);
    let mut record = PkgRecord::new("dev.hostkeeper.hostkeeper", "0.1.230");
    record.created_dirs = ["/Library/Application Support/hostkeeper".to_owned()].into();
    record.pending = true;
    Records::save(&repository.records(), &hello(), &record).expect("saved");
    assert_eq!(repository.unpend(&hello()), Ok(true));
    record.pending = false;
    assert_eq!(
        Records::load(&repository.records(), &hello()).expect("reads"),
        Some(record)
    );
    assert_eq!(
        repository.unpend(&hello()),
        Ok(false),
        "not pending any more"
    );
    let absent = ServiceName::new("absent").expect("a name");
    assert_eq!(repository.unpend(&absent), Ok(false));
    let placed = repository.records().placer().store().load().expect("reads");
    assert!(!placed.installed.contains_key("absent"), "nothing is made");
}

/// Catches: a `.pkg` entry whose digest is no SHA-256 fetched or staged, or refused as other than
/// `order_invalid`. It ends `Failed` before the commit, `release`, `order_invalid`, with nothing
/// asked of the store.
#[test]
fn a_pkg_entry_whose_digest_is_no_sha256_is_order_invalid() {
    let scratch = scratch("pkg-bad-digest");
    let root = scratch.path().join("mac");
    fs::create_dir(&root).expect("the Mac's root");
    let (repository, store) = repository_and_store(&scratch, Some(mac_ports(&root)));
    let bad = Target {
        digest: "zz".to_owned(),
        ..entry()
    };
    let version = Version::new("1.0.0").expect("a version");
    let failure = Failure::new(Reason::OrderInvalid, "the entry's digest is no SHA-256");
    assert_eq!(
        repository.stage_pkg(&hello(), &version, &bad, &|_| panic!("nothing is fetched")),
        Err(crate::repository::finished(&failure))
    );
    assert!(store.asked().is_empty());
}

/// Catches: a `placed.json` that cannot be read taken as a service with no record (a `.pkg`
/// staged with no conflict check, a removal that removes nothing), or a save that reports
/// success. Each of the record store's three calls is the store's error.
#[test]
fn a_pkg_record_store_that_cannot_read_placed_json_fails_each_call() {
    let scratch = scratch("pkg-record-faults");
    let mut repository = repository(&scratch, None);
    repository.fs = Arc::new(FaultFs::new(scratch.path(), Log::new()).failing_read(1, true));
    let records = repository.records();
    let other = ServiceName::new("other").expect("a name");
    assert!(Records::load(&records, &hello()).is_err());
    assert!(Records::holder(&records, "dev.hostkeeper.hello", &other).is_err());
    assert!(
        Records::save(
            &records,
            &hello(),
            &PkgRecord::new("dev.hostkeeper.hello", "1.0.0")
        )
        .is_err()
    );
}

/// Catches: a removal handed a kept version that is not on the host. A `previous` that names a
/// version whose directory is gone is no kept version.
#[test]
fn a_kept_link_to_a_version_gone_is_not_kept() {
    let scratch = scratch("pkg-kept-gone");
    let repository = repository(&scratch, None);
    let packages = scratch.path().join("packages/hello");
    fs::create_dir_all(&packages).expect("the service's directory");
    std::os::unix::fs::symlink("0.9.0", packages.join("previous")).expect("the link");
    assert_eq!(repository.kept(&hello()), Ok(Vec::new()));
}
