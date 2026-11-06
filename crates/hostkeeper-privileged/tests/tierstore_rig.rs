//! The parts of the end-to-end rig of `tests/tierstore_shaped.rs` that need no root, run in the
//! pull-request set so that a privileged run after the merge is not the first place a drift
//! shows: the fixture package has the archive shape the production unpacker accepts and a
//! manifest the production loader loads, and the release channel logs every request and refuses
//! connections once stopped (the witness the force test's "no fetch" depends on).

mod e2e;

use std::fs;
use std::io::Read;
use std::path::Path;

use e2e::channel::{ReleaseChannel, archive_path};
use e2e::package::{FixtureVersion, PackageBuilder};
use e2e::{FIXTURE_BINARY, SERVICE, instance};
use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform;
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::{HostFs, Limits, Owner, UnpackOrder, unpack};

/// A scratch directory under cargo's directory for tests; unpacked trees are mode 0555, so it is
/// opened up before it is removed.
struct Bench(Scratch);

impl Bench {
    fn new(name: &str) -> Bench {
        Bench(Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        open_up(self.0.path());
    }
}

/// Makes every directory under `dir` writable by its owner, so the scratch can be removed.
fn open_up(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    drop(fs::set_permissions(dir, fs::Permissions::from_mode(0o700)));
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            open_up(&entry.path());
        }
    }
}

/// The unpack of `archive` as the daemon does it, as the current user, into `packages`.
fn unpacked(
    archive: &Path,
    version: &str,
    sha256: &str,
    packages: &Path,
) -> Result<hostkeeper_unpack::Unpacked, hostkeeper_unpack::UnpackError> {
    let owner = Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    };
    let fs = HostFs::open(packages, owner).expect("the packages directory");
    let order = UnpackOrder {
        service: ServiceName::new(SERVICE).expect("a service name"),
        version: Version::new(version).expect("a version"),
        platform: Platform::current(),
        package_digest: Sha256Hex::parse(sha256).expect("a digest"),
    };
    let limits = Limits {
        max_unpacked_bytes: 2 << 30,
        max_entries: 20_000,
    };
    unpack(
        archive,
        &order,
        &limits,
        &fs,
        hostkeeper_unpack::Platform::Linux,
    )
}

/// `PackageBuilder`'s archive passes the production unpacker's bounds and shape checks and the
/// production manifest loader: the version directory has the fixture program, the manifest and
/// the unit template; the manifest names the two instances on the version's ports and the
/// fixture's hook; the seal names the archive's digest. The same builder with a link entry added
/// is refused `bad_archive`. Fails if the fixture drifts from what the daemon accepts.
#[test]
fn fixture_package_builds_the_archive_shape_the_unpacker_accepts() {
    let bench = Bench::new("fixture-package");
    let packages = bench.path().join("packages");
    let records = bench.path().join("hooks");
    let built = bench.path().join("built");
    for dir in [&packages, &records, &built] {
        fs::create_dir_all(dir).expect("a directory of the bench");
    }
    let fixture = FixtureVersion {
        version: "1.0.0",
        ports: [e2e::free_port(), e2e::free_port()],
        fail_verify: false,
    };
    let builder = PackageBuilder::new(&packages, &records, Path::new(FIXTURE_BINARY));
    let package = builder.build(&fixture, &built);
    assert_eq!(
        package.file_name,
        format!("{SERVICE}-1.0.0-{}.tar.zst", e2e::platform())
    );
    assert_eq!(package.run_units, fixture.ports.map(instance));

    let done = unpacked(&package.path, "1.0.0", &package.sha256, &packages)
        .expect("the production unpacker accepts the fixture package");
    assert_eq!(done.seal.package_digest(), package.sha256);
    assert_eq!(done.manifest.run.len(), 2);
    let dir = packages.join(SERVICE).join("1.0.0");
    assert_eq!(
        fs::read(dir.join("bin/tierstore-like")).expect("the fixture program"),
        fs::read(FIXTURE_BINARY).expect("the built fixture program")
    );
    assert!(dir.join("manifest.toml").is_file());
    assert!(dir.join("units").join(e2e::UNIT_TEMPLATE).is_file());

    let linked = builder
        .clone()
        .with_link("bin/elsewhere", "/usr/bin/true")
        .build(
            &FixtureVersion {
                version: "1.0.1",
                ..fixture
            },
            &built,
        );
    let refused = unpacked(&linked.path, "1.0.1", &linked.sha256, &packages)
        .expect_err("a link entry is refused");
    assert_eq!(refused.outcome().2, "bad_archive", "{refused}");
}

/// The release channel logs every request it answers, a missing path too, serves what was
/// published, refuses connections once stopped, and serves the same content with the same log
/// on a new port after `start_again`. The force test's "no fetch" reads this log and this
/// refusal. Fails if the log misses a request or a stopped channel still answers.
#[test]
fn release_channel_logs_every_request_and_refuses_after_stop() {
    let mut channel = ReleaseChannel::start();
    let file = format!("{SERVICE}-1.0.0-{}.tar.zst", e2e::platform());
    channel.publish(
        SERVICE,
        "1.0.0",
        &file,
        b"archive bytes".to_vec(),
        b"{}".to_vec(),
    );
    let index = format!("/{SERVICE}/1.0.0/index.json");
    let archive = archive_path(SERVICE, "1.0.0", &file);
    let missing = format!("/{SERVICE}/9.9.9/index.json");

    let agent = client(&channel);
    let get = |agent: &ureq::Agent, channel: &ReleaseChannel, path: &str| {
        let mut response = agent
            .get(format!("{}{path}", channel.base_url()))
            .call()
            .expect("an answer");
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .expect("a body");
        (status, body)
    };
    assert_eq!(
        get(&agent, &channel, &archive),
        (200, b"archive bytes".to_vec())
    );
    assert_eq!(get(&agent, &channel, &archive).0, 200);
    let (status, body) = get(&agent, &channel, &index);
    assert_eq!(status, 200);
    let index_json: serde_json::Value = serde_json::from_slice(&body).expect("the index is JSON");
    assert_eq!(index_json["packages"][0]["file"], file.as_str());
    assert_eq!(index_json["packages"][0]["bytes"], 13);
    assert_eq!(get(&agent, &channel, &missing).0, 404);
    assert_eq!(channel.count(&archive), 2);
    assert_eq!(channel.count(&index), 1);
    assert_eq!(channel.count(&missing), 1);
    assert_eq!(channel.total(), 4);

    let old_port = channel.port();
    channel.stop();
    assert!(
        !channel.reachable(),
        "a stopped channel refuses connections"
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", old_port)).is_err(),
        "nothing listens on the old port"
    );
    assert_eq!(channel.total(), 4, "a stopped channel logs nothing");

    channel.start_again();
    assert!(channel.reachable());
    let agent = client(&channel);
    assert_eq!(get(&agent, &channel, &archive).0, 200);
    assert_eq!(
        channel.count(&archive),
        3,
        "the log goes on across a restart"
    );
}

/// A ureq client that trusts only the channel's CA, as the daemon's fetch does.
fn client(channel: &ReleaseChannel) -> ureq::Agent {
    use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};
    let roots = vec![Certificate::from_der(&channel.ca_der()).to_owned()];
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::new_with_certs(&roots))
        .build();
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .tls_config(tls)
        .build();
    ureq::Agent::new_with_config(config)
}
