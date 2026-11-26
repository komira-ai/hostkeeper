//! `hk xcode add`, `hk xcode xcode-push` and `hk xcode verify` off macOS: the choice of the farm Mac
//! (`--mac`, the settings key `xcode_mac`, their usage errors, and `--mac` on macOS), and what
//! differs before the upload when no program of Apple's can read the archive: the version from
//! the name or the waiting release, the build from the waiting release, the record that gives
//! both, and a version that cannot be downloaded.

use std::os::unix::fs::PermissionsExt as _;

use serde_json::json;

use crate::add_rig::{Add, Store, TOOLS, archive_bytes, puts, sha256};
use crate::farm_rig::{
    Farm, MAC, RUN, SCHEDULE, Waits, asked, farm_calls, good_turn, linux, passing, probe_of,
    run_waiting,
};
use crate::rig::{Opts, Server};

/// The archive of the tests and a good check of it, behind a store that waits for 27.2.
fn serving(add: &Add, store: Store) -> Server {
    let sha = sha256(&archive_bytes());
    Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_000, &passing()))
        .serve(store, &add.rig)
}

/// The usage error of a verb off macOS with no Mac named.
fn missing(word: &str) -> String {
    format!(
        "hk: hk {word} checks the archive on a farm Mac, because this system is linux, which has \
         no pkgutil, xip or codesign of Apple's: name the Mac with --mac <node-id>, or with \
         xcode_mac in ~/.hostkeeper/hk.toml\n"
    )
}

/// Nothing was made, read, run or asked: no `~/.hostkeeper`, no program, no request.
fn nothing_happened(add: &Add, server: &Server) {
    assert!(!add.rig.path("home/.hostkeeper").exists());
    assert!(server.seen().is_empty());
    for tool in TOOLS {
        assert_eq!(add.runs(tool), 0, "{tool} ran");
    }
}

/// Writes the settings file with `text` under `~/.hostkeeper`, as the user's own with mode 0600.
fn settings_file(add: &Add, text: &str) {
    let dir = add.rig.path("home/.hostkeeper");
    std::fs::create_dir_all(dir.join("xcode")).expect("directories");
    for d in [dir.clone(), dir.join("xcode")] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("a mode");
    }
    std::fs::write(dir.join("hk.toml"), text).expect("settings");
    std::fs::set_permissions(dir.join("hk.toml"), std::fs::Permissions::from_mode(0o600))
        .expect("a mode");
}

/// Proves: off macOS, with neither `--mac` nor `xcode_mac`, `hk xcode add`, its second name
/// `hk xcode-push` and `hk xcode verify` are usage errors (exit 2) with the exact text that says the
/// archive is checked on a farm Mac and how to name it; nothing is created (no `~/.hostkeeper`),
/// nothing is run and the coordinator is not asked. The help is still printed.
#[test]
fn off_macos_with_no_mac_named_every_verb_says_how_to_name_one() {
    let add = linux("no-mac");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let cases: [(&[&str], &str, &str); 3] = [
        (&["xcode", "add"], "xcode add", ""),
        (
            &["xcode-push"],
            "xcode-push",
            "hk xcode-push is now hk xcode add\n",
        ),
        (&["xcode", "verify"], "xcode verify", ""),
    ];
    for (words, word, notice) in cases {
        let mut all = words.to_vec();
        all.push(&path);
        let ran = add.run(server.port, &all);
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (2, "", format!("{notice}{}", missing(word))),
            "{word}"
        );
        nothing_happened(&add, &server);
    }
    let help = add.run(server.port, &["xcode", "add", "--help"]);
    assert_eq!(help.code, 0, "{help:?}");
    assert!(
        help.stdout
            .starts_with("hk xcode add [--mac NODE-ID] [<version> | <archive.xip>]\n"),
        "{help:?}"
    );
}

/// Proves: on macOS `--mac` is a usage error (exit 2) saying the checks run on this Mac and that
/// `--mac` is for a system without macOS's programs, for the three verbs, before anything is read,
/// made or asked; and the settings key `xcode_mac` is ignored there: a run with it set goes on as
/// without it.
#[test]
fn on_macos_the_mac_flag_is_a_usage_error_and_the_key_is_ignored() {
    let add = Add::new("farm-on-macos");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default()
        .holding(&archive_bytes(), "27.2", "27B52")
        .serve(&add.rig);
    for (words, word) in [
        (&["xcode", "add"][..], "xcode add"),
        (&["xcode-push"][..], "xcode-push"),
        (&["xcode", "verify"][..], "xcode verify"),
    ] {
        let mut all = words.to_vec();
        all.extend(["--mac", MAC, &path]);
        let ran = add.run(server.port, &all);
        let notice = if word == "xcode-push" {
            "hk xcode-push is now hk xcode add\n"
        } else {
            ""
        };
        assert_eq!(
            (ran.code, ran.stdout.as_str(), ran.stderr),
            (
                2,
                "",
                format!(
                    "{notice}hk: hk {word} runs its checks on this Mac, so it takes no --mac: \
                     --mac names the farm Mac that checks the archive for a system without \
                     macOS's programs\n"
                )
            ),
            "{word}"
        );
    }
    assert!(server.seen().is_empty());
    assert!(!add.rig.path("home/.hostkeeper").exists());
    // The key is not read on macOS: a store that holds the archive ends the run as it does.
    settings_file(&add, &format!("xcode_mac = \"{MAC}\"\n"));
    let ran = add.run(server.port, &["xcode", "add", &path]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "already in the store: Xcode 27.2 (27B52)\n"),
        "{ran:?}"
    );
    assert!(farm_calls(&server).is_empty());
    assert!(!ran.stderr.contains("unknown key"), "{ran:?}");
}

/// Proves: `xcode_mac` of `~/.hostkeeper/hk.toml` names the farm Mac when no `--mac` is given (the
/// schedule asks for that host and the file's key is no unknown key), `--mac` overrides it, a key
/// that is not a string is a usage error naming the file, and an id the coordinator would not take
/// as a node id (a space, a leading or trailing dash, an upper-case letter, `_` or `.`, longer than
/// 63, empty) is a usage error naming where it came from, before anything is asked.
///
/// Catches: an id the coordinator refuses `invalid_request` after the upload, which `hk` would say
/// as a refusal of the archive.
#[test]
fn the_settings_key_names_the_mac_and_the_flag_overrides_it() {
    let add = linux("settings-key");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    settings_file(&add, "xcode_mac = \"mac-from-the-file\"\n");
    let server = serving(&add, Store::default().waiting("27.2", "27B52"));
    let ran = run_waiting(&add, &Waits::new(), server.port, &["xcode", "add", &path]);
    // The scripted answers are for the Mac of the other tests; the request is what is asserted.
    assert_eq!(
        asked(&server, SCHEDULE)[0]["hostIds"],
        json!(["mac-from-the-file"])
    );
    assert!(!ran.stderr.contains("unknown key"), "{ran:?}");
    let server = serving(
        &add,
        Store::default().holding(&archive_bytes(), "27.2", "27B52"),
    );
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", "mac-from-the-flag", &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(
        asked(&server, SCHEDULE)[0]["hostIds"],
        json!(["mac-from-the-flag"])
    );
    // Not a string.
    settings_file(&add, "xcode_mac = 5\n");
    let ran = add.run(server.port, &["xcode", "add", &path]);
    assert_eq!(ran.code, 2, "{ran:?}");
    assert!(
        ran.stderr
            .ends_with("hk.toml: xcode_mac must be a string\n"),
        "{ran:?}"
    );
    // Not a host id, from the file and from the flag.
    let rule = "is not a node id: 1 to 63 lower-case letters, digits or '-', not starting or \
                ending with '-'";
    let long = "m".repeat(64);
    for bad in [
        "bad mac",
        "-mac",
        "mac-",
        "",
        long.as_str(),
        "mac/1",
        "Mac-01",
        "mac_01",
        "mac.01",
    ] {
        settings_file(&add, &format!("xcode_mac = {bad:?}\n"));
        let ran = add.run(server.port, &["xcode", "add", &path]);
        assert_eq!(
            (ran.code, ran.stderr),
            (
                2,
                format!("hk: xcode_mac of ~/.hostkeeper/hk.toml: {bad:?} {rule}\n")
            ),
            "{bad:?}"
        );
        let ran = add.run(server.port, &["xcode", "add", "--mac", bad, &path]);
        assert_eq!(
            (ran.code, ran.stderr),
            (2, format!("hk: --mac: {bad:?} {rule}\n")),
            "{bad:?}"
        );
    }
}

/// Proves: `--mac` is taken once and needs a value, for `hk xcode add` and `hk xcode verify`.
#[test]
fn the_mac_flag_is_given_once_and_takes_a_value() {
    let add = linux("mac-twice");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    for word in ["add", "verify"] {
        let ran = add.run(
            server.port,
            &["xcode", word, "--mac", "a", "--mac", "b", &path],
        );
        assert_eq!(
            (ran.code, ran.stderr.as_str()),
            (2, "hk: --mac is given twice\n"),
            "{word}"
        );
        let ran = add.run(server.port, &["xcode", word, &path, "--mac"]);
        assert_eq!(
            (ran.code, ran.stderr.as_str()),
            (2, "hk: --mac needs a value\n"),
            "{word}"
        );
    }
    nothing_happened(&add, &server);
}

/// Proves: the version comes from the file's name (`Xcode-27.2.0.xip`) and the build from the
/// coordinator's waiting release of that version, whose own spelling of the version (`27.2`) is
/// the one uploaded; no program of Apple's runs, nothing is expanded, and `xcodes` is not run.
#[test]
fn the_version_is_the_names_and_the_build_the_waiting_releases() {
    let add = linux("name-version");
    let path = add.file("Xcode-27.2.0.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let server = serving(&add, Store::default().waiting("27.2", "27B52"));
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        puts(&server)[0].target,
        format!("/v1/xcode/{sha}?version=27.2&build=27B52")
    );
    for tool in TOOLS {
        assert_eq!(add.runs(tool), 0, "{tool} ran");
    }
    assert_eq!(
        asked(&server, SCHEDULE)[0]["reason"],
        format!(
            "hk xcode add: check Xcode 27.2 (27B52), sha256 {}",
            &sha[..12]
        )
    );
}

/// Proves: with no waiting release of the file's version (another version waits, or none does)
/// the run fails (exit 1, nothing uploaded, no check) saying the coordinator names no waiting
/// release of it and that the build cannot be read from the archive off macOS; and a name that
/// names no version fails saying the archive's name must be `Xcode_<version>.xip`.
#[test]
fn a_release_the_coordinator_does_not_wait_for_and_a_name_with_no_version_fail() {
    let add = linux("no-waiting");
    let sha = sha256(&archive_bytes());
    let path = add.file("Xcode_27.1.xip", &archive_bytes(), 100);
    let no_build = "hk: the coordinator names no waiting release of Xcode 27.1, and off macOS hk \
                    cannot read the build from the archive: the build is that of the release the \
                    daily list names and the store does not hold yet (hk xcode list shows it); \
                    nothing was uploaded\n";
    for store in [Store::default().waiting("27.2", "27B52"), Store::default()] {
        let server = serving(&add, store);
        let ran = run_waiting(
            &add,
            &Waits::new(),
            server.port,
            &["xcode", "add", "--mac", MAC, &path],
        );
        assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
        assert!(ran.stderr.ends_with(no_build), "{ran:?}");
        assert!(puts(&server).is_empty());
        assert!(farm_calls(&server).is_empty());
    }
    let path = add.file("release.xip", &archive_bytes(), 100);
    let server = serving(&add, Store::default().waiting("27.2", "27B52"));
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "hk: release.xip names no Xcode version, and off macOS hk cannot read it from the \
             archive: its name must be Xcode_<version>.xip (Xcode-<version>.xip too), as \
             Xcode_27.2.xip is; nothing was uploaded\n"
        ),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
    assert!(!add.kept(&format!("{sha}.json")).exists());
}

/// Proves: a version with no archive in `~/Downloads` is not downloaded off macOS: `xcodes` is
/// not run and the run fails (exit 1, nothing uploaded) telling to download
/// `Xcode_<version>.xip` from Apple's developer downloads page in a browser into `~/Downloads` and
/// to run `hk xcode add <version>` again; and the same when nothing is given, nothing is in
/// `~/Downloads` and the release waiting in the store is the one with no archive.
#[test]
fn a_version_with_no_archive_is_not_downloaded_off_macos() {
    let add = linux("no-download");
    let server = Store::default().waiting("27.2", "27B52").serve(&add.rig);
    for version in ["27.2", "27.5"] {
        let ran = add.run(server.port, &["xcode", "add", "--mac", MAC, version]);
        assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
        assert_eq!(
            ran.stderr,
            format!(
                "hk: Xcode {version} has no archive in ~/Downloads, and hk xcode add does not \
                 download off macOS (xcodes is a macOS program): download Xcode_{version}.xip \
                 from Apple's developer downloads page (https://developer.apple.com/download/all/) \
                 in a browser into ~/Downloads, and run hk xcode add {version} again; nothing was \
                 uploaded\n"
            )
        );
    }
    let ran = add.run(server.port, &["xcode", "add", "--mac", MAC]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: Xcode 27.2 has no archive in ~/Downloads, and hk xcode add does not"),
        "{ran:?}"
    );
    assert_eq!(add.runs("xcodes"), 0);
    assert!(puts(&server).is_empty());
    assert!(farm_calls(&server).is_empty());
    assert!(!add.downloads().join("hk-xcode-push").exists());
}

/// Proves: with nothing given, the newest archive in `~/Downloads` is taken off macOS as on a Mac,
/// its version from its name and its build from the waiting release.
#[test]
fn with_nothing_given_the_newest_archive_in_downloads_is_checked() {
    let add = linux("newest");
    add.file("Xcode_27.1.xip", b"xar!, the older archive", 100);
    add.file("Xcode_27.2.xip", &archive_bytes(), 200);
    let server = serving(&add, Store::default().waiting("27.2", "27B52"));
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr
            .contains("Xcode_27.2.xip: the newest Xcode archive in "),
        "{ran:?}"
    );
}

/// Writes a passing record of the archive of the tests as 27.2 (27B52) under `~/.hostkeeper/xcode`.
fn passing_record(add: &Add) {
    settings_file(add, "");
    let sha = sha256(&archive_bytes());
    let record = json!({
        "record": "hk-xcode-verify-v1",
        "archive": {"sha256": sha, "size": archive_bytes().len()},
        "expansion": {"version": "27.2", "build": "27B52"},
        "verdict": "pass",
    });
    let file = add.kept(&format!("{sha}.json"));
    std::fs::write(&file, record.to_string()).expect("a record");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("a mode");
}

/// Proves: a passing record of these bytes gives the version and the build off macOS, so a file
/// whose name has no version and a store that waits for no release are uploaded as the record says
/// and then checked; and a given version the record is not is `version_mismatch` (exit 1, nothing
/// uploaded).
#[test]
fn a_passing_record_gives_the_release_when_the_name_and_the_store_do_not() {
    let add = linux("record");
    passing_record(&add);
    let sha = sha256(&archive_bytes());
    let path = add.file("release.xip", &archive_bytes(), 100);
    let server = serving(&add, Store::default());
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: Xcode 27.2 (27B52), read from the record kept in {}; no program of Apple's runs \
             here\n",
            add.kept(&format!("{sha}.json")).display()
        )),
        "{ran:?}"
    );
    assert_eq!(
        puts(&server)[0].target,
        format!("/v1/xcode/{sha}?version=27.2&build=27B52")
    );
    // A version given that the record is not.
    add.file("Xcode_27.3.xip", &archive_bytes(), 300);
    let server = serving(&add, Store::default().waiting("27.3", "27C21"));
    let ran = add.run(server.port, &["xcode", "add", "--mac", MAC, "27.3"]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: version_mismatch: asked 27.3, {} is Xcode 27.2 (27B52); nothing was uploaded\n",
            add.downloads().join("Xcode_27.3.xip").display()
        )),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
}

/// Proves: the checks that need no program run off macOS as on a Mac: bytes held as another version
/// than the one asked are `version_mismatch` before any check, and the same release from other
/// bytes stops the run naming the `hk xcode remove` line, with nothing uploaded and nothing
/// scheduled.
#[test]
fn the_version_and_other_bytes_checks_still_stop_the_run() {
    let add = linux("stops");
    let path = add.file("Xcode_27.3.xip", &archive_bytes(), 100);
    let server = serving(
        &add,
        Store::default().holding(&archive_bytes(), "27.2", "27B52"),
    );
    let ran = add.run(server.port, &["xcode", "add", "--mac", MAC, "27.3"]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "hk: version_mismatch: asked 27.3, these bytes are Xcode 27.2 (27B52) in the store; \
             nothing was uploaded\n"
        ),
        "{ran:?}"
    );
    let other = Store::default()
        .holding_sha(&"0".repeat(64), "27.3", "27C21")
        .waiting("27.3", "27C21");
    let server = serving(&add, other);
    let ran = add.run(server.port, &["xcode", "add", "--mac", MAC, &path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: the store holds Xcode 27.3 (27C21) from other bytes (sha256 {}); if these are \
             the ones to keep, hk xcode remove --sha256 {} --reason <why> first; nothing was \
             uploaded\n",
            "0".repeat(64),
            "0".repeat(64)
        )),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty() && farm_calls(&server).is_empty());
}

/// Proves: off macOS a global flag that is not a token file is refused as on a Mac, `hk xcode
/// verify` without the words about `xcodes`, and the Mac's settings are read: the endpoint, the CA
/// and the token are required (exit 2).
#[test]
fn the_target_is_required_off_macos_as_on_a_mac() {
    let add = linux("target");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let ran = add.bare(&["xcode", "add", "--mac", MAC, &path]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: hk xcode add needs --endpoint, --ca-file and --token-file\n"
        )
    );
    let ran = add.bare(&["xcode", "verify", "--mac", MAC, &path]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: hk xcode verify needs --endpoint, --ca-file and --token-file\n"
        )
    );
    let server = Store::default().serve(&add.rig);
    let ran = add.run(
        server.port,
        &["xcode", "verify", "--mac", MAC, "--token-stdin", &path],
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: hk xcode verify takes --token-file only: --token-stdin, --github-oidc-audience \
             and --response-file are not for it\n"
        )
    );
    let ran = add.run_with(
        Opts::real().without_home(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            1,
            "hk: hk xcode add keeps its records and its lock under $HOME, which is not set\n"
        )
    );
}

/// Proves: the second name goes through the same run: `hk xcode-push --mac <mac> <archive>` says
/// `hk xcode-push is now hk xcode add` on stderr first, then uploads, checks on the farm Mac and
/// prints the check line, as `hk xcode add` does.
#[test]
fn the_second_name_takes_the_mac_flag_as_add_does() {
    let add = linux("push-mac");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = serving(&add, Store::default().waiting("27.2", "27B52"));
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode-push", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr
            .starts_with("hk xcode-push is now hk xcode add\n"),
        "{ran:?}"
    );
    assert!(
        ran.stdout
            .ends_with(&format!("checked on {MAC} (run {RUN}): pass\n")),
        "{ran:?}"
    );
}

/// Proves: after a pass the line `checked on <mac> (run <id>): pass` comes before the summary of the
/// Macs that are due the release, which is read after the check: stdout is `stored:`, the check
/// line, then the summary's lines naming the Mac the coordinator knows.
#[test]
fn the_check_line_comes_before_the_summary_of_the_macs() {
    use crate::summary_rig::{Fleet, group, mac, preview};
    let add = linux("summary-order");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let fleet = Fleet {
        hosts: vec![vec![mac("mac-01", "g", &[])]],
        groups: vec![vec![group("g", 2, None, None)]],
        ..Fleet::default()
    }
    .with_preview("g", preview(&["mac-01"], &[], None));
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_000, &passing()));
    let store = Store::default().waiting("27.2", "27B52");
    let server = farm.serve_over(store, &add.rig, move |request| fleet.answer(request));
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    let lines: Vec<&str> = ran.stdout.lines().collect();
    assert!(
        lines[0].starts_with("stored: Xcode 27.2 (27B52), sha256 "),
        "{ran:?}"
    );
    assert_eq!(lines[1], format!("checked on {MAC} (run {RUN}): pass"));
    assert!(
        lines.len() > 2 && lines[2..].iter().any(|l| l.contains("mac-01")),
        "{ran:?}"
    );
}
