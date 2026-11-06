//! The hello packages of `tests/device_mac_e2e.rs` checked without root or a Mac, in the
//! pull-request set, so that a device run after the merge is not the first place a drift shows:
//! with the released layout the builder gives the committed goldens byte for byte; with a test's
//! layout its package passes the production unpacker (for `macos-arm64`; the Mac's signature
//! check is the device run's) and loads, and the daemon's placer claims exactly its two plists
//! in the launchd directory through `current`, with no `WantedBy=` link.

mod e2e;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use e2e::hello::{
    GOLDEN_MANIFEST, GOLDEN_PLIST_A, GOLDEN_PLIST_B, GOLDEN_VERSION, HELLO_E2E, HelloLayout,
    PLATFORM, SERVICE, golden_entries,
};
use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform;
use hostkeeper_placed::testing::FakeUnitControl;
use hostkeeper_placed::{PathKind, Placer, RealFs, Roots};
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::{HostFs, Limits, Owner, UnpackOrder, unpack};

/// The unit directory of a Mac.
const LAUNCHD_DIR: &str = "/Library/LaunchDaemons";

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// Makes every directory under `dir` writable by its owner, so that the scratch directory,
/// whose unpacked trees are mode 0555, can be removed.
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

/// The names, modes and bytes of the entries of a package archive, in order.
fn entries(bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
    let mut tar = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(bytes)
        .expect("one zstd frame")
        .read_to_end(&mut tar)
        .expect("the frame decodes");
    let mut archive = tar::Archive::new(&tar[..]);
    archive
        .entries()
        .expect("a tar archive")
        .map(|entry| {
            let mut entry = entry.expect("an entry");
            let name = entry.path().expect("a name").to_string_lossy().into_owned();
            let mode = entry.header().mode().expect("a mode");
            let mut data = Vec::new();
            entry.read_to_end(&mut data).expect("its data");
            (name, mode, data)
        })
        .collect()
}

/// Catches the test's package drifting from the released one: with the released layout the
/// manifest of the golden's version and the plists of both slots are the committed goldens byte
/// for byte, and the archive's entries are those of `entries.txt` in its order and modes, with
/// the program given as `bin/hello`. The failing build differs from the good one only in
/// `--fail-ready` after the port of each plist.
#[test]
fn the_released_layout_gives_the_committed_goldens() {
    let released = HelloLayout::released();
    assert_eq!(released.manifest(GOLDEN_VERSION), GOLDEN_MANIFEST);
    assert_eq!(released.plist("a", false), GOLDEN_PLIST_A);
    assert_eq!(released.plist("b", false), GOLDEN_PLIST_B);
    let failing = released.plist("a", true);
    assert_eq!(
        failing.replacen("\t\t<string>--fail-ready</string>\n", "", 1),
        GOLDEN_PLIST_A
    );

    let package = released.build(GOLDEN_VERSION, false, b"the program");
    assert_eq!(
        package.file_name,
        format!("hello-{GOLDEN_VERSION}-{PLATFORM}.tar.zst")
    );
    let found = entries(&package.bytes);
    let listed: Vec<(u32, String)> = found
        .iter()
        .map(|(name, mode, _)| (*mode, name.clone()))
        .collect();
    assert_eq!(listed, golden_entries());
    let data: BTreeMap<String, Vec<u8>> = found
        .into_iter()
        .map(|(name, _, data)| (name, data))
        .collect();
    assert_eq!(data["bin/hello"], b"the program");
    assert_eq!(data["manifest.toml"], GOLDEN_MANIFEST.as_bytes());
    assert_eq!(data["units/hello.b.plist"], GOLDEN_PLIST_B.as_bytes());
}

/// Catches a package of the device run that the daemon would refuse or place wrongly: with a
/// test's packages, record and log directories and port, the package of `1.0.0` passes the
/// production unpacker for `macos-arm64` (seal of the archive's digest, the program as built),
/// its manifest names the port and the record directory, its plists start the program through
/// `<packages>/hello/current/`, and the placer of a host whose unit directory is launchd's
/// claims exactly the two plists, each a unit linked into `current`, and nothing else.
#[test]
fn a_test_layout_package_unpacks_and_places_its_two_plists() {
    let bench = scratch("hello-rig");
    let root = bench.path();
    let layout = HelloLayout {
        packages: root.join("packages"),
        record_dir: root.join("hooks").join(SERVICE),
        log_dir: root.join("logs"),
        port: e2e::free_port(),
    };
    fs::create_dir_all(&layout.packages).expect("the packages directory");
    let program = fs::read(HELLO_E2E).expect("the built hello-e2e");
    let package = layout.build("1.0.0", false, &program);
    let archive = root.join(&package.file_name);
    fs::write(&archive, &package.bytes).expect("write the archive");

    let owner = Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    };
    let host = HostFs::open(&layout.packages, owner).expect("the packages directory");
    let order = UnpackOrder {
        service: ServiceName::new(SERVICE).expect("a service name"),
        version: Version::new("1.0.0").expect("a version"),
        platform: Platform::MacosArm64,
        package_digest: Sha256Hex::parse(&package.sha256).expect("a digest"),
    };
    let limits = Limits {
        max_unpacked_bytes: 2 << 30,
        max_entries: 20_000,
    };
    let done = unpack(
        &archive,
        &order,
        &limits,
        &host,
        hostkeeper_unpack::Platform::Linux,
    )
    .expect("the production unpacker accepts the package");
    assert_eq!(done.seal.package_digest(), package.sha256);
    assert_eq!(done.manifest.run, ["hello.a", "hello.b"]);
    let dir = layout.packages.join(SERVICE).join("1.0.0");
    assert_eq!(fs::read(dir.join("bin/hello")).expect("bin/hello"), program);
    let manifest_text = fs::read_to_string(dir.join("manifest.toml")).expect("the manifest");
    assert!(manifest_text.contains(&format!("port = {}\n", layout.port)));
    assert!(manifest_text.contains(&format!("record_dir = \"{}\"", layout.record_dir.display())));
    let plist = fs::read_to_string(dir.join("units/hello.b.plist")).expect("a plist");
    let program_path = layout.packages.join("hello/current/bin/hello");
    assert!(plist.contains(&format!("<string>{}</string>", program_path.display())));

    let roots = Roots::new(
        &root.join("state"),
        Path::new(LAUNCHD_DIR),
        &layout.packages,
    );
    let fs = RealFs::new(Path::new("/"));
    let units = FakeUnitControl::new(hostkeeper_placed::testing::Log::new());
    let desired = Placer::new(roots, &fs, &units)
        .desired(&done.manifest)
        .expect("the placer takes the package");
    let claims: Vec<(PathBuf, PathKind, PathBuf)> = desired
        .iter()
        .map(|claim| (claim.path.clone(), claim.kind, claim.link_target.clone()))
        .collect();
    let current = layout.packages.join("hello/current/units");
    assert_eq!(
        claims,
        ["hello.a", "hello.b"].map(|label| (
            Path::new(LAUNCHD_DIR).join(format!("{label}.plist")),
            PathKind::Unit,
            current.join(format!("{label}.plist")),
        ))
    );
    open_up(root);
}
