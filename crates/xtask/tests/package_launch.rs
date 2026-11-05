//! The unit hostkeeper's own package carries (`packaging/linux/package/hostkeeper.service`),
//! started for real: a runnable program (`tests/bin/exec_fixture.rs`) is packaged as
//! `bin/hostkeeper` with the committed unit, as `cargo xtask release package` builds a package;
//! the daemon's own unpacker (design 5.5, `hostkeeper-unpack`) installs it into a scratch packages
//! directory; `current` is linked to its version; and the `ExecStart=` of the unpacked unit,
//! with its `--packages-dir` moved onto the scratch directory, is executed. The program reports
//! the path it was started by and the arguments it received, and its exit status says whether
//! it was started as the daemon.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostkeeper_config::names::{ServiceName, Version};
use testdir::TestDir;

/// The packaged program: it prints how it was started.
const FIXTURE: &str = env!("CARGO_BIN_EXE_xtask-exec-fixture");

/// The unit of hostkeeper's own package, as committed.
const PACKAGE_UNIT: &str = "../../packaging/linux/package/hostkeeper.service";

/// The unit the deb and the rpm install, as committed.
const DEB_UNIT: &str = "../../packaging/linux/hostkeeper.service";

/// A commit time for every entry of the archive.
const MTIME: u64 = 1_800_000_000;

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn scratch(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("package-launch-{name}"),
    )
}

/// The program and the arguments of the `ExecStart=` of `unit`, with the program's
/// `--packages-dir` prefix replaced by `packages`; or what the unit lacks for that.
fn exec_start(unit: &str, packages: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let line = unit
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .ok_or("the unit has no ExecStart=")?;
    let mut words = line.split_whitespace().map(str::to_owned);
    let program = words.next().ok_or("ExecStart= names no program")?;
    let args: Vec<String> = words.collect();
    let dir = args
        .windows(2)
        .find(|w| w[0] == "--packages-dir")
        .map(|w| format!("{}/", w[1]))
        .ok_or("ExecStart= has no --packages-dir")?;
    let inside = program
        .strip_prefix(&dir)
        .ok_or_else(|| format!("{program} is not under the --packages-dir {dir}"))?;
    Ok((packages.join(inside), args))
}

/// The package of the running daemon's version over the fixture and the committed package unit,
/// installed by the daemon's unpacker into `dir/packages`; that directory, and the version.
fn installed(dir: &Path) -> (PathBuf, String) {
    let version = hostkeeper_unpack::running_hostkeeper().to_string();
    let out = dir.join("out");
    xtask::test_support::linux_package(
        &version,
        Path::new(FIXTURE),
        &repo_file(PACKAGE_UNIT),
        &out,
        MTIME,
    )
    .expect("package");
    let archive = out.join(format!("hostkeeper-{version}-linux-x86_64.tar.zst"));
    let bytes = std::fs::read(&archive).expect("the package");
    let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    let packages = dir.join("packages");
    std::fs::create_dir(&packages).expect("the packages directory");
    let meta = std::fs::metadata(&packages).expect("stat the packages directory");
    let owner = hostkeeper_unpack::Owner {
        uid: std::os::unix::fs::MetadataExt::uid(&meta),
        gid: std::os::unix::fs::MetadataExt::gid(&meta),
    };
    let fs = hostkeeper_unpack::HostFs::open(&packages, owner).expect("open the packages");
    let order = hostkeeper_unpack::UnpackOrder {
        service: ServiceName::new("hostkeeper").expect("a service name"),
        version: Version::new(&version).expect("a version"),
        package_digest: hostkeeper_config::daemon::Sha256Hex::parse(&digest).expect("a digest"),
        // The package built above is the linux-x86_64 one.
        platform: hostkeeper_config::platform::Platform::LinuxX86_64,
    };
    let limits = hostkeeper_unpack::Limits {
        max_unpacked_bytes: 1 << 30,
        max_entries: 1000,
    };
    let unpacked = hostkeeper_unpack::unpack(
        &archive,
        &order,
        &limits,
        &fs,
        hostkeeper_unpack::Platform::Linux,
    )
    .expect("unpack");
    assert_eq!(unpacked.manifest.run, ["hostkeeper.service"]);
    (packages, version)
}

/// Runs `program` with `args`.
fn run(program: &Path, args: &[String]) -> std::io::Result<Output> {
    Command::new(program).args(args).output()
}

/// Makes every directory under `dir` writable again, so the scratch directory can remove the
/// tree the unpacker sealed read-only.
fn unseal(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            unseal(&entry.path());
        }
    }
}

/// Catches a package unit whose ExecStart does not start the binary the package carries once the
/// daemon has installed it and `current` names its version (the deb's /usr/bin path, a path
/// outside --packages-dir, a link that does not resolve), or that starts it with other arguments
/// or as anything but the daemon.
#[test]
fn the_committed_package_unit_starts_the_packaged_program_through_current() {
    let dir = scratch("unit");
    let (packages, version) = installed(&dir);
    let service = packages.join("hostkeeper");
    let unit = std::fs::read_to_string(service.join(&version).join("units/hostkeeper.service"))
        .expect("the unpacked unit");
    assert_eq!(
        unit,
        std::fs::read_to_string(repo_file(PACKAGE_UNIT)).expect("the committed unit")
    );
    let (program, args) = exec_start(&unit, &packages).expect("the unit's ExecStart");
    assert_eq!(program, service.join("current/bin/hostkeeper"));
    assert_eq!(args.first().map(String::as_str), Some("daemon"));

    // Before `current` names a version, there is nothing to start.
    let missing = run(&program, &args).expect_err("no current yet");
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    std::os::unix::fs::symlink(&version, service.join("current")).expect("link current");
    let started = run(&program, &args).expect("start the packaged program");
    assert_eq!(started.status.code(), Some(0), "{started:?}");
    let mut want = vec![program.to_string_lossy().into_owned()];
    want.extend(args.iter().cloned());
    assert_eq!(
        String::from_utf8_lossy(&started.stdout),
        format!("{}\n", want.join("\n"))
    );

    // The program says when it is not started as the daemon.
    let other = run(&program, &["serve".to_owned()]).expect("start it otherwise");
    assert_eq!(other.status.code(), Some(64), "{other:?}");

    // The deb's unit starts no packaged program, and units without what the start needs are
    // refused by name.
    let deb = std::fs::read_to_string(repo_file(DEB_UNIT)).expect("the deb's unit");
    let refused = exec_start(&deb, &packages).expect_err("the deb's unit");
    assert!(
        refused.starts_with("/usr/bin/hostkeeper is not under the --packages-dir"),
        "{refused}"
    );
    for (text, why) in [
        ("[Service]\nType=notify\n", "the unit has no ExecStart="),
        ("[Service]\nExecStart=\n", "ExecStart= names no program"),
        (
            "[Service]\nExecStart=/opt/hostkeeper daemon\n",
            "ExecStart= has no --packages-dir",
        ),
    ] {
        assert_eq!(exec_start(text, &packages), Err(why.to_owned()));
    }
    unseal(&packages);
}
