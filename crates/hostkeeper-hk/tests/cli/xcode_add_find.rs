//! `hk xcode add`: which archive it takes (design section 2.2), the missing `xcodes`, the release
//! waiting, and the lock that lets one run at a time.

use std::os::unix::fs::PermissionsExt as _;

use crate::add_rig::{Add, Store, archive_bytes, lists, puts, sha256, xip};

/// The one upload's body.
fn uploaded(server: &crate::rig::Server) -> Vec<u8> {
    let seen = puts(server);
    assert_eq!(seen.len(), 1, "{seen:#?}");
    seen[0].body.clone()
}

/// Proves: with no argument the run takes, among the files directly in `~/Downloads` named
/// `Xcode_<version>.xip` or `Xcode-<version>.xip`, the one for the highest version (numbers
/// compared part by part: 27.10 is above 27.9, and 27.2.0 is 27.2), the newest by modification
/// time among those of that version; and never a name with a tag (`_Release_Candidate`, `+build`,
/// a different case or suffix) or a file in a directory below. Runs no `xcodes`.
#[test]
fn no_argument_takes_the_highest_version_then_the_newest_file() {
    let add = Add::new("find-newest");
    let chosen = xip(", the chosen one");
    add.file("Xcode_27.9.xip", &xip(", 27.9"), 5000);
    add.file("Xcode-27.10.xip", &xip(", 27.10 old"), 100);
    add.file("Xcode_27.10.0.xip", &chosen, 200);
    add.file(
        "Xcode_27.10.1_Release_Candidate.xip",
        &xip(", tagged"),
        9000,
    );
    add.file("Xcode-27.10.1+27B52.xip", &xip(", build"), 9001);
    add.file("xcode_27.11.xip", &xip(", lower case"), 9002);
    add.file("Xcode_27.11.xip.part", &xip(", partial"), 9003);
    add.file("hk-xcode-push/27.12/Xcode-27.12.xip", &xip(", below"), 9004);
    std::fs::create_dir(add.downloads().join("Xcode_27.13.xip")).expect("a directory");
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(uploaded(&server), chosen);
    assert!(
        ran.stderr.contains(&format!(
            "Xcode_27.10.0.xip: the newest Xcode archive in {}\n",
            add.downloads().display()
        )),
        "{ran:?}"
    );
    assert_eq!(add.runs("xcodes"), 0);
}

/// Proves: given a version, the run takes the newest file named for it in `~/Downloads` or in
/// `~/Downloads/hk-xcode-push/<version>/`, where `27.2` is `27.2.0` and not `27.20` or `27.2.1`,
/// and runs no `xcodes`.
#[test]
fn a_version_finds_its_file_in_either_place() {
    let add = Add::new("find-version");
    let chosen = xip(", in the download directory");
    add.file("Xcode_27.20.xip", &xip(", 27.20"), 9000);
    add.file("Xcode_27.2.1.xip", &xip(", 27.2.1"), 9000);
    add.file("Xcode-27.2.xip", &xip(", old in downloads"), 100);
    add.file("hk-xcode-push/27.2/Xcode_27.2.0.xip", &chosen, 200);
    add.file("hk-xcode-push/27.2/Xcode_27.1.xip", &xip(", 27.1"), 9000);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &["27.2"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(uploaded(&server), chosen);
    assert!(
        ran.stderr
            .contains("Xcode_27.2.0.xip: the newest archive named for 27.2\n"),
        "{ran:?}"
    );
    assert_eq!(add.runs("xcodes"), 0);

    // The same name in `~/Downloads` itself, newer than the one below it.
    let newer = xip(", newer in downloads");
    add.file("Xcode-27.2.xip", &newer, 300);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &["27.2"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(uploaded(&server), newer);
}

/// Proves: given a version no file of `~/Downloads` is named for, the run runs `xcodes download
/// <version> --directory ~/Downloads/hk-xcode-push/<version>` once and takes the file it left
/// there; a file for another version is not taken.
#[test]
fn a_version_with_no_file_is_downloaded_once() {
    let add = Add::new("find-download");
    add.file("Xcode_27.1.xip", &xip(", 27.1"), 100);
    add.tool("xcodes.xip", &archive_bytes());
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &["27.2"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    let into = add.downloads().join("hk-xcode-push/27.2");
    assert_eq!(
        add.record("xcodes.args"),
        format!("download\t27.2\t--directory\t{}\n", into.display())
    );
    assert_eq!(uploaded(&server), archive_bytes());
    assert!(
        ran.stderr.contains("Xcode-27.2.xip: downloaded by "),
        "{ran:?}"
    );
}

/// Proves: a missing `xcodes` is a failure (exit 1) that names both ways out, installing it and
/// downloading the archive in a browser, then running `hk xcode add` again; and nothing is
/// uploaded.
#[test]
fn a_missing_xcodes_names_both_ways_out() {
    let mut add = Add::new("find-no-xcodes");
    add.mac.xcodes = add.rig.path("bin/no-such-xcodes");
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &["27.2"]);
    let xcodes = add.rig.path("bin/no-such-xcodes").display().to_string();
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(ran.stdout, "");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: {xcodes} is not installed, so Xcode 27.2 cannot be downloaded here; either \
             install it (brew install xcodesorg/made/xcodes), or download the archive in a \
             browser from Apple's developer downloads page \
             (https://developer.apple.com/download/all/) into ~/Downloads and run hk xcode add \
             27.2 again; nothing was uploaded\n"
        )),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
}

/// Proves: an `xcodes` that is found but cannot be run (a file that is not a program) is a failure
/// naming the program and the system's error, not the missing-program message.
#[test]
fn an_xcodes_that_cannot_run_is_not_called_missing() {
    let mut add = Add::new("find-bad-xcodes");
    let not_a_program = add.rig.put("bin/not-a-program", b"text", 0o644);
    add.mac.xcodes = not_a_program.clone().into();
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &["27.2"]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: cannot run {not_a_program} (Permission denied (os error 13)); nothing was \
             uploaded\n"
        )),
        "{ran:?}"
    );
}

/// Proves: with nothing given, no file in `~/Downloads` and no release waiting, the run says the
/// store holds the newest release (its version and build, from the first archive listed) and
/// exits 0 with nothing uploaded and no program run; with an empty store it says the store holds
/// none.
#[test]
fn nothing_given_and_nothing_waiting_exits_0() {
    let add = Add::new("find-nothing");
    let store = Store::default()
        .holding_sha(&"a".repeat(64), "27.2", "27B52")
        .holding_sha(&"b".repeat(64), "27.1", "27A10");
    let server = store.serve(&add.rig);
    let ran = add.add(&server, &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            0,
            "the store holds the newest release (27.2, 27B52); nothing to add\n"
        ),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
    assert_eq!(lists(&server).len(), 1);
    for tool in ["xcodes", "pkgutil", "xip"] {
        assert_eq!(add.runs(tool), 0, "{tool}");
    }
    let empty = Store::default().serve(&add.rig);
    let ran = add.add(&empty, &[]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            0,
            "the store holds no release and none is waiting; nothing to add\n"
        )
    );
}

/// Proves: with nothing given, no file in `~/Downloads` and a release waiting, the run goes on as
/// if that version were given: `xcodes download 27.3`, and the archive of 27.3 is uploaded; an
/// archive that turns out to be another version is `version_mismatch`.
#[test]
fn nothing_given_and_a_release_waiting_downloads_that_one() {
    let add = Add::new("find-waiting");
    add.tool("xcodes.xip", &archive_bytes());
    add.tool("xip.version", b"27.3\n");
    add.tool("xip.product-build", b"27C21\n");
    let server = Store::default().waiting("27.3", "27C21").serve(&add.rig);
    let ran = add.add(&server, &[]);
    assert_eq!(ran.code, 0, "{ran:?}");
    let into = add.downloads().join("hk-xcode-push/27.3");
    assert_eq!(
        add.record("xcodes.args"),
        format!("download\t27.3\t--directory\t{}\n", into.display())
    );
    assert_eq!(uploaded(&server), archive_bytes());
    let target = &puts(&server)[0].target;
    assert_eq!(
        target,
        &format!(
            "/v1/xcode/{}?version=27.3&build=27C21",
            sha256(&archive_bytes())
        )
    );
    assert!(
        ran.stdout
            .starts_with("stored: Xcode 27.3 (27C21), sha256 "),
        "{ran:?}"
    );

    // The record of the first run is not trusted for another release's name: remove it, so that
    // the app is read again.
    std::fs::remove_file(add.kept(&format!("{}.json", sha256(&archive_bytes()))))
        .expect("a record");
    add.tool("xip.version", b"27.4\n");
    let server = Store::default().waiting("27.3", "27C21").serve(&add.rig);
    let ran = add.add(&server, &[]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains("version_mismatch: asked 27.3, ")
            && ran
                .stderr
                .contains("Xcode-27.3.xip is Xcode 27.4 (27C21); nothing was uploaded\n"),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
}

/// Proves: a path given is taken whatever its name (`Xcode_27.3_Release_Candidate.xip`, a name
/// with a build), and the line says so.
#[test]
fn a_path_given_is_taken_whatever_its_name() {
    let add = Add::new("find-path");
    let path = add.file("Xcode_27.2_Release_Candidate.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(uploaded(&server), archive_bytes());
    assert!(
        ran.stderr
            .contains(&format!("hk: {path}: the file given\n")),
        "{ran:?}"
    );
}

/// Makes the lock file of a test's home as another run of `hk` would, holding `pid`.
fn hold(add: &Add, pid: &str) {
    let dir = add.rig.path("home/.hostkeeper/xcode");
    std::fs::create_dir_all(&dir).expect("directories");
    for d in [add.rig.path("home/.hostkeeper"), dir.clone()] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("a mode");
    }
    let lock = dir.join(".lock");
    std::fs::write(&lock, pid).expect("the lock file");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).expect("a mode");
}

/// Proves: a second run while a process that is alive holds the lock exits 1 at once, naming that
/// process id and the lock file, with no program run, nothing asked of the store and the lock file
/// as it was (never waiting).
#[test]
fn a_second_run_while_one_holds_the_lock_exits_1_naming_it() {
    let add = Add::new("lock-held");
    add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    // This test's own process is a process that is alive.
    let pid = std::process::id().to_string();
    hold(&add, &format!("{pid}\n"));
    let ran = add.add(&server, &[]);
    let lock = add.kept(".lock");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            1,
            "",
            format!(
                "hk: another hk xcode add is running (process id {pid}; it holds {}); wait for \
                 it to end, then run hk xcode add again\n",
                lock.display()
            )
            .as_str()
        )
    );
    assert!(server.seen().is_empty());
    for tool in ["xcodes", "pkgutil", "xip"] {
        assert_eq!(add.runs(tool), 0, "{tool}");
    }
    assert_eq!(
        std::fs::read_to_string(&lock).expect("kept"),
        format!("{pid}\n")
    );
}

/// Proves: a lock whose process is gone (a process id no system gives out), or that names none
/// (an empty file, a word), is stale: the run takes it over and goes on; the lock file holds this
/// run's process id and has mode 0600 while the run is in progress (as the store sees it asked),
/// and is gone when the run ends, so the next run finds nothing to take over.
#[test]
fn a_lock_whose_process_is_gone_is_taken_over() {
    for (n, stale) in ["2000000000\n", "", "not a process id", "-5"]
        .into_iter()
        .enumerate()
    {
        let add = Add::new(&format!("lock-stale-{n}"));
        add.file("Xcode_27.2.xip", &archive_bytes(), 100);
        hold(&add, stale);
        let (server, watched) = Store::default().serve_watching(&add.rig, &add.kept(".lock"));
        let ran = add.add(&server, &[]);
        assert_eq!(ran.code, 0, "{stale:?}: {ran:?}");
        let watched = watched.lock().expect("the record").clone();
        // Two `Xcode.List`, the query of the upload, the upload and the `Hosts.List` of the summary.
        assert_eq!(watched.len(), 5, "{stale:?}");
        for held in watched {
            assert_eq!(
                held,
                Some((format!("{}\n", std::process::id()), 0o600)),
                "{stale:?}"
            );
        }
        assert!(!add.kept(".lock").exists(), "{stale:?}");
        assert_eq!(add.add(&server, &[]).code, 0);
    }
}

/// Proves: the lock is let go when a run ends, however it ends: a run that stops at a check
/// leaves the next one able to run; and a lock file that is a link is refused (exit 2) naming it,
/// without following it.
#[test]
fn the_lock_is_let_go_when_a_run_ends_and_a_link_is_refused() {
    let add = Add::new("lock-link");
    let server = Store::default().serve(&add.rig);
    let bad = add.file("Xcode_27.2.xip", b"not an xip", 100);
    assert_eq!(add.add(&server, &[&bad]).code, 2);
    assert!(!add.kept(".lock").exists());
    let good = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    assert_eq!(add.add(&server, &[&good]).code, 0);
    let lock = add.kept(".lock");
    let elsewhere = add.rig.path("elsewhere");
    std::fs::write(&elsewhere, b"").expect("a file");
    std::os::unix::fs::symlink(&elsewhere, &lock).expect("a link");
    let ran = add.add(&server, &[&good]);
    assert_eq!(ran.code, 2, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: {} is a link; hk does not follow it\n",
            lock.display()
        )),
        "{ran:?}"
    );
}

/// Proves: a lock that cannot be made (the `xcode` directory cannot be written to) is a failure
/// (exit 1) naming the lock file and the system's reason, with nothing run.
#[test]
fn a_lock_that_cannot_be_made_is_a_failure() {
    let add = Add::new("lock-denied");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let dir = add.rig.path("home/.hostkeeper/xcode");
    std::fs::create_dir_all(&dir).expect("directories");
    std::fs::set_permissions(
        add.rig.path("home/.hostkeeper"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("a mode");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("a mode");
    let server = Store::default().serve(&add.rig);
    let ran = add.add(&server, &[&path]);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("a mode");
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(
        ran.stderr,
        format!(
            "hk: cannot use {}: Permission denied (os error 13)\n",
            dir.join(".lock").display()
        )
    );
    assert!(server.seen().is_empty());
}
