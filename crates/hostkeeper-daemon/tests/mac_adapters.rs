//! The host's adapters by platform, and the real adapters of a Mac's package check and claims,
//! against the fake process port and files in a scratch directory: the argument lists of
//! `codesign` and `ioreg`, how each of their ends is read, the quarantine attribute read from the
//! entry itself, which ports each platform's set asks, and the boot id of the host's clock.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use hostkeeper_daemon::adapters::{Adapters, host_claims, host_managed};
use hostkeeper_daemon::cli::{Cli, Command, UNWIRED, Unwired};
use hostkeeper_daemon::ipmi::DevIpmi;
use hostkeeper_daemon::mac::{
    CODESIGN, HostIoreg, HostMacVerify, IOREG, NOT_SIGNED, QUARANTINE_ATTRIBUTE, TOOL_DEADLINE,
    codesign_args, codesign_verdict, has_attribute, ioreg_args,
};
use hostkeeper_daemon::start::StartError;
use hostkeeper_enroll::{Claims, Ioreg, LinuxClaims, ManagedFile};
use hostkeeper_launchd::launchctl::print_args;
use hostkeeper_launchd::{
    FakeLaunchctl, Label, LaunchctlError, LaunchctlOutput, PidInfo, PidInfoError, Platform,
    PlatformKind,
};
use hostkeeper_observe::{ActiveState, ObserveError, ProcessIdentity};
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::{MacVerify, SignatureVerdict, VerifyIoError};
use rustix::fs::XattrFlags;

/// An attribute a test may set on Linux, where `com.apple.quarantine` is not a name a file can
/// carry: the adapter reads whichever name it is given.
const TEST_ATTRIBUTE: &str = "user.hostkeeper-test-quarantine";

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

fn text(path: &Path) -> String {
    path.to_str().expect("UTF-8").to_owned()
}

fn verifier(fake: &FakeLaunchctl) -> HostMacVerify<FakeLaunchctl> {
    HostMacVerify::new(fake.clone(), TEST_ATTRIBUTE)
}

fn exit(code: Option<i32>, stderr: &str) -> LaunchctlOutput {
    LaunchctlOutput {
        code,
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// Catches a `codesign` end read as the wrong verdict: exit 0 is valid; exit 1 or 3 saying the
/// file is not signed at all is unsigned, and any other exit 1 or 3 is invalid with `codesign`'s
/// words; another code or a signal means no check was made, never a verdict on the binary.
#[test]
fn codesign_verdicts_follow_its_exit_and_its_words() {
    let cases: Vec<(LaunchctlOutput, Result<SignatureVerdict, String>)> = vec![
        (exit(Some(0), ""), Ok(SignatureVerdict::Valid)),
        (
            exit(Some(1), &format!("/s/bin/x: {NOT_SIGNED}\n")),
            Ok(SignatureVerdict::Unsigned),
        ),
        (
            exit(
                Some(1),
                "/s/bin/x: invalid signature (code or signature have been modified)\n",
            ),
            Ok(SignatureVerdict::Invalid {
                note: "/s/bin/x: invalid signature (code or signature have been modified)"
                    .to_owned(),
            }),
        ),
        (
            exit(
                Some(3),
                "/s/bin/x: a sealed resource is missing or invalid\n",
            ),
            Ok(SignatureVerdict::Invalid {
                note: "/s/bin/x: a sealed resource is missing or invalid".to_owned(),
            }),
        ),
        (
            exit(Some(3), &format!("/s/bin/x: {NOT_SIGNED}")),
            Ok(SignatureVerdict::Unsigned),
        ),
        (
            exit(Some(1), ""),
            Ok(SignatureVerdict::Invalid {
                note: String::new(),
            }),
        ),
        (
            exit(Some(2), "codesign: unknown option\n"),
            Err(
                "codesign did not finish: it ended with Some(2): codesign: unknown option"
                    .to_owned(),
            ),
        ),
        (
            exit(None, ""),
            Err("codesign did not finish: it ended with None: ".to_owned()),
        ),
    ];
    for (output, want) in cases {
        let got = codesign_verdict(&output).map_err(|error| error.to_string());
        assert_eq!(got, want, "for {output:?}");
    }
}

/// Catches a signature check that runs anything but `codesign --verify --strict <path>`, or that
/// judges a binary when `codesign` could not run: the argument list is exact; a spawn failure and
/// a kill at the deadline are errors of the check that name the tool; a path that is not UTF-8 is
/// never passed on.
#[test]
fn the_signature_check_runs_codesign_with_a_fixed_argument_list() {
    let binary = Path::new("/packages/web/.stage-1/bin/x");
    assert_eq!(
        codesign_args(&text(binary)),
        ["--verify", "--strict", "/packages/web/.stage-1/bin/x"]
    );

    let fake = FakeLaunchctl::new();
    let args = codesign_args(&text(binary));
    fake.then(&args, Ok(exit(Some(0), "")));
    fake.then(&args, Ok(exit(Some(1), NOT_SIGNED)));
    fake.then(
        &args,
        Err(LaunchctlError::Spawn {
            args: args.join(" "),
            why: "No such file or directory".to_owned(),
        }),
    );
    fake.then(
        &args,
        Err(LaunchctlError::TimedOut {
            args: args.join(" "),
        }),
    );
    let check = verifier(&fake);
    assert_eq!(
        check.signature(binary).expect("a verdict"),
        SignatureVerdict::Valid
    );
    assert_eq!(
        check.signature(binary).expect("a verdict"),
        SignatureVerdict::Unsigned
    );
    let spawn = check.signature(binary).expect_err("codesign did not run");
    assert!(matches!(
        spawn,
        VerifyIoError::Spawn {
            tool: "codesign",
            ..
        }
    ));
    assert_eq!(
        spawn.to_string(),
        "codesign could not be run: No such file or directory"
    );
    let killed = check.signature(binary).expect_err("codesign was killed");
    assert_eq!(
        killed.to_string(),
        format!(
            "codesign did not finish: killed at its deadline of {} seconds",
            TOOL_DEADLINE.as_secs()
        )
    );
    assert_eq!(fake.calls(), vec![args.clone(); 4]);

    let not_utf8 = Path::new(OsStr::from_bytes(b"/packages/web/bin/\xff"));
    let refused = check.signature(not_utf8).expect_err("not UTF-8");
    assert_eq!(
        refused.to_string(),
        format!(
            "codesign did not finish: the path {} is not UTF-8",
            not_utf8.display()
        )
    );
    assert_eq!(fake.calls().len(), 4, "nothing was run for it");
}

/// Catches a quarantine read that follows a link, misses a mark on a file or a directory, or
/// calls an unreadable attribute absent: the attribute is read from the entry itself (a link to
/// a marked file is not marked), a marked file and a marked directory are quarantined, an
/// unmarked file is not, and an entry that cannot be asked is an error of the check.
#[test]
fn the_quarantine_attribute_is_read_from_the_entry_itself() {
    let dir = scratch("mac-quarantine");
    let marked = dir.path().join("marked");
    let clean = dir.path().join("clean");
    let marked_dir = dir.path().join("marked-dir");
    let link = dir.path().join("link");
    fs::write(&marked, b"x").expect("write");
    fs::write(&clean, b"x").expect("write");
    fs::create_dir(&marked_dir).expect("mkdir");
    symlink(&marked, &link).expect("symlink");
    for path in [&marked, &marked_dir] {
        rustix::fs::lsetxattr(
            path.as_path(),
            TEST_ATTRIBUTE,
            b"0081;00000000;Safari;",
            XattrFlags::empty(),
        )
        .expect("set the attribute");
    }

    let check = verifier(&FakeLaunchctl::new());
    assert!(check.quarantine(&marked).expect("read"), "a marked file");
    assert!(
        check.quarantine(&marked_dir).expect("read"),
        "a marked directory"
    );
    assert!(!check.quarantine(&clean).expect("read"), "an unmarked file");
    assert!(
        !check.quarantine(&link).expect("read"),
        "a link is not followed"
    );

    let gone = dir.path().join("gone");
    let error = check.quarantine(&gone).expect_err("nothing to ask");
    assert!(matches!(
        error,
        VerifyIoError::Spawn {
            tool: "getxattr",
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "getxattr could not be run: No such file or directory (os error 2)"
    );
    assert!(!has_attribute(&clean, TEST_ATTRIBUTE).expect("read"));
}

/// Catches the host's adapters built with another tool, attribute or deadline than the
/// documented ones: `/usr/bin/codesign` and `/usr/sbin/ioreg`, killed after 30 seconds, and the
/// attribute `com.apple.quarantine`. Making them runs nothing.
#[test]
fn the_hosts_tools_are_codesign_and_ioreg_with_their_deadline() {
    assert_eq!(CODESIGN, "/usr/bin/codesign");
    assert_eq!(IOREG, "/usr/sbin/ioreg");
    assert_eq!(QUARANTINE_ATTRIBUTE, "com.apple.quarantine");
    assert_eq!(TOOL_DEADLINE.as_secs(), 30);
    let verify = format!("{:?}", HostMacVerify::system());
    assert!(verify.contains("/usr/bin/codesign"), "{verify}");
    assert!(verify.contains("com.apple.quarantine"), "{verify}");
    let ioreg = format!("{:?}", HostIoreg::system());
    assert!(ioreg.contains("/usr/sbin/ioreg"), "{ioreg}");
}

/// Catches an `ioreg` read that is not `ioreg -rd1 -c IOPlatformExpertDevice`, or whose failure
/// becomes text to parse: exit 0 with UTF-8 output is the text; another exit, output that is not
/// UTF-8, a spawn failure and a kill at the deadline are each an error, which stops the
/// enrollment before anything is posted.
#[test]
fn ioreg_runs_with_a_fixed_argument_list_and_needs_exit_0_and_utf8() {
    assert_eq!(ioreg_args(), ["-rd1", "-c", "IOPlatformExpertDevice"]);
    let fake = FakeLaunchctl::new();
    let args = ioreg_args();
    fake.then(&args, Ok(LaunchctlOutput::success("+-o Mac\n")));
    fake.then(
        &args,
        Ok(LaunchctlOutput::exit(1, "ioreg: no such class\n")),
    );
    fake.then(
        &args,
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: vec![0xff],
            stderr: Vec::new(),
        }),
    );
    fake.then(
        &args,
        Err(LaunchctlError::Spawn {
            args: args.join(" "),
            why: "denied".to_owned(),
        }),
    );
    fake.then(
        &args,
        Err(LaunchctlError::TimedOut {
            args: args.join(" "),
        }),
    );
    let ioreg = HostIoreg::new(fake.clone());
    assert_eq!(ioreg.platform_expert().expect("the text"), "+-o Mac\n");
    let errors: Vec<String> = (0..4)
        .map(|_| ioreg.platform_expert().expect_err("an error").to_string())
        .collect();
    assert_eq!(
        errors,
        [
            "ioreg ended with Some(1): ioreg: no such class".to_owned(),
            "ioreg wrote output that is not UTF-8".to_owned(),
            "ioreg could not be run: denied".to_owned(),
            format!(
                "ioreg did not finish: killed at its deadline of {} seconds",
                TOOL_DEADLINE.as_secs()
            ),
        ]
    );
    assert_eq!(fake.calls(), vec![args; 5]);
}

/// An `ioreg` that prints one platform expert device.
struct FakeIoreg;

impl Ioreg for FakeIoreg {
    fn platform_expert(&self) -> std::io::Result<String> {
        Ok(concat!(
            "+-o Mac  <class IOPlatformExpertDevice>\n",
            "  {\n",
            "    \"IOPlatformSerialNumber\" = \"C02TEST0ABCD\"\n",
            "    \"IOPlatformUUID\" = \"12345678-90AB-CDEF-1234-567890ABCDEF\"\n",
            "  }\n",
        )
        .to_owned())
    }
}

/// Catches Linux collectors run on a Mac: the claims of a Mac are the serial and the platform
/// UUID `ioreg` gives, and nothing else.
#[test]
fn a_macs_claims_are_its_serial_and_platform_uuid_alone() {
    let claims = host_claims(
        PlatformKind::Macos,
        LinuxClaims::host(),
        Box::new(FakeIoreg),
        Arc::new(DevIpmi::host()),
    )
    .claims()
    .expect("the claims");
    assert_eq!(
        claims,
        Claims {
            mac_serial: Some("C02TEST0ABCD".to_owned()),
            platform_uuid: Some("12345678-90AB-CDEF-1234-567890ABCDEF".to_owned()),
            ..Claims::default()
        }
    );
    // A Linux host's claims are its own collectors'; making them reads nothing.
    let _linux = host_claims(
        PlatformKind::Linux,
        LinuxClaims::host(),
        Box::new(FakeIoreg),
        Arc::new(DevIpmi::host()),
    );
}

/// Catches a Mac whose enroller does not read its managed preferences, or a Linux host that
/// reads any: a Mac's are the file it is given, read with its checks, and a Linux host has none.
#[test]
fn a_mac_reads_its_managed_preferences_and_linux_has_none() {
    let dir = scratch("managed-preferences");
    let file = dir.path().join("dev.hostkeeper.enrollment.plist");
    fs::write(&file, b"managed").expect("write");
    fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644)).expect("chmod");
    let uid = rustix::process::geteuid().as_raw();
    let mac = host_managed(PlatformKind::Macos, ManagedFile::new(&file, uid))
        .expect("a Mac's managed preferences");
    assert_eq!(mac.read().expect("read"), b"managed");
    assert!(host_managed(PlatformKind::Linux, ManagedFile::new(&file, uid)).is_none());
}

/// A pid lookup with fixed answers.
struct FakePids;

impl PidInfo for FakePids {
    fn path(&self, pid: u32) -> Result<PathBuf, PidInfoError> {
        Err(PidInfoError {
            pid,
            why: "no path in this test".to_owned(),
        })
    }

    fn start_time(&self, _pid: u32) -> Result<u64, PidInfoError> {
        Ok(41)
    }

    fn image(&self, pid: u32) -> Result<(u64, u64), PidInfoError> {
        Err(PidInfoError {
            pid,
            why: "no image in this test".to_owned(),
        })
    }
}

/// A package check that marks one path.
struct OnePath(PathBuf);

impl MacVerify for OnePath {
    fn quarantine(&self, path: &Path) -> Result<bool, VerifyIoError> {
        Ok(path == self.0)
    }

    fn signature(&self, _binary: &Path) -> Result<SignatureVerdict, VerifyIoError> {
        Ok(SignatureVerdict::Unsigned)
    }
}

fn running(label: &str, pid: u32) -> LaunchctlOutput {
    LaunchctlOutput::success(&format!(
        "system/{label} = {{\n\tstate = running\n\tpid = {pid}\n}}\n"
    ))
}

/// Catches a macOS set wired to the wrong ports: the unit control asks the `launchctl` it was
/// given (with the long deadline) and the observer's service manager the other (`print`), the
/// process probe asks the pid lookup, the steps run on the launcher given, the units are linked
/// in the platform's directory, and every package is judged by the check given.
#[test]
fn the_macos_adapters_ask_launchd_through_the_ports_they_are_given() {
    let dir = scratch("adapters-macos");
    let platform = Platform {
        unit_dir: dir.path().to_path_buf(),
        ..Platform::macos()
    };
    let label = "dev.hostkeeper.web-main";
    let printed = print_args(&Label::parse(label).expect("a label"));
    let launchctl = FakeLaunchctl::new();
    launchctl.always(&printed, Ok(running(label, 300)));
    let print = FakeLaunchctl::new();
    print.always(&printed, Ok(running(label, 301)));
    let marked = dir.path().join("marked");

    let adapters = Adapters::macos(
        &platform,
        Arc::new(Unwired),
        launchctl.clone(),
        print.clone(),
        FakePids,
        Arc::new(OnePath(marked.clone())),
    )
    .expect("the macOS set");

    assert_eq!(adapters.kind, PlatformKind::Macos);
    assert_eq!(adapters.unit_dir, dir.path());
    assert_eq!(adapters.units.is_active(label), Ok(true), "a running job");
    assert_eq!(launchctl.calls(), std::slice::from_ref(&printed));
    let state = adapters.manager.unit_state(label).expect("a state");
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.main_pid, Some(301));
    assert_eq!(print.calls(), std::slice::from_ref(&printed));
    assert_eq!(
        adapters.process.identity(9),
        Ok(ProcessIdentity {
            pid: 9,
            start_time: 41
        })
    );
    assert_eq!(
        adapters.process.exe_digest(9),
        Err(ObserveError::Process {
            pid: 9,
            why: "no path in this test".to_owned()
        })
    );
    let refused = adapters
        .launcher
        .stop("updater-web-prepare-1")
        .expect_err("the launcher given");
    assert_eq!(
        refused.to_string(),
        format!("the launcher failed: {UNWIRED}")
    );
    let check = adapters.package_check.expect("a Mac checks its packages");
    assert!(check.quarantine(&marked).expect("a verdict"));
    assert!(!check.quarantine(dir.path()).expect("a verdict"));
}

/// Catches a Mac daemon that starts without its unit directory, or a Linux build that starts as
/// a Mac: a macOS set whose unit directory cannot be read stops the start with `Io` naming it,
/// and a Linux build asked for macOS has no adapters for it.
#[test]
fn a_macos_set_that_cannot_be_built_stops_the_start() {
    let dir = scratch("adapters-macos-missing");
    let missing = dir.path().join("LaunchDaemons");
    let platform = Platform {
        unit_dir: missing.clone(),
        ..Platform::macos()
    };
    let error = Adapters::macos(
        &platform,
        Arc::new(Unwired),
        FakeLaunchctl::new(),
        FakeLaunchctl::new(),
        FakePids,
        Arc::new(OnePath(PathBuf::new())),
    )
    .err()
    .expect("no unit directory");
    assert!(
        matches!(&error, StartError::Io { path, .. } if *path == missing),
        "{error}"
    );

    // A Linux build has no macOS adapters to give; a Mac's build gives its host set.
    #[cfg(not(target_os = "macos"))]
    {
        let error = Adapters::for_platform(&Platform::macos())
            .err()
            .expect("this is a Linux build");
        assert!(matches!(
            error,
            StartError::NoAdapters { platform: "macOS" }
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let adapters = Adapters::for_platform(&Platform::macos()).expect("this is a Mac build");
        assert_eq!(adapters.kind, PlatformKind::Macos);
    }
}

/// Catches a Linux set that is not the systemd set: the systemd unit directory and no package
/// check. Its unit control runs the host's `systemctl`, so it is not asked here
/// (tests/linux_adapters.rs runs it against a fake, privileged.yml against the host's systemd).
#[test]
fn the_linux_adapters_are_the_systemd_set() {
    let adapters = Adapters::for_platform(&Platform::linux()).expect("the Linux set");
    assert_eq!(adapters.kind, PlatformKind::Linux);
    assert_eq!(adapters.unit_dir, Path::new("/etc/systemd/system"));
    assert!(adapters.package_check.is_none());
}

/// Catches a run directory that is not the host platform's: the flags' settings name where the
/// platform keeps its run directory (`/var/run` on a Mac), which the registration socket's
/// directory is under.
#[test]
fn the_run_directory_of_the_settings_is_the_host_platforms() {
    let Command::Daemon(flags) = Cli::parse_from(["hostkeeper", "daemon"]).command else {
        panic!("`daemon` parses as the daemon verb");
    };
    assert_eq!(flags.settings().run_dir, Platform::host().run_dir);
}

/// Catches a daemon built for Linux whose clock has no boot id, or one that is not the kernel's:
/// the host's clock and the host's boot-id port both give the kernel's UUID, the same on two
/// reads, and a stamp taken on the clock is of that boot.
#[cfg(target_os = "linux")]
#[test]
fn the_host_clock_of_a_linux_build_reads_the_kernels_boot_id() {
    // Imported here, not at the top: on a Mac, which runs none of this test, they would be unused.
    use hostkeeper_daemon::adapters::{host_boot_id, host_clock};
    use hostkeeper_steps::{BootId, Clock, Stamp};

    let clock = host_clock();
    let id = clock.boot_id().expect("the host's boot id");
    assert!(BootId::uuid("the host", id.as_str()).is_ok(), "{id}");
    assert_eq!(host_boot_id().read().expect("the host's boot id"), id);
    assert_eq!(Stamp::now(&clock).boot_id, Some(id));
}
