//! The tests of dnf's stage, over the fake host ([`super::super::testing`]) and what dnf 4's
//! `upgrade --assumeno` writes: each run's program, way, arguments and deadline, the directory
//! measured, the waits and the outcome, asserted by equality.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;

use super::*;
use crate::maintenance_hook::linux::stage::{
    REASON_BUSY, REASON_DOWNLOAD_FAILED, REASON_LISTING_FAILED, REASON_SPACE_SHORT,
};
use crate::maintenance_hook::linux::testing::{
    Call, FakeHost, Way, args, deferred, host_with, params, ran,
};

const DNF: &str = "/usr/bin/dnf";

/// What `dnf upgrade --assumeno` writes for two packages, 3.6 MiB to download (3774874 bytes,
/// rounded up), before it answers no.
const ESTIMATE: &str = "\
Dependencies resolved.
================================================================================
 Package           Arch       Version                Repository           Size
================================================================================
Upgrading:
 openssl           x86_64     1:3.2.2-6.el9_5        baseos              1.4 M
 openssl-libs      x86_64     1:3.2.2-6.el9_5        baseos              2.2 M

Transaction Summary
================================================================================
Upgrade  2 Packages

Total download size: 3.6 M
Operation aborted.
";

/// What it writes with nothing to upgrade, exit 0.
const NOTHING: &str = "\
Dependencies resolved.
Nothing to do.
Complete!
";

/// The options of both runs under `security`, hostkeeper's own packages excluded by name.
const SECURITY_OPTIONS: [&str; 4] = [
    "--setopt=exit_on_lock=True",
    "--security",
    "--exclude=hostkeeper",
    "--exclude=nginx",
];

/// The options under `all`.
const ALL_OPTIONS: [&str; 3] = [
    "--setopt=exit_on_lock=True",
    "--exclude=hostkeeper",
    "--exclude=nginx",
];

fn estimate(options: &[&str]) -> Vec<String> {
    let mut list = args(&["upgrade", "--assumeno"]);
    list.extend(args(options));
    list
}

fn download(options: &[&str]) -> Vec<String> {
    let mut list = args(&["upgrade", "--downloadonly", "-y", "-q"]);
    list.extend(args(options));
    list
}

fn call(way: Way, args: Vec<String>, secs: u64) -> Call {
    Call {
        program: DNF.into(),
        way,
        args,
        deadline: Duration::from_secs(secs),
    }
}

/// A fake host whose `dnf` estimates [`ESTIMATE`] (exit 1, as `--assumeno` exits) and downloads,
/// both with `options`.
fn dnf_host(options: &[&str]) -> FakeHost {
    let fake = FakeHost::new();
    fake.always(&estimate(options), ran(1, ESTIMATE, ""));
    fake.always(&download(options), ran(0, "", ""));
    fake
}

/// Proves: with room for exactly twice the download, the scope `security` estimates and then
/// downloads alone with `--security`, the lock option and an `--exclude` for each of
/// hostkeeper's own packages, each run of `/usr/bin/dnf` with exactly these arguments; the
/// download lands in dnf's cache, which is measured.
///
/// Catches: an `upgrade` without `--downloadonly`, hostkeeper's own packages left in the
/// transaction, or a dnf left to wait for its lock with no bound.
#[test]
fn security_scope_downloads_with_security_and_the_own_packages_excluded() {
    let root = host_with("stage-dnf-security", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.free(Ok(7_549_748));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.calls(),
        vec![
            call(Way::Listing, estimate(&SECURITY_OPTIONS), 600),
            call(Way::Command, download(&SECURITY_OPTIONS), 3600),
        ]
    );
    assert_eq!(fake.made(), vec![CACHE.to_owned()]);
    assert_eq!(fake.measured(), vec!["/var/cache/dnf".to_owned()]);
    assert_eq!(fake.waits(), Vec::<u64>::new());
}

/// Proves: the scope `all` runs both without `--security`, hostkeeper's own packages still
/// excluded.
///
/// Catches: `all` read as `security`, or the exclusion tied to the scope.
#[test]
fn all_scope_downloads_every_update_but_hostkeepers_own() {
    let root = host_with("stage-dnf-all", DNF);
    let fake = dnf_host(&ALL_OPTIONS);
    let outcome = fake.stage(&root, 300).run(&params(WireScope::All));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.arg_lists(),
        vec![estimate(&ALL_OPTIONS), download(&ALL_OPTIONS)]
    );
}

/// Proves: one byte short of twice the download, the stage defers `free_space_short` and runs no
/// download.
///
/// Catches: a download onto a disk it would fill.
#[test]
fn short_space_defers_and_runs_no_download() {
    let root = host_with("stage-dnf-short", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.free(Ok(7_549_747));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_SPACE_SHORT,
            "/var/cache/dnf has 7549747 bytes free; the download of 3774874 bytes needs 7549748"
        )
    );
    assert_eq!(fake.arg_lists(), vec![estimate(&SECURITY_OPTIONS)]);
}

/// Proves: a download that fails defers `stage_download_failed` with dnf's first error line.
///
/// Catches: a failed download taken as staged.
#[test]
fn a_failed_download_defers() {
    let root = host_with("stage-dnf-fail", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.always(
        &download(&SECURITY_OPTIONS),
        ran(1, "", "Error: Failed to download packages\n"),
    );
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_DOWNLOAD_FAILED,
            "dnf upgrade --downloadonly failed with exit code 1: Error: Failed to download packages"
        )
    );
}

/// Proves: dnf's lock held at the estimate (exit 200, twice) and at the download (once) is
/// waited for through the injected waiter, and the stage then succeeds.
///
/// Catches: exit 200 read as a failure, or a wait made by sleeping.
#[test]
fn a_held_lock_is_waited_for_through_the_waiter() {
    let root = host_with("stage-dnf-lock", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.then(&estimate(&SECURITY_OPTIONS), ran(LOCK_HELD, "", ""));
    fake.then(&estimate(&SECURITY_OPTIONS), ran(LOCK_HELD, "", ""));
    fake.then(&download(&SECURITY_OPTIONS), ran(200, "", "Error: locked"));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.waits(), vec![2, 2, 2]);
    assert_eq!(fake.arg_lists().len(), 5);
}

/// Proves: a lock held through the whole bound defers `package_manager_busy`.
///
/// Catches: a wait past `--package-lock-wait-secs`.
#[test]
fn a_lock_held_past_the_bound_defers_busy() {
    let root = host_with("stage-dnf-busy", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.always(&estimate(&SECURITY_OPTIONS), ran(LOCK_HELD, "", ""));
    let outcome = fake.stage(&root, 2).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_BUSY,
            "the package manager's lock was held for 2s of 2s (dnf upgrade --assumeno)"
        )
    );
    assert_eq!(fake.waits(), vec![2]);
}

/// Proves: with nothing to upgrade (exit 0, no total) the stage succeeds, measuring and
/// downloading nothing.
///
/// Catches: "Nothing to do." read as a failure.
#[test]
fn nothing_to_upgrade_downloads_nothing() {
    let root = host_with("stage-dnf-nothing", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.always(&estimate(&SECURITY_OPTIONS), ran(0, NOTHING, ""));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.arg_lists(), vec![estimate(&SECURITY_OPTIONS)]);
    assert_eq!(fake.made(), Vec::<String>::new());
}

/// Proves: a total of 0 (every package already in dnf's cache) succeeds with no download.
///
/// Catches: a download run for nothing.
#[test]
fn everything_downloaded_downloads_nothing() {
    let root = host_with("stage-dnf-cached", DNF);
    let fake = dnf_host(&SECURITY_OPTIONS);
    fake.always(
        &estimate(&SECURITY_OPTIONS),
        ran(1, "Total size: 3.6 M\nTotal download size: 0\n", ""),
    );
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.arg_lists(), vec![estimate(&SECURITY_OPTIONS)]);
}

/// Proves: an estimate that exits 1 without a total (a repository that failed), one that exits
/// with another code, and a total it cannot read each defer `stage_listing_failed`, and nothing
/// is downloaded.
///
/// Catches: a failed resolve read as nothing to do, or an unreadable total read as 0.
#[test]
fn an_estimate_it_cannot_read_defers() {
    let cases = [
        (
            ran(
                1,
                "Error: Failed to download metadata for repo 'baseos'\n",
                "",
            ),
            "dnf upgrade --assumeno exited 1 and printed no download size",
        ),
        (
            ran(3, "", ""),
            "dnf upgrade --assumeno failed with exit code 3",
        ),
        (
            ran(1, "Total download size: 3.6 Q\n", ""),
            "dnf upgrade --assumeno printed a size it does not know",
        ),
    ];
    for (answer, detail) in cases {
        let root = host_with("stage-dnf-unread", DNF);
        let fake = dnf_host(&SECURITY_OPTIONS);
        fake.always(&estimate(&SECURITY_OPTIONS), answer);
        let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
        assert_eq!(outcome, deferred(REASON_LISTING_FAILED, detail));
        assert_eq!(fake.arg_lists(), vec![estimate(&SECURITY_OPTIONS)]);
    }
}

/// Proves: dnf's sizes are read as `format_number` prints them, powers of 1024, rounded up to a
/// byte; any other unit or number is refused.
///
/// Catches: a unit read as a power of 1000, a fraction dropped, or a size it cannot read taken
/// as some number.
#[test]
fn parse_size_reads_dnfs_sizes() {
    assert_eq!(parse_size("512"), Some(512));
    assert_eq!(parse_size("0"), Some(0));
    assert_eq!(parse_size("345 k"), Some(345 * 1024));
    assert_eq!(parse_size("3.6 M"), Some(3_774_874));
    assert_eq!(parse_size("12 G"), Some(12 * 1024 * 1024 * 1024));
    assert_eq!(parse_size("1.5 T"), Some(1_649_267_441_664));
    for refused in ["", "M", "3.6 Q", "3.6M", "x k", "1.2.3 M", "-1 k"] {
        assert_eq!(parse_size(refused), None, "{refused:?}");
    }
}

/// Proves: dnf's lock is held for exit 200 alone, whatever was written.
///
/// Catches: a failed resolve (exit 1) waited for as a held lock.
#[test]
fn busy_is_exit_200() {
    assert!(busy(Some(200), ""));
    assert!(!busy(Some(1), "Error: locked"));
    assert!(!busy(None, ""));
}
