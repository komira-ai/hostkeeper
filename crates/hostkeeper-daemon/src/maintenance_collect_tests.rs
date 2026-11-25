//! The tests of `maintenance_collect`: the due rule of a look, a listing through a scripted
//! program, the reads of a host root over a scratch directory, the cuts of the texts and the
//! comparison of versions, each asserted by equality.

use std::os::unix::fs::symlink;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn own(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Proves a package of hostkeeper's own is never due, with or without an architecture suffix, and
/// that otherwise the scope decides: security keeps security updates and kernels, all keeps every
/// update.
#[test]
fn an_update_is_due_by_its_scope_and_never_for_an_own_package() {
    let own = own(&["hostkeeper"]);
    let security = Look {
        scope: LinuxScope::Security,
        own_packages: &own,
    };
    let all = Look {
        scope: LinuxScope::All,
        ..security
    };
    // (package, security, kernel, due under the security scope, due under all)
    let cases = [
        ("openssl", true, false, true, true),
        ("linux-image-6.8.0", false, true, true, true),
        ("curl", false, false, false, true),
        ("hostkeeper", true, true, false, false),
        ("hostkeeper:amd64", true, true, false, false),
        ("hostkeeper-extra", true, false, true, true),
        ("openssl:amd64", true, false, true, true),
    ];
    for (package, sec, kernel, under_security, under_all) in cases {
        assert_eq!(
            security.due(package, sec, kernel),
            under_security,
            "{package}"
        );
        assert_eq!(all.due(package, sec, kernel), under_all, "{package}");
    }
    assert_eq!(LinuxScope::default(), LinuxScope::Security);
}

/// Proves a listing gives the exit code and the standard output of a program that exited with a
/// code it accepts, of several accepted codes each as it was.
#[test]
fn a_listing_gives_the_output_of_an_accepted_exit() {
    let tool = FakeLaunchctl::new();
    tool.then(
        &args(&["check-update"]),
        Ok(LaunchctlOutput::success("a\n")),
    );
    tool.then(
        &args(&["check-update"]),
        Ok(LaunchctlOutput {
            code: Some(100),
            stdout: b"b\n".to_vec(),
            stderr: Vec::new(),
        }),
    );
    assert_eq!(
        listing(&tool, "dnf check-update", &["check-update"], &[0, 100]),
        Ok((0, "a\n".to_owned()))
    );
    assert_eq!(
        listing(&tool, "dnf check-update", &["check-update"], &[0, 100]),
        Ok((100, "b\n".to_owned()))
    );
}

/// Proves a listing that ended with a code it does not accept fails with the step, the code and
/// the first line of the standard error, never the arguments; a program ended by a signal, and
/// one that could not run or did not finish by its deadline, fail by their own words.
#[test]
fn a_listing_that_did_not_end_as_accepted_says_why() {
    let tool = FakeLaunchctl::new();
    let call = args(&["--secret", "list"]);
    tool.then(
        &call,
        Ok(LaunchctlOutput::exit(
            100,
            "E: first line\nE: second line\n",
        )),
    );
    tool.then(&call, Ok(LaunchctlOutput::exit(2, "")));
    tool.then(
        &call,
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
    );
    tool.then(
        &call,
        Err(LaunchctlError::Spawn {
            args: "--secret list".to_owned(),
            why: "no such file".to_owned(),
        }),
    );
    tool.then(
        &call,
        Err(LaunchctlError::TimedOut {
            args: "--secret list".to_owned(),
        }),
    );
    let reasons: Vec<String> = (0..5)
        .map(|_| {
            listing(&tool, "apt-get upgrade", &["--secret", "list"], &[0])
                .expect_err("a listing that failed")
        })
        .collect();
    assert_eq!(
        reasons,
        [
            "apt-get upgrade failed with exit code 100: E: first line",
            "apt-get upgrade failed with exit code 2",
            "apt-get upgrade was ended by a signal",
            "apt-get upgrade could not be run: no such file",
            "apt-get upgrade did not finish within its deadline",
        ]
    );
}

/// Proves a listing whose standard output reached the bound, which may have been cut, fails, and
/// one a byte under it does not.
#[test]
fn a_listing_cut_at_the_output_bound_fails() {
    let tool = FakeLaunchctl::new();
    tool.then(
        &args(&["list"]),
        Ok(LaunchctlOutput::success(&"x".repeat(OUTPUT_MAX - 1))),
    );
    tool.then(
        &args(&["list"]),
        Ok(LaunchctlOutput::success(&"x".repeat(OUTPUT_MAX))),
    );
    let first = listing(&tool, "apk version", &["list"], &[0]).expect("a listing under the bound");
    assert_eq!(first.1.len(), OUTPUT_MAX - 1);
    assert_eq!(
        listing(&tool, "apk version", &["list"], &[0]),
        Err("apk version wrote 65536 bytes or more, which may have been cut".to_owned())
    );
}

/// Proves the host's own root is `/`, and a root at a directory holds the host's absolute paths
/// under it, a leading slash or none.
#[test]
fn a_root_places_the_hosts_paths_under_it() {
    assert_eq!(
        HostRoot::host().path("/etc/os-release"),
        PathBuf::from("/etc/os-release")
    );
    let root = HostRoot::at(Path::new("/srv/fixture"));
    assert_eq!(
        root.path("/etc/os-release"),
        PathBuf::from("/srv/fixture/etc/os-release")
    );
    assert_eq!(root.path("etc"), PathBuf::from("/srv/fixture/etc"));
}

/// Proves a root reads a file as text, and a file that is not there fails with the host's path,
/// where `read_if_there` says `None` for it and still fails for a file it cannot read (a
/// directory).
#[test]
fn a_root_reads_files_and_tells_a_missing_one_from_an_unreadable_one() {
    let scratch = scratch("collect-read");
    std::fs::create_dir_all(scratch.path().join("etc/dir")).expect("a directory");
    std::fs::write(scratch.path().join("etc/os-release"), "ID=debian\n").expect("a file");
    let root = HostRoot::at(scratch.path());
    assert_eq!(root.read("/etc/os-release"), Ok("ID=debian\n".to_owned()));
    assert_eq!(
        root.read_if_there("/etc/os-release"),
        Ok(Some("ID=debian\n".to_owned()))
    );
    assert_eq!(
        root.read("/etc/missing"),
        Err("/etc/missing could not be read: No such file or directory (os error 2)".to_owned())
    );
    assert_eq!(root.read_if_there("/etc/missing"), Ok(None));
    assert_eq!(
        root.read_if_there("/etc/dir"),
        Err("/etc/dir could not be read: Is a directory (os error 21)".to_owned())
    );
}

/// Proves a root tells what is there: a file is, a directory is there and no file, a link that
/// dangles is there and no file, and nothing is neither.
#[test]
fn a_root_tells_what_is_there() {
    let scratch = scratch("collect-exists");
    std::fs::create_dir_all(scratch.path().join("usr/bin")).expect("a directory");
    std::fs::write(scratch.path().join("usr/bin/tool"), "").expect("a file");
    symlink("gone", scratch.path().join("usr/bin/dangling")).expect("a link");
    let root = HostRoot::at(scratch.path());
    assert!(root.exists("/usr/bin/tool") && root.is_file("/usr/bin/tool"));
    assert!(root.exists("/usr/bin") && !root.is_file("/usr/bin"));
    assert!(root.exists("/usr/bin/dangling") && !root.is_file("/usr/bin/dangling"));
    assert!(!root.exists("/usr/bin/none") && !root.is_file("/usr/bin/none"));
}

/// Proves the directories of a directory are its subdirectories alone (files are skipped), and a
/// directory that cannot be read fails with the host's path.
#[test]
fn a_root_lists_the_directories_of_a_directory() {
    let scratch = scratch("collect-dirs");
    let modules = scratch.path().join("lib/modules");
    std::fs::create_dir_all(modules.join("6.8.0-1")).expect("a directory");
    std::fs::create_dir_all(modules.join("6.8.0-2")).expect("a directory");
    std::fs::write(modules.join("modules.dep"), "").expect("a file");
    let root = HostRoot::at(scratch.path());
    let mut names = root.dirs_in("/lib/modules").expect("the directories");
    names.sort();
    assert_eq!(names, ["6.8.0-1", "6.8.0-2"]);
    assert_eq!(
        root.dirs_in("/lib/none"),
        Err("/lib/none could not be read: No such file or directory (os error 2)".to_owned())
    );
}

/// Proves a text is cut to its bound at a character's boundary: never inside a character, whole
/// when it fits, and to nothing when not even the first character fits.
#[test]
fn a_text_is_cut_at_a_character_boundary() {
    assert_eq!(clip("hello", 3), "hel");
    assert_eq!(clip("hello", 5), "hello");
    assert_eq!(clip("hello", 99), "hello");
    assert_eq!(clip("h\u{e9}llo", 2), "h");
    assert_eq!(clip("h\u{e9}llo", 3), "h\u{e9}");
    assert_eq!(clip("\u{e9}", 1), "");
    assert_eq!(clip("", 4), "");
}

/// Proves a due Linux item is a Linux item of the package and its versions, needing a reboot for
/// a kernel, each text cut to its bound; and a due reboot is a reboot item with no `to`, needing
/// one, its reason and its running kernel cut to theirs.
#[test]
fn the_due_items_are_made_with_their_kind_and_bounds() {
    assert_eq!(
        linux_item("openssl", "3.0.1", "3.0.2", true, false),
        pb::DueItem {
            kind: pb::MaintenanceKind::Linux.into(),
            from: "3.0.1".to_owned(),
            to: "3.0.2".to_owned(),
            needs_reboot: false,
            label: "openssl".to_owned(),
            security: true,
        }
    );
    assert_eq!(
        linux_item(
            &"p".repeat(70),
            &"f".repeat(40),
            &"t".repeat(40),
            false,
            true
        ),
        pb::DueItem {
            kind: pb::MaintenanceKind::Linux.into(),
            from: "f".repeat(32),
            to: "t".repeat(32),
            needs_reboot: true,
            label: "p".repeat(64),
            security: false,
        }
    );
    assert_eq!(
        reboot_item("kernel", "6.8.0-1"),
        pb::DueItem {
            kind: pb::MaintenanceKind::Reboot.into(),
            from: "6.8.0-1".to_owned(),
            to: String::new(),
            needs_reboot: true,
            label: "kernel".to_owned(),
            security: false,
        }
    );
    let long = reboot_item(&"r".repeat(70), &"k".repeat(40));
    assert_eq!((long.label, long.from), ("r".repeat(64), "k".repeat(32)));
}

/// Proves versions compare run by run: digits as numbers (`6.10` after `6.9`, leading zeros
/// ignored), other runs as bytes, digits before anything else, and a prefix before what extends it.
/// Catches: a text comparison that puts kernel `6.8.0-9` after `6.8.0-10`.
#[test]
fn versions_compare_by_runs_of_digits_and_of_text() {
    let cases = [
        ("6.10", "6.9", Ordering::Greater),
        ("6.9", "6.10", Ordering::Less),
        ("6.8.0-9", "6.8.0-10", Ordering::Less),
        ("6.8.0-1-generic", "6.8.0-1-generic", Ordering::Equal),
        ("1.01", "1.1", Ordering::Equal),
        ("", "", Ordering::Equal),
        ("1", "1.0", Ordering::Less),
        ("1.0", "1", Ordering::Greater),
        ("1", "a", Ordering::Less),
        ("a", "1", Ordering::Greater),
        ("1a", "1b", Ordering::Less),
        ("abd", "abc", Ordering::Greater),
    ];
    for (left, right, order) in cases {
        assert_eq!(
            compare_versions(left, right),
            order,
            "{left} against {right}"
        );
    }
}

/// Proves a host with no `/Applications` has no Xcode.
#[test]
fn a_host_without_applications_has_no_xcode() {
    let scratch = scratch("xcode-none");
    assert_eq!(xcode_apps(&HostRoot::at(scratch.path())), Ok(Vec::new()));
}

/// Proves the Xcodes are the directories named `Xcode*.app`, sorted: not another application, a
/// name without `.app`, a file with an Xcode's name, nor the staging directory.
#[test]
fn the_xcodes_are_the_xcode_app_directories_sorted() {
    let scratch = scratch("xcode-apps");
    let root = HostRoot::at(scratch.path());
    for path in [
        "/Applications/Xcode-beta.app",
        "/Applications/Xcode.app",
        "/Applications/Xcode-27.2.app",
        "/Applications/Safari.app",
        "/Applications/XcodeHelper",
        "/Applications/.hostkeeper-staging",
    ] {
        std::fs::create_dir_all(root.path(path)).expect("the directory");
    }
    std::fs::write(root.path("/Applications/Xcode-9.app"), "").expect("the file");
    let paths = xcode_apps(&root).map(|names| {
        names
            .iter()
            .map(|name| format!("{APPLICATIONS}/{name}"))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        paths,
        Ok(vec![
            "/Applications/Xcode-27.2.app".to_owned(),
            "/Applications/Xcode-beta.app".to_owned(),
            "/Applications/Xcode.app".to_owned(),
        ])
    );
}

/// Proves an `/Applications` that is there and cannot be listed (here a file) is the reason, not
/// an empty list.
#[test]
fn an_applications_that_cannot_be_listed_is_an_error() {
    let scratch = scratch("xcode-unlistable");
    std::fs::write(scratch.path().join("Applications"), "").expect("the file");
    let reason = io::Error::from_raw_os_error(libc::ENOTDIR);
    assert_eq!(
        xcode_apps(&HostRoot::at(scratch.path())),
        Err(format!("/Applications could not be read: {reason}"))
    );
}
