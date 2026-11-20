//! The tests of the stage's frame: the scope and the package manager it picks, what it defers
//! before and after the package manager's part, the runs that end without an exit, the host's
//! disk and wait, and the host's programs' empty environment (over a child of this test binary).

use std::os::unix::ffi::OsStrExt;

use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;

use super::*;
use crate::maintenance_hook::linux::testing::{
    FakeHost, TIMED_OUT, args, deferred, host_with, params, params_scoped, ran, scratch, signalled,
};

const APT: &str = "/usr/bin/apt-get";

/// A fake host whose apt-get refreshes, simulates one security update and lists one archive of
/// 10 bytes, and whose download is scripted by the test.
fn apt_host() -> FakeHost {
    let fake = FakeHost::new();
    fake.always(&args(&["update", "-qq"]), ran(0, "", ""));
    fake.always(
        &args(&["--simulate", "-qq", "dist-upgrade"]),
        ran(
            0,
            "Inst openssl [3.0.13-0ubuntu3.2] (3.0.13-0ubuntu3.4 Ubuntu:24.04/noble-security [amd64])\n",
            "",
        ),
    );
    fake.always(
        &args(&[
            "--print-uris",
            "-qq",
            "install",
            "--only-upgrade",
            "openssl",
        ]),
        ran(0, "'http://x/openssl.deb' openssl.deb 10 SHA512:1\n", ""),
    );
    fake
}

fn download_args() -> Vec<String> {
    args(&[
        "--download-only",
        "-y",
        "-qq",
        "-o",
        "Dpkg::Options::=--force-confold",
        "install",
        "--only-upgrade",
        "openssl",
    ])
}

/// Proves: an order whose `linux_scope` is unspecified or a value this release does not know
/// defers `linux_scope_invalid`, naming the value, before anything runs.
///
/// Catches: a missing scope read as `all`, which would stage every update.
#[test]
fn a_scope_that_is_not_security_or_all_defers_before_anything_runs() {
    let root = host_with("stage-scope", APT);
    for value in [0, 9] {
        let fake = apt_host();
        let outcome = fake.stage(&root, 300).run(&params_scoped(value));
        assert_eq!(
            outcome,
            deferred(
                REASON_SCOPE_INVALID,
                &format!("linux_scope {value} is not security or all")
            )
        );
        assert_eq!(fake.calls(), Vec::new());
    }
}

/// Proves: a host with none of the three programs defers `no_package_manager`.
///
/// Catches: a stage that guesses a package manager it does not have.
#[test]
fn a_host_with_no_package_manager_defers() {
    let root = scratch("stage-none");
    let fake = apt_host();
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_NO_PACKAGE_MANAGER,
            "this host has none of apt-get, dnf and apk"
        )
    );
    assert_eq!(fake.calls(), Vec::new());
}

/// Proves: the package manager is `apt-get` where it is there, else `dnf`, else `apk`, as the
/// maintenance look picks it; none without any of them.
///
/// Catches: a host with two package managers staged with the one the look did not read.
#[test]
fn the_package_manager_is_apt_then_dnf_then_apk() {
    let root = host_with("stage-managers", "/sbin/apk");
    let under = || Manager::under(&HostRoot::at(root.path()));
    assert_eq!(under(), Some(Manager::Apk));
    std::fs::create_dir_all(root.path().join("usr/bin")).expect("usr/bin");
    std::fs::write(root.path().join("usr/bin/dnf"), "").expect("dnf");
    assert_eq!(under(), Some(Manager::Dnf));
    std::fs::write(root.path().join("usr/bin/apt-get"), "").expect("apt-get");
    assert_eq!(under(), Some(Manager::Apt));
    assert_eq!(
        Manager::under(&HostRoot::at(scratch("stage-bare").path())),
        None
    );
    assert_eq!(
        [Manager::Apt, Manager::Dnf, Manager::Apk].map(Manager::program),
        ["/usr/bin/apt-get", "/usr/bin/dnf", "/sbin/apk"]
    );
}

/// Proves: hostkeeper's own packages that cannot be read defer `own_packages_unreadable` before
/// anything runs.
///
/// Catches: a stage that goes on without knowing what it must leave out, and so downloads
/// hostkeeper's own packages.
#[test]
fn own_packages_that_cannot_be_read_defer() {
    let root = host_with("stage-own", APT);
    let fake = apt_host();
    fake.own(Err("placed.json could not be read"));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_OWN_PACKAGES,
            "own packages: placed.json could not be read"
        )
    );
    assert_eq!(fake.calls(), Vec::new());
}

/// Proves: a download directory that cannot be made, or whose free space cannot be read, defers
/// `stage_dir_failed` with the disk's reason, and nothing is downloaded.
///
/// Catches: an unreadable free space taken as enough.
#[test]
fn a_directory_that_cannot_be_made_or_measured_defers() {
    let root = host_with("stage-dir", APT);
    let fake = apt_host();
    fake.make_fails("/var/cache/apt/archives could not be made: read-only");
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_STAGE_DIR,
            "/var/cache/apt/archives could not be made: read-only"
        )
    );
    assert_eq!(fake.measured(), Vec::<String>::new());

    let fake = apt_host();
    fake.free(Err(
        "the free space of /var/cache/apt/archives could not be read".to_owned(),
    ));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_STAGE_DIR,
            "the free space of /var/cache/apt/archives could not be read"
        )
    );
    assert!(!fake.arg_lists().contains(&download_args()));
}

/// Proves: a command that could not be run, one stopped at its deadline and one ended by a signal,
/// and a listing that could not be run or was ended by a signal, each defer with the step named
/// and no argument.
///
/// Catches: a run without an exit code read as a success.
#[test]
fn runs_that_end_without_an_exit_defer_naming_the_step() {
    let root = host_with("stage-ends", APT);
    let cases: [(&dyn Fn(&FakeHost), &'static str, &'static str); 5] = [
        (
            &|fake| fake.always(&args(&["update", "-qq"]), Err("No such file".to_owned())),
            REASON_LISTING_FAILED,
            "apt-get update could not be run: No such file",
        ),
        (
            &|fake| fake.always(&download_args(), Err(TIMED_OUT.to_owned())),
            REASON_DOWNLOAD_FAILED,
            "apt-get --download-only did not finish within its deadline",
        ),
        (
            &|fake| fake.always(&download_args(), signalled()),
            REASON_DOWNLOAD_FAILED,
            "apt-get --download-only was ended by a signal",
        ),
        (
            &|fake| {
                fake.always(
                    &args(&["--simulate", "-qq", "dist-upgrade"]),
                    Err("No such file".to_owned()),
                );
            },
            REASON_LISTING_FAILED,
            "apt-get dist-upgrade simulation could not be run: No such file",
        ),
        (
            &|fake| fake.always(&args(&["--simulate", "-qq", "dist-upgrade"]), signalled()),
            REASON_LISTING_FAILED,
            "apt-get dist-upgrade simulation was ended by a signal",
        ),
    ];
    for (script, reason, detail) in cases {
        let fake = apt_host();
        script(&fake);
        let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
        assert_eq!(outcome, deferred(reason, detail));
        assert_eq!(fake.waits(), Vec::<u64>::new());
    }
}

/// Proves: a number and a unit are scaled exactly, a fraction rounded up to a byte; a number with
/// no whole part, two points, a sign, more than 6 decimals, or past `u64` is refused.
///
/// Catches: a fraction dropped, which would make an estimate too small.
#[test]
fn scaled_rounds_up_and_refuses_what_is_not_a_number() {
    assert_eq!(scaled("12", 1024), Some(12_288));
    assert_eq!(scaled("1.5", 1024), Some(1536));
    assert_eq!(scaled("0.1", 1), Some(1));
    assert_eq!(scaled("1.", 1), Some(1));
    assert_eq!(scaled("0.000001", 1_000_000), Some(1));
    let refused = [
        "",
        ".5",
        "1.2.3",
        "+1",
        "1.1234567",
        "18446744073709551616",
        "340282366920938463463374607431768211456",
    ];
    for number in refused {
        assert_eq!(scaled(number, 1), None, "{number:?}");
    }
    assert_eq!(scaled("18446744073709551615", 2), None);
    assert_eq!(
        scaled("340282366920938463463374607431768211455", 2),
        None,
        "past u128 once scaled"
    );
}

/// Proves: the host's disk makes a directory and its parents under its root, reads a free space
/// of its filesystem no larger than the filesystem (other writers may move it, so it is not
/// compared with a second read), and names the directory when it cannot.
///
/// Catches: a free space read from the wrong path, or an error that does not say which.
#[test]
fn the_host_disk_makes_and_measures_under_its_root() {
    let root = scratch("stage-disk");
    let disk = HostDisk::new(HostRoot::at(root.path()));
    assert_eq!(disk.make_dir("/var/cache/stage/apk"), Ok(()));
    assert!(root.path().join("var/cache/stage/apk").is_dir());
    let stat = rustix::fs::statvfs(root.path()).expect("statvfs");
    let free = disk.free_bytes("/var/cache/stage/apk").expect("measured");
    assert!(free <= stat.f_blocks * stat.f_frsize, "{free}");
    let missing = disk.free_bytes("/missing").expect_err("not there");
    assert!(
        missing.starts_with("the free space of /missing could not be read: "),
        "{missing}"
    );
    std::fs::write(root.path().join("file"), "").expect("a file");
    let made = disk.make_dir("/file/sub").expect_err("under a file");
    assert!(made.starts_with("/file/sub could not be made: "), "{made}");
}

/// Proves: the host's wait returns for a wait of no time (it sleeps the thread; nothing is timed).
#[test]
fn the_host_wait_returns() {
    SleepWaiter.wait(0);
}

/// The environment this process has, without what an instrumented build sets by itself in each
/// process it starts (`__LLVM_PROFILE_RT_*`), which no parent passed.
#[expect(
    clippy::disallowed_methods,
    reason = "the helper child reports the environment its parent gave it"
)]
fn own_environment() -> Vec<String> {
    std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(b"__LLVM_PROFILE_RT_"))
        .map(|(name, value)| format!("env:{}={}", name.to_string_lossy(), value.to_string_lossy()))
        .collect()
}

/// Catches nothing itself: the child of `the_host_programs_run_with_an_empty_environment`, which
/// prints each argument and each variable it was given on a line of its own.
#[test]
#[ignore = "the child process of the_host_programs_run_with_an_empty_environment; run only by it"]
fn child_prints_its_arguments_and_environment() {
    for arg in std::env::args().skip(1) {
        println!("arg:{arg}");
    }
    for line in own_environment() {
        println!("{line}");
    }
}

/// The arguments that run this module's helper child in this test binary.
fn helper() -> Vec<String> {
    args(&[
        "maintenance_hook::linux::stage::tests::child_prints_its_arguments_and_environment",
        "--exact",
        "--ignored",
        "--nocapture",
    ])
}

/// The child's own lines in `text`: its arguments and its variables.
fn reported(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("arg:") || line.starts_with("env:"))
        .map(str::to_owned)
        .collect()
}

/// Proves: the host's programs, a command and a listing, are run with exactly the arguments
/// given and an empty environment, over a child of this test binary that prints what it got.
///
/// Catches: a runner that passes the hook's own environment on to apt, dnf or apk (a proxy, a
/// locale, a `DEBIAN_FRONTEND`), or that adds an argument.
#[test]
fn the_host_programs_run_with_an_empty_environment() {
    let exe = std::env::current_exe().expect("the test binary");
    let expected: Vec<String> = helper().iter().map(|arg| format!("arg:{arg}")).collect();

    let command = HostPrograms.command(&exe, Duration::from_secs(60));
    let output = command.run(&helper()).expect("the child ran");
    assert_eq!(output.code, Some(0));
    assert_eq!(reported(&String::from_utf8_lossy(&output.stdout)), expected);

    let listing = HostPrograms.listing(&exe, Duration::from_secs(60));
    let mut lines = Vec::new();
    let code = listing
        .run_lines(&helper(), &mut |line| lines.push(line.to_owned()))
        .expect("the child ran");
    assert_eq!(code, Some(0));
    assert_eq!(reported(&lines.join("\n")), expected);
}
