//! The daemon's unit (`packaging/linux/hostkeeper.service`, which hostkeeper's own package
//! carries), started for real: a runnable program (`tests/bin/exec_fixture.rs`) is packaged as
//! `bin/hostkeeper` with the committed unit, as `cargo xtask release package` builds a package;
//! the daemon's own unpacker (design 5.5, `hostkeeper-unpack`) installs it into a scratch packages
//! directory; the self-update's follow-on stages the package's binary into the binary directory
//! under a scratch state directory and switches `current` to it; and the `ExecStart=` of the
//! unpacked unit, with its `--state-dir` moved onto the scratch directory, is executed. The
//! program reports the path it was started by and the arguments it received, and its exit status
//! says whether it was started as the daemon.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_steps::Layout;
use testdir::TestDir;

/// The packaged program: it prints how it was started.
const FIXTURE: &str = env!("CARGO_BIN_EXE_xtask-exec-fixture");

/// The daemon's one unit, as committed: the deb, the rpm, the overlay and hostkeeper's own
/// package carry it.
const UNIT: &str = "../../packaging/linux/hostkeeper.service";

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
/// `--state-dir` prefix replaced by `state`; or what the unit lacks for that.
fn exec_start(unit: &str, state: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let line = unit
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .ok_or("the unit has no ExecStart=")?;
    let mut words = line.split_whitespace().map(str::to_owned);
    let program = words.next().ok_or("ExecStart= names no program")?;
    let args: Vec<String> = words.collect();
    let dir = args
        .windows(2)
        .find(|w| w[0] == "--state-dir")
        .map(|w| format!("{}/", w[1]))
        .ok_or("ExecStart= has no --state-dir")?;
    let inside = program
        .strip_prefix(&dir)
        .ok_or_else(|| format!("{program} is not under the --state-dir {dir}"))?;
    Ok((state.join(inside), args))
}

/// The package of the running daemon's version over the fixture and the committed unit,
/// installed by the daemon's unpacker into `dir/packages`; that directory, and the version.
fn installed(dir: &Path) -> (PathBuf, String) {
    let version = hostkeeper_unpack::running_hostkeeper().to_string();
    let out = dir.join("out");
    xtask::test_support::linux_package(&version, Path::new(FIXTURE), &repo_file(UNIT), &out, MTIME)
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
        // The package `linux_package` makes is the linux-x86_64 one.
        platform: hostkeeper_config::platform::Platform::LinuxX86_64,
        package_digest: hostkeeper_config::daemon::Sha256Hex::parse(&digest).expect("a digest"),
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

/// Catches a unit whose ExecStart does not start the binary the self-update staged from the
/// package once `current` names its version (the deb's /usr/bin path, the package's own
/// `current/bin`, a path outside --state-dir, a link that does not resolve), or that starts it
/// with other arguments or as anything but the daemon.
#[test]
fn the_committed_unit_starts_the_packaged_program_through_the_binary_directory() {
    let dir = scratch("unit");
    let (packages, version) = installed(&dir);
    let release = packages.join("hostkeeper").join(&version);
    let unit = std::fs::read_to_string(release.join("units/hostkeeper.service"))
        .expect("the unpacked unit");
    assert_eq!(
        unit,
        std::fs::read_to_string(repo_file(UNIT)).expect("the committed unit")
    );
    let state = dir.join("state");
    let (program, args) = exec_start(&unit, &state).expect("the unit's ExecStart");
    let layout = Layout::new(&binary_dir(&state));
    assert_eq!(program, layout.current_path());
    assert_eq!(args.first().map(String::as_str), Some("daemon"));

    // Before `current` names a version, there is nothing to start.
    let missing = run(&program, &args).expect_err("no current yet");
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    layout
        .stage(&version, &release.join("bin/hostkeeper"))
        .expect("staged");
    layout.switch(&version).expect("switched");
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

    // Units without what the start needs, or starting a program outside the state directory,
    // are refused by name.
    for (text, why) in [
        ("[Service]\nType=notify\n", "the unit has no ExecStart="),
        ("[Service]\nExecStart=\n", "ExecStart= names no program"),
        (
            "[Service]\nExecStart=/opt/hostkeeper daemon\n",
            "ExecStart= has no --state-dir",
        ),
        (
            "[Service]\nExecStart=/usr/bin/hostkeeper daemon --state-dir /var/lib/updater/state\n",
            "/usr/bin/hostkeeper is not under the --state-dir /var/lib/updater/state/",
        ),
    ] {
        assert_eq!(exec_start(text, &state), Err(why.to_owned()));
    }
    unseal(&packages);
}
