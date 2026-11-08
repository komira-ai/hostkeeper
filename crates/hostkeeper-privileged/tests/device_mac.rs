#![cfg(all(target_os = "macos", feature = "privileged"))]
//! The device run on a Mac (design section 5.9): the launchd adapters, the process probe, the
//! boot id, the enrollment claims read from `ioreg` and the package check against the real launchd, kernel and tools of a GitHub-hosted macOS runner, as
//! root, in `.github/workflows/privileged.yml`'s job `macos-device` (and pr-device.yml's copy of it). Like the Linux
//! privileged tests these are not counted by the coverage gate; what they drive is measured
//! against fakes in the pull-request set.
//!
//! Each test works in a scratch directory of its own, names its launchd jobs
//! `dev.hostkeeper.ci-<test>-<pid>` (the step job of the launcher test is named by
//! `Label::from_unit`, `dev.hostkeeper.hostkeeper.step.hkci.prepare.<pid>`), and boots them out
//! when it ends, failed or not. The daemon's coordinator is the loopback address, where nothing
//! answers: no test leaves the host. Waits poll a state with a bounded number of reads; no test
//! asserts how long anything took. What a test finds about the system (the `proc_pidpath` table,
//! the verdicts of `codesign`) it prints, for the record of the run.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use hostkeeper_daemon::boot::BOOT_ID_FILE;
use hostkeeper_daemon::mac::{HostIoreg, HostMacVerify};
use hostkeeper_enroll::{Claims, ClaimsSource, MacClaims};
use hostkeeper_hook::{Invocation, Outcome, Step};
use hostkeeper_launchd::launchctl::{JobStatus, bootout_args, bootstrap_args, print};
use hostkeeper_launchd::procid::darwin::DarwinPidInfo;
use hostkeeper_launchd::{
    Label, Launchctl, LaunchctlProcess, LaunchdLauncher, LaunchdProcessProbe,
    LaunchdServiceManager, PidInfo, StdFileIdentity, SysctlBootId, parse_boot_session_uuid,
};
use hostkeeper_observe::{ActiveState, ProcessProbe, ServiceManager, UnitState};
use hostkeeper_privileged::{Mode, Rig};
use hostkeeper_proto::hostkeeper::local::v1::StatusRequest;
use hostkeeper_proto::hostkeeper::local::v1::admin_client::AdminClient;
use hostkeeper_steps::{
    ArtifactError, BootId, BootIdSource, Bound, Staged, StepCall, StepLauncher,
};
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::{MacVerify, SignatureVerdict};
use hyper_util::rt::TokioIo;
use tonic::transport::{Endpoint, Uri};

const FIXTURE: &str = env!("CARGO_BIN_EXE_hostkeeper-fixture-hook");
const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-device-daemon");
/// How many times a wait reads a state before it gives up, and the pause between two reads.
const READS: u32 = 300;
const PAUSE: Duration = Duration::from_millis(200);

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// Refuses to run as anything but root: the system domain of launchd is root's.
fn as_root() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "the device tests run as root (privileged.yml's cargo runner `sudo`)"
    );
}

fn launchctl() -> LaunchctlProcess {
    LaunchctlProcess::system(Duration::from_secs(60))
}

/// The label of a job of the test `test`.
fn label(test: &str) -> Label {
    Label::parse(&format!("dev.hostkeeper.ci-{test}-{}", std::process::id())).expect("a label")
}

/// A loaded job, booted out when dropped.
struct Booted(Label);

impl Drop for Booted {
    fn drop(&mut self) {
        let _ = launchctl().run(&bootout_args(&self.0));
    }
}

/// `text` for the inside of an XML element.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A job plist: `program` with `args`, started at load, never kept alive.
fn job_plist(label: &Label, argv: &[&str], dir: &Path) -> String {
    let args: String = argv
        .iter()
        .map(|arg| format!("    <string>{}</string>\n", xml(arg)))
        .collect();
    let out = xml(&dir.join("stdout").display().to_string());
    let err = xml(&dir.join("stderr").display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n  \
         <key>KeepAlive</key>\n  <false/>\n  <key>Label</key>\n  <string>{label}</string>\n  \
         <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  <key>RunAtLoad</key>\n  \
         <true/>\n  <key>StandardErrorPath</key>\n  <string>{err}</string>\n  \
         <key>StandardOutPath</key>\n  <string>{out}</string>\n</dict>\n</plist>\n"
    )
}

/// Writes the plist of `label` running `argv` under `dir` and bootstraps it in the system domain.
fn bootstrap(label: &Label, argv: &[&str], dir: &Path) -> Booted {
    let plist = dir.join(format!("{label}.plist"));
    fs::write(&plist, job_plist(label, argv, dir)).expect("the plist");
    fs::set_permissions(&plist, fs::Permissions::from_mode(0o644)).expect("its mode");
    let _ = launchctl().run(&bootout_args(label));
    let out = launchctl()
        .run(&bootstrap_args(&plist))
        .expect("launchctl bootstrap ran");
    assert_eq!(
        out.code,
        Some(0),
        "bootstrap {label}: {}",
        out.stderr_text()
    );
    Booted(label.clone())
}

/// Reads the state of `label` through the observer's service manager until `done` holds it, or
/// [`READS`] reads; the last state read.
fn wait_state(label: &Label, done: impl Fn(&UnitState) -> bool) -> UnitState {
    let manager = LaunchdServiceManager::new(launchctl());
    let mut last = None;
    for _ in 0..READS {
        let state = manager.unit_state(label.as_str()).expect("a state");
        if done(&state) {
            return state;
        }
        last = Some(state);
        std::thread::sleep(PAUSE);
    }
    last.expect("read at least once")
}

/// Runs `program` with `args` (no shell) and returns its standard output; a non-zero exit fails.
fn run(program: &str, args: &[&str]) -> String {
    let out = Command::new(program).args(args).output().expect(program);
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn device_inode(path: &Path) -> (u64, u64) {
    let meta = fs::metadata(path).expect("metadata");
    (meta.dev(), meta.ino())
}

/// Waits until `pid` has executed (its path is readable), [`READS`] reads at most.
fn exec_path(pid: u32) -> Result<PathBuf, String> {
    let mut last = Err(String::new());
    for _ in 0..READS {
        last = DarwinPidInfo.path(pid).map_err(|e| e.why);
        if last.as_ref().is_ok_and(|path| !path.as_os_str().is_empty()) {
            return last;
        }
        std::thread::sleep(PAUSE);
    }
    last
}

/// A copy of the fixture at `path`, executable.
fn fixture_copy(path: &Path) {
    fs::copy(FIXTURE, path).expect("copy the fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("its mode");
}

fn serve(exe: &Path) -> Child {
    Command::new(exe).arg("serve").spawn().expect("start serve")
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Catches a package check whose real adapters give another verdict than the system's: an
/// ad-hoc signed copy of `/usr/bin/true` is valid, the copy whose signature was removed is
/// unsigned, a signed binary with a changed byte is invalid, and `xattr -w
/// com.apple.quarantine` marks a file quarantined while a clean file is not.
#[test]
fn real_codesign_and_quarantine_verdicts() {
    as_root();
    let dir = scratch("device-codesign");
    let path = |name: &str| dir.path().join(name);
    let text = |path: &Path| path.display().to_string();

    let signed = path("true-signed");
    fs::copy("/usr/bin/true", &signed).expect("copy true");
    run(
        "/usr/bin/codesign",
        &["--force", "--sign", "-", &text(&signed)],
    );
    let unsigned = path("true-unsigned");
    fs::copy(&signed, &unsigned).expect("copy");
    run(
        "/usr/bin/codesign",
        &["--remove-signature", &text(&unsigned)],
    );
    // A thin binary (the fixture, built for this runner alone), signed ad hoc, then one byte of
    // its first quarter changed: every page before the signature is hashed by it.
    let tampered = path("fixture-tampered");
    fs::copy(FIXTURE, &tampered).expect("copy the fixture");
    run(
        "/usr/bin/codesign",
        &["--force", "--sign", "-", &text(&tampered)],
    );
    let mut bytes = fs::read(&tampered).expect("read");
    let quarter = bytes.len() / 4;
    bytes[quarter] ^= 0xff;
    fs::write(&tampered, &bytes).expect("write");

    let verify = HostMacVerify::system();
    let verdict = |file: &Path| verify.signature(file).expect("codesign ran");
    let found = [
        ("ad-hoc signed", verdict(&signed)),
        ("signature removed", verdict(&unsigned)),
        ("one byte changed", verdict(&tampered)),
    ];
    for (what, verdict) in &found {
        println!("codesign --verify --strict, {what}: {verdict:?}");
    }
    assert_eq!(found[0].1, SignatureVerdict::Valid);
    assert_eq!(found[1].1, SignatureVerdict::Unsigned);
    assert!(
        matches!(found[2].1, SignatureVerdict::Invalid { .. }),
        "{:?}",
        found[2].1
    );

    let marked = path("downloaded");
    let clean = path("clean");
    fs::write(&marked, b"x").expect("write");
    fs::write(&clean, b"x").expect("write");
    run(
        "/usr/bin/xattr",
        &[
            "-w",
            "com.apple.quarantine",
            "0081;00000000;hostkeeper-device-test;",
            &text(&marked),
        ],
    );
    let marked_verdict = verify.quarantine(&marked).expect("getxattr ran");
    let clean_verdict = verify.quarantine(&clean).expect("getxattr ran");
    println!("quarantine: marked {marked_verdict}, clean {clean_verdict}");
    assert!(marked_verdict);
    assert!(!clean_verdict);
}

/// A directory the test made, removed when it is dropped, failed or not.
struct Made(PathBuf);

impl Drop for Made {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Catches a sealed package refused as `artifact_changed` on a Mac because its packages
/// directory is reached through `/var`, which is a link to `/private/var` (every install
/// halted at `prepare` so): a tree made, closed and sealed under `/var` the way the daemon's
/// `/var/lib/hostkeeper/packages` is, is named by its real path under `/private/var` and
/// accepted by the check every step makes; the same tree with one byte of a file changed is
/// `Changed`.
#[test]
fn a_tree_sealed_under_var_passes_its_check_and_a_changed_one_does_not() {
    as_root();
    let base = Made(PathBuf::from(format!(
        "/var/hk-seal-{}",
        std::process::id()
    )));
    // `create_dir_all` would take over a directory that is already there.
    fs::create_dir(&base.0).unwrap_or_else(|e| panic!("create {}: {e}", base.0.display()));
    fs::set_permissions(&base.0, fs::Permissions::from_mode(0o755)).expect("chmod");
    let tree = base.0.join("1.0.0");
    fs::create_dir_all(tree.join("bin")).expect("the tree");
    fs::write(tree.join("bin/hook"), "#!/bin/sh\n").expect("the hook");
    fs::write(tree.join("manifest.toml"), "name = \"x\"\n").expect("a file");
    // Closed as the unpack closes a tree: files 0555 or 0444, directories 0555.
    for (path, mode) in [
        ("bin/hook", 0o555),
        ("manifest.toml", 0o444),
        ("bin", 0o555),
    ] {
        fs::set_permissions(tree.join(path), fs::Permissions::from_mode(mode)).expect("chmod");
    }
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o555)).expect("chmod");
    let real = fs::canonicalize(&base.0)
        .expect("the real path")
        .join("1.0.0");
    assert!(real.starts_with("/private/var"), "{}", real.display());

    let staged = Staged::seal(&tree).expect("sealed");
    assert_eq!(staged.dir(), real);
    let hook = Path::new("bin/hook");
    assert_eq!(
        staged.check(hook).expect("the check passes"),
        real.join("bin/hook")
    );

    // Root writes a 0444 file in a 0555 directory; its mode stays 0444.
    fs::write(tree.join("manifest.toml"), "name = \"y\"\n").expect("change a byte");
    let err = staged.check(hook).expect_err("a changed tree");
    assert!(matches!(err, ArtifactError::Changed { .. }), "{err:?}");
}

/// One row of the `proc_pidpath` findings.
fn finding(case: &str, pid: u32, path_now: &Path) {
    let pidpath = DarwinPidInfo.path(pid);
    let image = DarwinPidInfo.image(pid);
    let at_path = fs::metadata(path_now).map(|meta| (meta.dev(), meta.ino()));
    let probe = LaunchdProcessProbe::new(DarwinPidInfo, StdFileIdentity);
    let digest = probe.exe_digest(pid).map(hex);
    println!(
        "| {case} | {pidpath:?} | image {image:?} | path now {at_path:?} | probe {digest:?} |"
    );
}

fn hex(digest: [u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Catches a process identity that is not the running process's: for a real child, the pid and
/// start time read twice are the same, `proc_pidpath` names the file it was started from, the
/// image's device and inode are that file's, and the probe's digest is the file's; once a new
/// file is renamed over the path, the image is still the old inode, the path names another, and
/// the probe refuses to give a digest (the observer's `unobserved`) rather than hash the new
/// file. The other cases of the findings table are printed, not judged.
#[test]
fn the_process_identity_check_works_against_a_real_process() {
    as_root();
    let dir = scratch("device-procid");
    let exe = dir.path().join("fixture");
    fixture_copy(&exe);
    let real = fs::canonicalize(&exe).expect("canonical path");
    let child = serve(&exe);
    let pid = child.id();
    assert_eq!(exec_path(pid), Ok(real.clone()));
    let first = DarwinPidInfo.start_time(pid).expect("start time");
    let second = DarwinPidInfo.start_time(pid).expect("start time");
    assert_eq!(first, second);
    let image = DarwinPidInfo.image(pid).expect("the image");
    assert_eq!(image, device_inode(&exe));
    let probe = LaunchdProcessProbe::new(DarwinPidInfo, StdFileIdentity);
    let bytes = fs::read(&exe).expect("read");
    let sha: [u8; 32] = {
        use sha2::Digest;
        sha2::Sha256::digest(&bytes).into()
    };
    assert_eq!(probe.exe_digest(pid), Ok(sha));
    println!("| case | proc_pidpath | image (dev, ino) | the path now (dev, ino) | probe digest |");
    println!("|---|---|---|---|---|");
    finding("file unchanged", pid, &exe);

    let new = dir.path().join("fixture-new");
    fixture_copy(&new);
    fs::rename(&new, &exe).expect("rename over");
    finding("file renamed over", pid, &exe);
    assert_eq!(DarwinPidInfo.image(pid), Ok(image), "the running inode");
    assert_ne!(device_inode(&exe), image);
    assert!(
        probe.exe_digest(pid).is_err(),
        "never the new file's digest"
    );
    stop(child);

    let gone = dir.path().join("unlinked");
    fixture_copy(&gone);
    let child = serve(&gone);
    let _ = exec_path(child.id());
    fs::remove_file(&gone).expect("unlink");
    finding("file unlinked", child.id(), &gone);
    stop(child);

    let target = dir.path().join("target");
    fixture_copy(&target);
    let link = dir.path().join("link");
    symlink(&target, &link).expect("symlink");
    let child = serve(&link);
    let _ = exec_path(child.id());
    finding("started through a symlink", child.id(), &link);
    stop(child);

    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() <= 4096 {
        deep.push("d".repeat(200));
    }
    let made = fs::create_dir_all(&deep);
    let long = deep.join("fixture");
    match made.and_then(|()| fs::copy(FIXTURE, &long)) {
        Ok(_) => match Command::new(&long).arg("serve").spawn() {
            Ok(child) => {
                let _ = exec_path(child.id());
                finding("path over PROC_PIDPATHINFO_MAXSIZE", child.id(), &long);
                stop(child);
            }
            Err(e) => println!("| path over PROC_PIDPATHINFO_MAXSIZE | not started: {e} | | | |"),
        },
        Err(e) => println!("| path over PROC_PIDPATHINFO_MAXSIZE | not made: {e} | | | |"),
    }

    let other = dir.path().join("other-user");
    fixture_copy(&other);
    match Command::new(&other).arg("serve").uid(4_294_967_294).spawn() {
        Ok(child) => {
            let _ = exec_path(child.id());
            finding("owned by another user (nobody)", child.id(), &other);
            stop(child);
        }
        Err(e) => println!("| owned by another user (nobody) | not started: {e} | | | |"),
    }
}

/// Catches an observer that reads launchd's job state wrong: a running job is `Active` with its
/// pid, a job that ran and exited 0 is `Inactive`, and a label launchd does not hold is
/// `NotFound`.
#[test]
fn the_observer_reads_launchd_job_state() {
    as_root();
    let dir = scratch("device-observer");
    let exe = dir.path().join("fixture");
    fixture_copy(&exe);
    let exe_text = exe.display().to_string();

    let running = label("observer-running");
    let _running = bootstrap(&running, &[&exe_text, "serve"], dir.path());
    let state = wait_state(&running, |s| s.active == ActiveState::Active);
    println!("running job: {state:?}");
    assert_eq!(state.active, ActiveState::Active);
    assert!(state.main_pid.is_some());

    let ended = label("observer-ended");
    let _ended = bootstrap(&ended, &["/usr/bin/true"], dir.path());
    let state = wait_state(&ended, |s| s.active == ActiveState::Inactive);
    println!("ended job: {state:?}");
    assert_eq!(state.active, ActiveState::Inactive);

    let unknown = label("observer-unknown");
    let manager = LaunchdServiceManager::new(launchctl());
    let state = manager.unit_state(unknown.as_str()).expect("a state");
    println!("unknown label: {state:?}");
    assert_eq!(state.active, ActiveState::NotFound);
}

/// Catches a step job that outlives its bound or keeps an exit: the fixture hook in mode `sleep`
/// runs as a launchd job through the daemon's launcher with a bound of 5 seconds; the launcher
/// boots it out at the bound, the step has no exit code (the runner's failed step), and launchd
/// no longer holds the job.
#[test]
fn a_fixture_hook_runs_as_a_launchd_job_and_is_stopped_at_its_timeout() {
    as_root();
    let dir = scratch("device-launcher");
    let rig = Rig::new(
        dir.path(),
        Path::new(FIXTURE),
        "launcher",
        std::process::id(),
    )
    .expect("a rig");
    let params = rig
        .params("prepare", &Mode::Sleep, None)
        .expect("the params");
    let unit = format!("updater-hkci-prepare-{}", std::process::id());
    let step_label = Label::from_unit(&unit).expect("a step label");
    let _booted = Booted(step_label.clone());
    let call = StepCall {
        service: "hkci",
        invocation: Invocation {
            step: Step::Prepare,
            // `prepare` is given a staged release; the fixture only reads its params.
            artifact: Some(rig.dir().to_path_buf()),
            params: Some(params),
        },
        executable: Path::new(FIXTURE),
        unit: &unit,
        bound: Bound::Secs(std::num::NonZeroU64::new(5).expect("not zero")),
    };
    let exit = LaunchdLauncher::system().run(&call).expect("the step ran");
    println!("the hook at its bound: {exit:?}");
    assert_eq!(exit.code, None, "stopped, not exited");
    assert_eq!(Outcome::from_exit(exit.code), Outcome::Failure);
    let status = print(&launchctl(), &step_label).expect("launchctl print");
    assert_eq!(status, JobStatus::NotLoaded, "the job is gone");
}

/// Catches a boot id of a Mac that is empty, not the kernel's or not stable within a boot: the
/// real reader gives `kern.bootsessionuuid` as a UUID, and two reads in this process give the
/// same id.
#[test]
fn the_boot_id_of_a_mac_is_a_uuid_the_same_on_two_reads() {
    let first = SysctlBootId.read().expect("kern.bootsessionuuid");
    let second = SysctlBootId.read().expect("kern.bootsessionuuid");
    println!("kern.bootsessionuuid: {first}");
    assert!(!first.as_str().is_empty());
    assert_eq!(
        parse_boot_session_uuid(first.as_str()).expect("a UUID"),
        first
    );
    assert_eq!(second, first);
}

/// Catches a parser that no longer reads the `ioreg` of a current macOS: the daemon's Mac claims,
/// [`MacClaims`] over [`HostIoreg::system`] (the real `/usr/sbin/ioreg`, as the daemon runs it),
/// are a non-empty serial and a platform UUID in the 8-4-4-4-12 form, and nothing else. The
/// values differ from host to host, so the test prints them and asserts their shape only; the
/// UUID is judged by `hostkeeper_steps`'s check, not by the parser under test. The canned
/// output of the pull-request tests cannot show that the live layout still parses.
#[test]
fn the_mac_claims_from_the_real_ioreg_are_a_serial_and_a_platform_uuid() {
    let claims = MacClaims::new(Box::new(HostIoreg::system()))
        .claims()
        .expect("the claims of this Mac, from its ioreg");
    println!(
        "ioreg: IOPlatformSerialNumber {:?}, IOPlatformUUID {:?}",
        claims.mac_serial, claims.platform_uuid
    );
    let serial = claims.mac_serial.clone().expect("a serial number");
    let uuid = claims.platform_uuid.clone().expect("a platform UUID");
    assert!(!serial.trim().is_empty(), "the serial {serial:?} is blank");
    assert!(
        !uuid.trim().is_empty(),
        "the platform UUID {uuid:?} is blank"
    );
    let parsed = BootId::uuid("IOPlatformUUID", &uuid).expect("the platform UUID is a UUID");
    assert_eq!(parsed.as_str(), uuid);
    assert_eq!(
        claims,
        Claims {
            mac_serial: Some(serial),
            platform_uuid: Some(uuid),
            ..Claims::default()
        }
    );
}

/// Catches a daemon that does not start under launchd on a Mac, or starts under no boot id: the
/// daemon binary, as a job of the system domain with a config, state and packages directories
/// and admin socket of its own, is `running`, answers `Status` on its admin socket, and its start
/// pass wrote the Mac's boot id.
#[test]
fn the_daemon_starts_under_launchd_and_answers_status() {
    as_root();
    let dir = scratch("device-daemon");
    let root = dir.path();
    let config = root.join("hostkeeper.toml");
    let mut file = fs::File::create(&config).expect("the config");
    file.write_all(
        br#"coordinator = "https://127.0.0.1"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#,
    )
    .expect("write the config");
    drop(file);
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("its mode");
    let state = root.join("state");
    let packages = root.join("packages");
    fs::create_dir_all(&packages).expect("the packages directory");
    let socket = root.join("run").join("admin.sock");
    let text = |path: &Path| path.display().to_string();
    let daemon = label("daemon");
    let argv = [
        DAEMON.to_owned(),
        "daemon".to_owned(),
        "--config".to_owned(),
        text(&config),
        "--state-dir".to_owned(),
        text(&state),
        "--packages-dir".to_owned(),
        text(&packages),
        "--admin-socket".to_owned(),
        text(&socket),
    ];
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let _booted = bootstrap(&daemon, &argv, root);

    let running = wait_state(&daemon, |s| s.active == ActiveState::Active);
    let mut answered = None;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    for _ in 0..READS {
        if socket.exists() {
            let path = socket.clone();
            let connector = tower::service_fn(move |_: Uri| {
                let path = path.clone();
                async move {
                    tokio::net::UnixStream::connect(path)
                        .await
                        .map(TokioIo::new)
                }
            });
            let reply = runtime.block_on(async move {
                let channel = Endpoint::from_static("http://admin.invalid")
                    .connect_with_connector_lazy(connector);
                AdminClient::new(channel).status(StatusRequest {}).await
            });
            if let Ok(reply) = reply {
                answered = Some(reply.into_inner());
                break;
            }
        }
        std::thread::sleep(PAUSE);
    }
    let log = |name: &str| fs::read_to_string(root.join(name)).unwrap_or_default();
    println!("the daemon's job: {running:?}");
    println!("its standard error:\n{}", log("stderr"));
    assert_eq!(running.active, ActiveState::Active, "{}", log("stderr"));
    let status = answered.unwrap_or_else(|| panic!("no Status answer: {}", log("stderr")));
    println!("Status: {status:?}");
    let socket_dir = fs::metadata(socket.parent().expect("a parent")).expect("its directory");
    assert_eq!(socket_dir.permissions().mode() & 0o777, 0o700);
    let written = fs::read_to_string(state.join(BOOT_ID_FILE)).expect("the boot id file");
    let id = SysctlBootId.read().expect("kern.bootsessionuuid");
    assert_eq!(written, id.as_str());
}
