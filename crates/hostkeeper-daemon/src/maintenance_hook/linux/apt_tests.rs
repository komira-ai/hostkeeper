//! The tests of apt's stage, over the fake host ([`super::super::testing`]) and the Ubuntu 24.04
//! simulation the maintenance look's tests use: each run's program, way, arguments and deadline,
//! the directory measured, the waits and the outcome, asserted by equality.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;

use super::*;
use crate::maintenance_hook::linux::stage::{
    REASON_BUSY, REASON_DOWNLOAD_FAILED, REASON_SPACE_SHORT,
};
use crate::maintenance_hook::linux::testing::{
    Call, FakeHost, Way, args, deferred, host_with, params, ran,
};

const UBUNTU: &str = include_str!("../../../tests/fixtures/maintenance/apt/ubuntu_2404.txt");
const NOTHING: &str = include_str!("../../../tests/fixtures/maintenance/apt/nothing_to_do.txt");
const APT: &str = "/usr/bin/apt-get";
const LOCKED: &str =
    "E: Could not get lock /var/lib/apt/lists/lock. It is held by process 4242 (apt-get)";

/// The packages of the Ubuntu simulation the scope `security` makes due: its security updates
/// and its kernel image, not `vim-tiny` (updates only) or `hostkeeper` (hostkeeper's own).
const SECURITY: [&str; 6] = [
    "libc6",
    "libc-bin",
    "openssl",
    "linux-image-6.8.0-45-generic",
    "linux-modules-6.8.0-45-generic",
    "linux-generic",
];

/// The packages the scope `all` makes due: every one but `hostkeeper`.
const ALL: [&str; 7] = [
    "libc6",
    "libc-bin",
    "openssl",
    "linux-image-6.8.0-45-generic",
    "linux-modules-6.8.0-45-generic",
    "linux-generic",
    "vim-tiny",
];

/// What `--print-uris` writes for two archives of 3000 and 4000 bytes.
const URIS: &str = "\
'http://archive.ubuntu.com/ubuntu/pool/main/g/glibc/libc6_2.39-0ubuntu8.3_amd64.deb' libc6_2.39-0ubuntu8.3_amd64.deb 3000 SHA512:9f2c
'http://archive.ubuntu.com/ubuntu/pool/main/o/openssl/openssl_3.0.13-0ubuntu3.4_amd64.deb' openssl_3.0.13-0ubuntu3.4_amd64.deb 4000 SHA512:41ab
";

fn update() -> Vec<String> {
    args(&["update", "-qq"])
}

fn simulate() -> Vec<String> {
    args(&["--simulate", "-qq", "dist-upgrade"])
}

fn print_uris(packages: &[&str]) -> Vec<String> {
    let mut list = args(&["--print-uris", "-qq", "install", "--only-upgrade"]);
    list.extend(args(packages));
    list
}

fn download(packages: &[&str]) -> Vec<String> {
    let mut list = args(&[
        "--download-only",
        "-y",
        "-qq",
        "-o",
        "Dpkg::Options::=--force-confold",
        "install",
        "--only-upgrade",
    ]);
    list.extend(args(packages));
    list
}

fn call(way: Way, args: Vec<String>, secs: u64) -> Call {
    Call {
        program: APT.into(),
        way,
        args,
        deadline: Duration::from_secs(secs),
    }
}

/// A fake host whose `apt-get` refreshes, simulates `simulation`, lists `URIS` for `packages` and
/// downloads them.
fn apt_host(simulation: &str, packages: &[&str]) -> FakeHost {
    let fake = FakeHost::new();
    fake.always(&update(), ran(0, "", ""));
    fake.always(&simulate(), ran(0, simulation, ""));
    fake.always(&print_uris(packages), ran(0, URIS, ""));
    fake.always(&download(packages), ran(0, "", ""));
    fake
}

/// Proves: with room for exactly twice the download, the scope `security` refreshes the index,
/// simulates the upgrade, sizes the security updates and kernels and downloads them alone, each
/// run of `/usr/bin/apt-get` with exactly these arguments; the download lands in apt's archives,
/// which are measured, and the stage succeeds without a wait.
///
/// Catches: an install in place of a download, a non-security update or hostkeeper's own package
/// downloaded, the confold option lost, or a free-space check that wants more than twice.
#[test]
fn security_scope_downloads_the_security_updates_and_kernels() {
    let root = host_with("stage-apt-security", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.free(Ok(14_000));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.calls(),
        vec![
            call(Way::Command, update(), 600),
            call(Way::Listing, simulate(), 600),
            call(Way::Listing, print_uris(&SECURITY), 600),
            call(Way::Command, download(&SECURITY), 3600),
        ]
    );
    assert_eq!(fake.made(), vec![ARCHIVES.to_owned()]);
    assert_eq!(fake.measured(), vec!["/var/cache/apt/archives".to_owned()]);
    assert_eq!(fake.waits(), Vec::<u64>::new());
}

/// Proves: the scope `all` downloads every update the simulation lists but `hostkeeper`,
/// hostkeeper's own package, which no argument names.
///
/// Catches: hostkeeper's own package moved by a maintenance download, or `all` read as
/// `security`.
#[test]
fn all_scope_downloads_every_update_but_hostkeepers_own() {
    let root = host_with("stage-apt-all", APT);
    let fake = apt_host(UBUNTU, &ALL);
    let outcome = fake.stage(&root, 300).run(&params(WireScope::All));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.arg_lists(),
        vec![update(), simulate(), print_uris(&ALL), download(&ALL)]
    );
    for list in fake.arg_lists() {
        assert!(
            !list.iter().any(|arg| arg.contains("hostkeeper")),
            "{list:?}"
        );
    }
}

/// Proves: one byte short of twice the download, the stage defers `free_space_short` naming the
/// directory, the free bytes, the download and the bytes it needs, and runs no download.
///
/// Catches: a stage that downloads onto a disk it would fill, or that checks once the download
/// started.
#[test]
fn short_space_defers_and_runs_no_download() {
    let root = host_with("stage-apt-short", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.free(Ok(13_999));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_SPACE_SHORT,
            "/var/cache/apt/archives has 13999 bytes free; the download of 7000 bytes needs 14000"
        )
    );
    assert_eq!(
        fake.arg_lists(),
        vec![update(), simulate(), print_uris(&SECURITY)]
    );
}

/// Proves: a download that fails defers `stage_download_failed` with the step, the exit code and
/// apt's first error line.
///
/// Catches: a failed download taken as staged, or read as a failure that halts the host.
#[test]
fn a_failed_download_defers() {
    let root = host_with("stage-apt-fail", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.always(
        &download(&SECURITY),
        ran(
            100,
            "",
            "E: Failed to fetch http://archive.ubuntu.com/x.deb  404  Not Found\nE: Some files failed to download\n",
        ),
    );
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_DOWNLOAD_FAILED,
            "apt-get --download-only failed with exit code 100: E: Failed to fetch http://archive.ubuntu.com/x.deb  404  Not Found"
        )
    );
}

/// Proves: a lock held at the refresh (twice) and at the download (once) is waited for through
/// the injected waiter, 2 seconds a try, and the stage then succeeds: each run is tried again
/// with the same arguments.
///
/// Catches: a held lock read as a failure, a wait made by sleeping, or a stage that goes on
/// before the refresh ran.
#[test]
fn a_held_lock_is_waited_for_through_the_waiter() {
    let root = host_with("stage-apt-lock", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.then(&update(), ran(100, "", LOCKED));
    fake.then(&update(), ran(100, "", LOCKED));
    fake.then(&download(&SECURITY), ran(100, "", LOCKED));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.waits(), vec![2, 2, 2]);
    assert_eq!(
        fake.arg_lists(),
        vec![
            update(),
            update(),
            update(),
            simulate(),
            print_uris(&SECURITY),
            download(&SECURITY),
            download(&SECURITY),
        ]
    );
}

/// Proves: a lock held through the whole bound (3 seconds: a wait of 2, then of the 1 left)
/// defers `package_manager_busy` naming the seconds and the step, and nothing after the refresh
/// runs.
///
/// Catches: a wait past `--package-lock-wait-secs`, or one that never ends.
#[test]
fn a_lock_held_past_the_bound_defers_busy() {
    let root = host_with("stage-apt-busy", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.always(&update(), ran(100, "", LOCKED));
    let outcome = fake.stage(&root, 3).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_BUSY,
            "the package manager's lock was held for 3s of 3s (apt-get update)"
        )
    );
    assert_eq!(fake.waits(), vec![2, 1]);
    assert_eq!(fake.arg_lists(), vec![update(), update(), update()]);
}

/// Proves: with nothing due the stage succeeds after the simulation, sizing, measuring and
/// downloading nothing.
///
/// Catches: an empty `install --only-upgrade`, which apt runs as nothing but still locks for.
#[test]
fn nothing_due_downloads_nothing() {
    let root = host_with("stage-apt-nothing", APT);
    let fake = apt_host(NOTHING, &[]);
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.arg_lists(), vec![update(), simulate()]);
    assert_eq!(fake.made(), Vec::<String>::new());
}

/// Proves: when every archive is already downloaded (`--print-uris` lists none), the stage
/// succeeds and runs no download.
///
/// Catches: a stage that measures or downloads for 0 bytes.
#[test]
fn archives_already_downloaded_download_nothing() {
    let root = host_with("stage-apt-cached", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.always(&print_uris(&SECURITY), ran(0, "\n", ""));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.arg_lists(),
        vec![update(), simulate(), print_uris(&SECURITY)]
    );
    assert_eq!(fake.measured(), Vec::<String>::new());
}

/// Proves: a `--print-uris` line that is not an archive defers `stage_listing_failed`, and
/// nothing is downloaded.
///
/// Catches: an estimate that passes over what it cannot read and so comes out too small.
#[test]
fn a_print_uris_line_it_does_not_know_defers() {
    let root = host_with("stage-apt-unknown", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.always(&print_uris(&SECURITY), ran(0, "W: something else\n", ""));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_LISTING_FAILED,
            "apt-get --print-uris wrote a line that is not an archive"
        )
    );
    assert_eq!(fake.made(), Vec::<String>::new());
}

/// Proves: a refresh that fails for another reason than the lock defers `stage_listing_failed`
/// at once, with apt's first error line, and is not tried again.
///
/// Catches: every apt failure taken as a held lock and waited for.
#[test]
fn a_failed_refresh_defers_with_its_first_error_line() {
    let root = host_with("stage-apt-refresh", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.always(
        &update(),
        ran(
            100,
            "",
            "E: The repository 'http://archive.example.com noble Release' is not signed.\n",
        ),
    );
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_LISTING_FAILED,
            "apt-get update failed with exit code 100: E: The repository 'http://archive.example.com noble Release' is not signed."
        )
    );
    assert_eq!(fake.arg_lists(), vec![update()]);
    assert_eq!(fake.waits(), Vec::<u64>::new());
}

/// Proves: a simulation that fails defers `stage_listing_failed` with its exit code.
///
/// Catches: a failed simulation read as nothing due, which would stage nothing and say so.
#[test]
fn a_failed_simulation_defers() {
    let root = host_with("stage-apt-simulation", APT);
    let fake = apt_host(UBUNTU, &SECURITY);
    fake.always(&simulate(), ran(100, "", "E: Broken packages\n"));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_LISTING_FAILED,
            "apt-get dist-upgrade simulation failed with exit code 100"
        )
    );
}

/// Proves: a package the simulation names twice (an `Inst` line repeated) is given to apt once.
///
/// Catches: a package named twice on apt's command line.
#[test]
fn a_package_named_twice_is_downloaded_once() {
    let own = std::collections::BTreeSet::from(["hostkeeper".to_owned()]);
    let look = Look {
        scope: crate::maintenance_collect::LinuxScope::All,
        own_packages: &own,
    };
    let line =
        "Inst vim-tiny [2:9.1-1ubuntu7.2] (2:9.1-1ubuntu7.3 Ubuntu:24.04/noble-updates [amd64])";
    assert_eq!(
        due_packages(&format!("{line}\n{line}\n"), &look),
        vec!["vim-tiny".to_owned()]
    );
}

/// Proves: apt's lock is held only for a run that did not exit 0 and says so in one of apt's or
/// dpkg's forms.
///
/// Catches: a successful run that mentions a lock tried again, or any failure taken as busy.
#[test]
fn busy_is_a_failed_run_that_names_the_lock() {
    assert!(busy(Some(100), LOCKED));
    assert!(busy(
        Some(2),
        "dpkg: error: dpkg frontend lock was locked by another process with pid 7"
    ));
    assert!(busy(
        None,
        "E: Could not get lock /var/cache/apt/archives/lock"
    ));
    assert!(!busy(Some(0), LOCKED));
    assert!(!busy(Some(100), "E: Unable to locate package x"));
}

/// Proves: the download's bytes are the sum of each `--print-uris` line's size, blank lines
/// passed over; a line without the quoted URI, without a size or with a size that is not a
/// number, and a sum past `u64`, give none.
///
/// Catches: a size read from the wrong field, or an unreadable line counted as 0.
#[test]
fn download_bytes_sums_the_archive_sizes() {
    let lines = |text: &str| -> Vec<String> { text.lines().map(str::to_owned).collect() };
    assert_eq!(download_bytes(&lines(URIS)), Some(7000));
    assert_eq!(download_bytes(&lines("\n  \n")), Some(0));
    assert_eq!(
        download_bytes(&lines("http://archive.example.com/a.deb a.deb 10 SHA512:1")),
        None
    );
    assert_eq!(
        download_bytes(&lines("'http://archive.example.com/a.deb' a.deb")),
        None
    );
    assert_eq!(
        download_bytes(&lines(
            "'http://archive.example.com/a.deb' a.deb ten SHA512:1"
        )),
        None
    );
    assert_eq!(
        download_bytes(&lines("'http://archive.example.com/a.deb a.deb 10")),
        None
    );
    let max = format!("'u' a.deb {} h\n'u' b.deb 1 h", u64::MAX);
    assert_eq!(download_bytes(&lines(&max)), None);
}
