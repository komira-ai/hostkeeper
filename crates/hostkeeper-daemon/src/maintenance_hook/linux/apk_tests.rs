//! The tests of apk's stage, over the fake host ([`super::super::testing`]) and the `apk version`
//! listing the maintenance look's tests use: each run's program, way, arguments and deadline, the
//! directory made and measured, the waits and the outcome, asserted by equality.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;

use super::*;
use crate::maintenance_hook::linux::stage::{
    REASON_BUSY, REASON_DOWNLOAD_FAILED, REASON_SPACE_SHORT,
};
use crate::maintenance_hook::linux::testing::{
    Call, FakeHost, Way, args, deferred, host_with, params, ran,
};

const VERSION_LIST: &str = include_str!("../../../tests/fixtures/maintenance/apk/version.txt");
const VERSION_NONE: &str = include_str!("../../../tests/fixtures/maintenance/apk/version_none.txt");
const APK: &str = "/sbin/apk";
const MIB: u64 = 1024 * 1024;
const KIB: u64 = 1024;
const LOCK_LINES: &str = "ERROR: Unable to lock database: Resource temporarily unavailable\nERROR: Failed to open apk database: Resource temporarily unavailable\n";

/// The packages the scope `all` makes due: every update listed but `hostkeeper`, in the
/// listing's order.
const ALL: [&str; 6] = [
    "busybox",
    "musl",
    "ssl_client",
    "linux-lts",
    "openssl",
    "libcrypto3",
];

/// What `apk info --size linux-lts` writes: the installed and the new version.
const KERNEL_SIZES: &str = "\
linux-lts-6.6.54-r0 installed size:
120 MiB

linux-lts-6.6.58-r0 installed size:
121 MiB

";

/// What `apk info --size` writes for every package of [`ALL`]: for some the new version alone,
/// for some the installed alone.
const ALL_SIZES: &str = "\
busybox-1.36.1-r31 installed size:
924 KiB

musl-1.2.5-r0 installed size:
383 KiB

ssl_client-1.36.1-r31 installed size:
26 KiB

linux-lts-6.6.58-r0 installed size:
121 MiB

openssl-3.3.2-r1 installed size:
1640 KiB

libcrypto3-3.3.2-r1 installed size:
4 MiB

";

fn update() -> Vec<String> {
    args(&["update", "-q"])
}

fn version() -> Vec<String> {
    args(&["version", "-l", "<"])
}

fn info(packages: &[&str]) -> Vec<String> {
    let mut list = args(&["info", "--size"]);
    list.extend(args(packages));
    list
}

fn fetch(packages: &[&str]) -> Vec<String> {
    let mut list = args(&[
        "fetch",
        "--quiet",
        "--output",
        "/var/cache/hostkeeper/maintenance/apk",
    ]);
    list.extend(args(packages));
    list
}

fn call(way: Way, args: Vec<String>, secs: u64) -> Call {
    Call {
        program: APK.into(),
        way,
        args,
        deadline: Duration::from_secs(secs),
    }
}

/// A fake host whose `apk` refreshes, lists [`VERSION_LIST`], sizes `packages` with `sizes` and
/// fetches them.
fn apk_host(packages: &[&str], sizes: &str) -> FakeHost {
    let fake = FakeHost::new();
    fake.always(&update(), ran(0, "", ""));
    fake.always(&version(), ran(0, VERSION_LIST, ""));
    fake.always(&info(packages), ran(0, sizes, ""));
    fake.always(&fetch(packages), ran(0, "", ""));
    fake
}

/// The kernel's estimate: its larger version, 121 MiB, one unit up for apk's rounding down.
const KERNEL_BYTES: u64 = 122 * MIB;

/// Proves: with room for exactly twice the download, the scope `security` fetches the kernel
/// alone (apk has no security origin), each run of `/sbin/apk` with exactly these arguments,
/// into hostkeeper's staging directory, which is made and measured.
///
/// Catches: a `security` stage that fetches every update, an `apk upgrade` in place of a fetch,
/// or a fetch into a directory that is not there.
#[test]
fn security_scope_fetches_the_kernel_alone() {
    let root = host_with("stage-apk-security", APK);
    let fake = apk_host(&["linux-lts"], KERNEL_SIZES);
    fake.free(Ok(2 * KERNEL_BYTES));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.calls(),
        vec![
            call(Way::Command, update(), 600),
            call(Way::Listing, version(), 600),
            call(Way::Listing, info(&["linux-lts"]), 600),
            call(Way::Command, fetch(&["linux-lts"]), 3600),
        ]
    );
    assert_eq!(fake.made(), vec![STAGE_DIR.to_owned()]);
    assert_eq!(
        fake.measured(),
        vec!["/var/cache/hostkeeper/maintenance/apk".to_owned()]
    );
}

/// Proves: the scope `all` sizes and fetches every update but `hostkeeper`, the estimate the
/// sum of each package's size whichever version apk printed it for.
///
/// Catches: hostkeeper's own package fetched, or a package whose new version apk did not size
/// left out of the estimate.
#[test]
fn all_scope_fetches_every_update_but_hostkeepers_own() {
    let root = host_with("stage-apk-all", APK);
    let fake = apk_host(&ALL, ALL_SIZES);
    let bytes = (925 + 384 + 27 + 1641) * KIB + 122 * MIB + 5 * MIB;
    fake.free(Ok(2 * bytes - 1));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::All));
    assert_eq!(
        outcome,
        deferred(
            REASON_SPACE_SHORT,
            &format!(
                "/var/cache/hostkeeper/maintenance/apk has {} bytes free; the download of {bytes} bytes needs {}",
                2 * bytes - 1,
                2 * bytes
            )
        )
    );
    fake.free(Ok(2 * bytes));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::All));
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        fake.arg_lists(),
        vec![
            update(),
            version(),
            info(&ALL),
            update(),
            version(),
            info(&ALL),
            fetch(&ALL)
        ]
    );
}

/// Proves: a fetch that fails defers `stage_download_failed` with apk's first error line.
///
/// Catches: a failed fetch taken as staged.
#[test]
fn a_failed_fetch_defers() {
    let root = host_with("stage-apk-fail", APK);
    let fake = apk_host(&["linux-lts"], KERNEL_SIZES);
    fake.always(
        &fetch(&["linux-lts"]),
        ran(1, "", "ERROR: linux-lts-6.6.58-r0: network error\n"),
    );
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_DOWNLOAD_FAILED,
            "apk fetch failed with exit code 1: ERROR: linux-lts-6.6.58-r0: network error"
        )
    );
}

/// Proves: apk's database lock held at the refresh is waited for through the injected waiter,
/// and the stage then succeeds; held through the whole bound, it defers `package_manager_busy`.
///
/// Catches: a held lock read as a failure, or a wait made by sleeping.
#[test]
fn a_held_lock_is_waited_for_through_the_waiter() {
    let root = host_with("stage-apk-lock", APK);
    let fake = apk_host(&["linux-lts"], KERNEL_SIZES);
    fake.then(&update(), ran(99, "", LOCK_LINES));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.waits(), vec![2]);

    let busy_host = apk_host(&["linux-lts"], KERNEL_SIZES);
    busy_host.always(&update(), ran(99, "", LOCK_LINES));
    let outcome = busy_host.stage(&root, 1).run(&params(WireScope::Security));
    assert_eq!(
        outcome,
        deferred(
            REASON_BUSY,
            "the package manager's lock was held for 1s of 1s (apk update)"
        )
    );
    assert_eq!(busy_host.arg_lists(), vec![update(), update()]);
}

/// Proves: with nothing listed, and with only hostkeeper's own package due, the stage succeeds
/// and sizes, makes, measures and fetches nothing.
///
/// Catches: an `apk fetch` with no package, or hostkeeper's own package fetched alone.
#[test]
fn nothing_due_fetches_nothing() {
    let root = host_with("stage-apk-nothing", APK);
    let fake = apk_host(&[], "");
    fake.always(&version(), ran(0, VERSION_NONE, ""));
    let outcome = fake.stage(&root, 300).run(&params(WireScope::All));
    assert_eq!(outcome, Ok(()));
    assert_eq!(fake.arg_lists(), vec![update(), version()]);

    let own_only = apk_host(&[], "");
    own_only.always(&version(), ran(0, "hostkeeper-1.8.0-r0 < 1.9.0-r0\n", ""));
    let outcome = own_only.stage(&root, 300).run(&params(WireScope::All));
    assert_eq!(outcome, Ok(()));
    assert_eq!(own_only.arg_lists(), vec![update(), version()]);
    assert_eq!(own_only.made(), Vec::<String>::new());
}

/// Proves: a package apk printed no size of, and a size listing with a line it does not know,
/// each defer `stage_listing_failed`, and nothing is fetched.
///
/// Catches: a package without a size counted as 0, so the estimate comes out too small.
#[test]
fn a_size_it_cannot_read_defers() {
    let cases = [
        (
            "linux-lts-6.6.40-r0 installed size:\n100 MiB\n",
            "apk info --size gave no size of linux-lts",
        ),
        (
            "linux-lts-6.6.58-r0 installed size:\nabout 121 MiB\n",
            "apk info --size wrote a line it does not know",
        ),
    ];
    for (sizes, detail) in cases {
        let root = host_with("stage-apk-unread", APK);
        let fake = apk_host(&["linux-lts"], sizes);
        let outcome = fake.stage(&root, 300).run(&params(WireScope::Security));
        assert_eq!(outcome, deferred(REASON_LISTING_FAILED, detail));
        assert_eq!(
            fake.arg_lists(),
            vec![update(), version(), info(&["linux-lts"])]
        );
    }
}

/// Proves: the size listing is read as headers each followed by its size; a size with no header,
/// two headers in a row and a header with no size after it are refused.
///
/// Catches: a size put on the wrong version, or a listing cut after a header read as whole.
#[test]
fn parse_apk_sizes_pairs_each_header_with_its_size() {
    let lines = |text: &str| -> Vec<String> { text.lines().map(str::to_owned).collect() };
    let sizes = parse_apk_sizes(&lines(KERNEL_SIZES)).expect("read");
    assert_eq!(
        sizes.into_iter().collect::<Vec<_>>(),
        vec![
            ("linux-lts-6.6.54-r0".to_owned(), 121 * MIB),
            ("linux-lts-6.6.58-r0".to_owned(), 122 * MIB),
        ]
    );
    assert_eq!(parse_apk_sizes(&lines("")), Some(BTreeMap::new()));
    assert_eq!(parse_apk_sizes(&lines("12 KiB\n")), None);
    assert_eq!(
        parse_apk_sizes(&lines(
            "a-1-r0 installed size:\nb-1-r0 installed size:\n1 B\n"
        )),
        None
    );
    assert_eq!(parse_apk_sizes(&lines("a-1-r0 installed size:\n")), None);
}

/// Proves: apk's sizes are whole numbers of `B` or a power of 1024, taken one unit up when the
/// unit is larger than a byte; any other text is refused.
///
/// Catches: a unit read as a power of 1000, or apk's rounding down passed on.
#[test]
fn parse_apk_size_reads_apks_sizes() {
    assert_eq!(parse_apk_size("512 B"), Some(512));
    assert_eq!(parse_apk_size("924 KiB"), Some(925 * KIB));
    assert_eq!(parse_apk_size("121 MiB"), Some(122 * MIB));
    assert_eq!(parse_apk_size("2 GiB"), Some(3 * 1024 * MIB));
    assert_eq!(parse_apk_size("1 TiB"), Some(2 * 1024 * 1024 * MIB));
    let past = format!("{} KiB", u64::MAX);
    for refused in ["", "12", "12 kB", "1.5 MiB", "x MiB", past.as_str()] {
        assert_eq!(parse_apk_size(refused), None, "{refused:?}");
    }
}

/// Proves: apk's lock is held only for a run that did not exit 0 and says `Unable to lock
/// database`.
///
/// Catches: any apk failure taken as busy.
#[test]
fn busy_is_a_failed_run_that_names_the_lock() {
    assert!(busy(Some(99), LOCK_LINES));
    assert!(!busy(Some(0), LOCK_LINES));
    assert!(!busy(Some(1), "ERROR: unable to select packages"));
}
