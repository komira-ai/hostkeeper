//! The parts of the end-to-end rig of `tests/tierstore_shaped.rs` that need no root, run in the
//! pull-request set so that a privileged run after the merge is not the first place a drift
//! shows: the fixture package is a `.deb` whose tree the daemon's path rule and hook check accept
//! and whose entry the installer's own reading of an entry takes, and a port the rig holds for
//! the fixture's instances is refused to other sockets and served by the fixture.

mod e2e;

use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::{Child, Command};
use std::{io, io::Write};

use e2e::aptrepo::{self, File};
use e2e::package::{FixtureVersion, PackageBuilder};
use e2e::{
    FIXTURE_BINARY, HeldPort, REPOSITORY_NAME, SERVICE, UNIT_TEMPLATE, instance, served_version,
    wait_for,
};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::repository::Order;
use hostkeeper_install::repository::apt::Apt;
use hostkeeper_install::repository::tree::{self, Rules, TreeError};
use hostkeeper_testkit::Scratch;

/// A scratch directory under cargo's directory for tests.
fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir")
}

/// The files of `deb`'s `.deb` extracted by `dpkg-deb -x` into `into`, as the user running the
/// test.
fn extracted(deb: &Path, into: &Path) {
    fs::create_dir(into).expect("the directory to extract into");
    let status = Command::new("dpkg-deb")
        .arg("-x")
        .arg(deb)
        .arg(into)
        .status()
        .expect("run dpkg-deb");
    assert!(status.success(), "dpkg-deb -x failed: {status}");
}

/// `PackageBuilder`'s `.deb` has the tree and the entry the daemon accepts: the installer's reading
/// of its entry (`Order::from_entry`) gives the package, its pin, the repository, the two run
/// units and the params; the tree extracted from the `.deb` passes the path rule, has every run
/// unit as a file of the package and has the hook at its fixed path (`tree::check` is the check
/// `stage` makes of the extracted tree); the tree holds the fixture program as the run units'
/// program and as the hook, and the release file of the version with the two instances on the
/// version's ports. The same package with a file outside the paths a service's package may use is
/// refused. Fails if the fixture drifts from what the daemon accepts.
#[test]
fn fixture_package_builds_the_deb_shape_the_daemon_accepts() {
    let bench = scratch("fixture-package");
    let (records, trees, pool) = (
        bench.path().join("hooks"),
        bench.path().join("trees"),
        bench.path().join("pool"),
    );
    for dir in [&records, &trees, &pool] {
        fs::create_dir_all(dir).expect("a directory of the bench");
    }
    let fixture = FixtureVersion {
        version: "1.0.0",
        ports: [e2e::free_port(), e2e::free_port()],
        fail_verify: false,
    };
    let builder = PackageBuilder::new(&records, Path::new(FIXTURE_BINARY));
    let deb = builder.deb(&fixture);
    let (published, _) = aptrepo::build(&deb, &trees, &pool);
    let entry = builder.entry(&fixture, &published);
    assert_eq!(
        fixture.ports.map(instance),
        [
            format!("{SERVICE}@{}.service", fixture.ports[0]),
            format!("{SERVICE}@{}.service", fixture.ports[1]),
        ]
    );

    let order = Order::from_entry(
        &Apt,
        &ServiceName::new(SERVICE).expect("a service name"),
        &Version::new("1.0.0").expect("a version"),
        &entry,
        1 << 20,
    )
    .expect("the installer reads the entry");
    assert_eq!(order.package, SERVICE);
    assert_eq!(order.package_version, "1.0.0");
    assert_eq!(order.repository, REPOSITORY_NAME);
    assert_eq!(order.pin, published.sha256);
    assert_eq!(order.run, fixture.ports.map(instance));
    assert!(order.params, "the entry has the params the hook reads");

    let tree_dir = bench.path().join("extracted");
    extracted(&published.file, &tree_dir);
    let rules = Rules {
        service: SERVICE,
        package: SERVICE,
        run: &order.run,
        boot: order.boot.as_deref(),
        params: order.params,
    };
    assert_eq!(
        tree::check(&tree_dir, &rules),
        Ok(true),
        "the extracted tree passes the checks of stage, with its hook"
    );
    let program = fs::read(FIXTURE_BINARY).expect("the built fixture program");
    assert_eq!(
        fs::read(tree_dir.join("usr/bin/tierstore-like")).expect("the program"),
        program
    );
    assert_eq!(
        fs::read(tree_dir.join(tree::hook_path(SERVICE))).expect("the hook"),
        program
    );
    let release =
        fs::read_to_string(tree_dir.join("usr/share/hostkeeper-ci-tierstore/release.toml"))
            .expect("the release file");
    assert!(release.contains("version = \"1.0.0\"\n"), "{release}");
    for unit in &order.run {
        assert!(release.contains(unit.as_str()), "{release}");
    }
    assert!(
        tree_dir
            .join("usr/lib/systemd/system")
            .join(UNIT_TEMPLATE)
            .is_file()
    );

    let mut stray = deb.clone();
    stray
        .files
        .push(File::new("etc/cron.d/fixture", "x", 0o644));
    let (published, _) = aptrepo::build(&stray, &trees, &pool);
    let stray_dir = bench.path().join("stray");
    extracted(&published.file, &stray_dir);
    assert_eq!(
        tree::check(&stray_dir, &rules),
        Err(TreeError::Outside("etc/cron.d".to_owned())),
        "a file outside the paths of a service's package is refused"
    );
}

/// A [`HeldPort`] is the scenario's port of a fixture instance, held for the whole test: while it
/// is held, a socket that does not share the address (no `SO_REUSEADDR`) is refused the port with
/// `AddrInUse`, so the port is no one else's, and nothing answers on it; the fixture's own
/// `serve`, run on a release file of version `1.0.0`, takes the port and serves its version, and
/// once stopped gives it back and takes it again when started anew, as a unit of it does at a
/// cutover and a rollback. Fails if the scenario's ports are only chosen and let go (a listener
/// the coordinator binds to port 0 may then be given one, and the instance on it fails to start
/// with `Address already in use`), or if holding a port keeps the fixture from serving on it.
#[test]
fn a_held_port_is_refused_to_other_sockets_and_served_by_the_fixture() {
    let bench = scratch("held-port");
    let held = HeldPort::new();
    let port = held.port();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    let unshared = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
        .expect("a TCP socket");
    let refused = unshared
        .bind(&address.into())
        .expect_err("a held port is refused to a socket that does not share it");
    assert_eq!(refused.kind(), io::ErrorKind::AddrInUse, "{refused}");
    assert_eq!(served_version(port), None, "nothing answers on a held port");

    let release = bench.path().join("release.toml");
    fs::File::create(&release)
        .and_then(|mut file| {
            write!(
                file,
                "version = \"1.0.0\"\nrun = [\"{}\"]\n",
                instance(port)
            )
        })
        .expect("the release file");

    for start in ["first", "second"] {
        let serving = Serving(
            Command::new(FIXTURE_BINARY)
                .arg("serve")
                .arg(port.to_string())
                .arg(&release)
                .spawn()
                .expect("the fixture's serve starts"),
        );
        wait_for(
            &format!("the {start} serve on the held port answers 1.0.0"),
            40,
            || served_version(port).as_deref() == Some("1.0.0"),
        );
        drop(serving);
        assert_eq!(
            served_version(port),
            None,
            "a stopped serve leaves the held port answering nothing"
        );
    }
}

/// A `serve` of the fixture, killed and reaped when dropped, so its listener is closed when the
/// drop returns.
struct Serving(Child);

impl Drop for Serving {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}
