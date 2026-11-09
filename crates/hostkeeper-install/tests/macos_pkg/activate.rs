//! Tests of `activate`: a first install, an update, a rollback, a resume and the failures.

use super::*;

fn expected_dirs(extra: &[&str]) -> std::collections::BTreeSet<String> {
    let mut dirs: std::collections::BTreeSet<String> = [
        "/Library",
        "/Library/Application Support",
        "/Library/LaunchDaemons",
        HOME,
    ]
    .iter()
    .map(|d| (*d).to_owned())
    .collect();
    dirs.extend(extra.iter().map(|d| (*d).to_owned()));
    dirs
}

fn record_of(version: &str, dirs: std::collections::BTreeSet<String>) -> PkgRecord {
    let mut record = PkgRecord::new(IDENTIFIER, version);
    record.created_dirs = dirs;
    record
}

/// Catches: an install that runs without `-allowUntrusted`, without the proof of the receipt, without the pending record written first, or that leaves a `run` label not enabled.
#[test]
fn activate_of_a_first_install_installs_proves_enables_and_records() {
    let rig = Rig::new("act-first");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.mac.clear_calls();
    assert_eq!(
        rig.activate(&order, &dir, None).unwrap(),
        Activated { drift: None }
    );
    let id = IDENTIFIER.to_owned();
    let call = |tool, args: &[&str]| Call {
        tool,
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    };
    let artifact = dir.join("artifact").to_str().unwrap().to_owned();
    assert_eq!(
        rig.mac.calls(),
        vec![
            call(Tool::Pkgutil, &["--pkg-info-plist", &id]),
            call(
                Tool::Installer,
                &["-allowUntrusted", "-pkg", &artifact, "-target", "/"]
            ),
            call(Tool::Pkgutil, &["--pkg-info-plist", &id]),
            call(Tool::Launchctl, &["enable", "system/svc"]),
            call(Tool::Launchctl, &["enable", "system/svc.worker"]),
        ]
    );
    assert_eq!(rig.read_text(&home("svc")), "program svc 1.0");
    assert_eq!(rig.read_text(&home("old/legacy")), "legacy");
    assert_eq!(rig.mac.receipt(IDENTIFIER).unwrap().version, "1.0");
    let dirs = expected_dirs(&[&home("old")]);
    assert_eq!(
        rig.records.get(SERVICE),
        Some(record_of("1.0", dirs.clone()))
    );
    let saves = rig.records.saves();
    assert_eq!(saves.len(), 2);
    assert!(saves[0].pending && saves[0].version == "1.0" && saves[0].created_dirs == dirs);
    assert!(!saves[1].pending);
    assert_eq!(
        (rig.mac.enabled("svc"), rig.mac.enabled("svc.worker")),
        (Some(true), Some(true))
    );
    assert!(
        rig.lock.try_hold().unwrap().is_some(),
        "the lock is released"
    );
}

/// Catches: a directory that was on the host before the first install being recorded as created, and so deleted by `remove`.
#[test]
fn activate_records_only_the_directories_that_were_not_on_the_host() {
    let rig = Rig::new("act-created");
    rig.mkdir("/Library", 0o755);
    rig.mkdir("/Library/Application Support", 0o755);
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    let dirs = ["/Library/LaunchDaemons", HOME, &home("old")]
        .iter()
        .map(|d| (*d).to_owned())
        .collect();
    assert_eq!(rig.records.get(SERVICE).unwrap().created_dirs, dirs);
}

/// Catches: an update that leaves a file only the old `Bom` lists, deletes a file no `Bom` lists (the service's data), or does not disable a dropped label.
#[test]
fn an_update_deletes_the_files_the_old_bom_listed_and_the_new_one_does_not_and_no_other() {
    let i = installed("act-update");
    let rig = &i.rig;
    rig.write(&home("data/user.db"), b"user data", 0o600);
    rig.write(&home("cache.bin"), b"made by a script", 0o644);
    let (dir2, order2) = i.stage_v2();
    rig.mac.clear_calls();
    assert_eq!(
        rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap(),
        Activated { drift: None }
    );
    assert_eq!(rig.read_text(&home("svc")), "program svc 2.0");
    assert_eq!(rig.read_text(&home("lib/x")), "lib v2");
    assert!(
        !rig.exists(&home("old/legacy")),
        "a file of 1.0 that 2.0 does not list"
    );
    assert!(!rig.exists(&home("old")), "the directory it left empty");
    assert!(!rig.exists("/Library/LaunchDaemons/svc.worker.plist"));
    assert_eq!(rig.read_text(&home("data/user.db")), "user data");
    assert_eq!(rig.read_text(&home("cache.bin")), "made by a script");
    assert_eq!(rig.mac.receipt(IDENTIFIER).unwrap().version, "2.0");
    assert_eq!(
        rig.records.get(SERVICE),
        Some(record_of("2.0", expected_dirs(&[&home("lib")])))
    );
    assert_eq!(
        rig.mac.calls_of(Tool::Launchctl),
        [["enable", "system/svc"], ["disable", "system/svc.worker"]]
    );
    assert_eq!(
        (rig.mac.enabled("svc"), rig.mac.enabled("svc.worker")),
        (Some(true), Some(false))
    );
    assert_eq!(
        rig.mac.runs_of(Tool::Pkgutil, "--file-info-plist"),
        3,
        "each payload file already on the host"
    );
    assert_eq!(rig.mac.calls_of(Tool::Installer).len(), 1);
}

/// Catches: a recorded directory being deleted, or dropped from the record, while it still holds a file `installer` did not put there.
#[test]
fn an_update_keeps_a_recorded_directory_that_still_holds_a_file_it_did_not_install() {
    let i = installed("act-update-keeps-dir");
    i.rig.write(&home("old/user.txt"), b"mine", 0o644);
    let (dir2, order2) = i.stage_v2();
    i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap();
    assert!(!i.rig.exists(&home("old/legacy")));
    assert_eq!(i.rig.read_text(&home("old/user.txt")), "mine");
    assert!(
        i.rig
            .records
            .get(SERVICE)
            .unwrap()
            .created_dirs
            .contains(&home("old"))
    );
}

/// Catches: an update deleting a file of the old version through a link that now stands where its directory was.
#[test]
fn an_update_deletes_nothing_through_a_directory_that_became_a_link() {
    let i = installed("act-update-link");
    let outside = i.rig.root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("legacy"), b"not ours").unwrap();
    fs::remove_file(i.rig.real(&home("old/legacy"))).unwrap();
    fs::remove_dir(i.rig.real(&home("old"))).unwrap();
    symlink(&outside, i.rig.real(&home("old"))).unwrap();
    let (dir2, order2) = i.stage_v2();
    i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap();
    assert_eq!(fs::read(outside.join("legacy")).unwrap(), b"not ours");
}

/// Catches: a second `activate` of the same version changing the files, the record, the receipt or the report.
#[test]
fn activate_twice_is_the_same_as_once() {
    let (i, dir2, order2) = Installed::updated("act-twice");
    let rig = &i.rig;
    let (files, record, receipts) = (rig.snapshot(), rig.records.get(SERVICE), rig.mac.receipts());
    assert_eq!(
        rig.activate(&order2, &dir2, None).unwrap(),
        Activated { drift: None }
    );
    assert_eq!(rig.snapshot(), files);
    assert_eq!(rig.records.get(SERVICE), record);
    assert_eq!(rig.mac.receipts(), receipts);
    assert_eq!(rig.mac.enabled("svc.worker"), Some(false));
}

/// Catches: a pending record being reported as drift, or a resumed `activate` leaving the files of the previous version.
#[test]
fn an_activate_cut_short_after_installer_resumes_and_finishes_the_deletions() {
    let i = installed("act-resume");
    let (dir2, order2) = i.stage_v2();
    let mut pending = record_of("2.0", expected_dirs(&[&home("old"), &home("lib")]));
    pending.pending = true;
    i.rig.records.set(SERVICE, pending);
    i.rig.mac.install_by_hand("svc-2.0");
    assert!(
        i.rig.exists(&home("old/legacy")),
        "1.0's file is still there"
    );
    assert_eq!(
        i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap(),
        Activated { drift: None }
    );
    assert!(!i.rig.exists(&home("old")));
    assert_eq!(
        i.rig.records.get(SERVICE),
        Some(record_of("2.0", expected_dirs(&[&home("lib")])))
    );
}

/// Catches: a rollback that checks the signature again, expands the package again, or leaves the newer version's files or directories.
#[test]
fn a_rollback_installs_the_kept_package_without_a_fetch_or_a_signature_check() {
    let (mut i, dir2, _) = Installed::updated("act-rollback");
    let rig = &mut i.rig;
    rig.write(&home("data/user.db"), b"user data", 0o600);
    let before = rig.records.saves()[1].clone();
    // The signer is taken out of the file, and the Mac would no longer accept either package.
    rig.signers = Signers::new(vec![Fingerprint::parse(&fp("7e")).unwrap()]).unwrap();
    for name in ["svc-1.0", "svc-2.0"] {
        rig.mac
            .edit(name, |p| p.check = (1, "Status: no signature\n".into()));
    }
    rig.mac.clear_calls();
    assert_eq!(
        rig.activate(&i.order1, &i.dir1, Some(&dir2)).unwrap(),
        Activated { drift: None }
    );
    assert_eq!(rig.mac.runs_of(Tool::Pkgutil, "--check-signature"), 0);
    assert_eq!(rig.mac.runs_of(Tool::Pkgutil, "--expand-full"), 0);
    assert!(rig.mac.calls_of(Tool::Lsbom).is_empty());
    let artifact = i.dir1.join("artifact").to_str().unwrap().to_owned();
    assert_eq!(
        rig.mac.calls_of(Tool::Installer),
        [["-allowUntrusted", "-pkg", &artifact, "-target", "/"]]
    );
    assert!(
        only_verbs(rig, Tool::Pkgutil)
            .iter()
            .all(|v| v == "--pkg-info-plist" || v == "--file-info-plist")
    );
    assert_eq!(rig.read_text(&home("svc")), "program svc 1.0");
    assert_eq!(rig.read_text(&home("old/legacy")), "legacy");
    assert!(!rig.exists(&home("lib/x")), "a file only 2.0 listed");
    assert!(!rig.exists(&home("lib")), "a directory only 2.0 created");
    assert_eq!(rig.read_text(&home("data/user.db")), "user data");
    assert_eq!(rig.mac.receipt(IDENTIFIER).unwrap().version, "1.0");
    assert_eq!(
        rig.records.get(SERVICE),
        Some(record_of("1.0", before.created_dirs))
    );
    assert_eq!(rig.mac.enabled("svc.worker"), Some(true));
}

/// Catches: an activate over a file another package's receipt lists installing anything.
#[test]
fn activate_refuses_a_payload_file_a_receipt_of_another_identifier_lists() {
    let rig = Rig::new("act-conflict");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.write(&home("svc"), b"theirs", 0o755);
    rig.mac.set_receipt(
        "com.other.pkg",
        "7",
        &["Library/Application Support/svc/svc"],
    );
    let (files, receipts) = (rig.snapshot(), rig.mac.receipts());
    rig.mac.clear_calls();
    let err = rig.activate(&order, &dir, None).unwrap_err();
    assert_eq!(err.reason(), Reason::PackageConflict);
    assert_eq!(
        err.detail(),
        format!("{} is a file of the package com.other.pkg", home("svc"))
    );
    no_install_ran(&rig);
    assert_eq!(rig.mac.runs_of(Tool::Pkgutil, "--pkg-info-plist"), 1);
    assert_eq!(rig.snapshot(), files);
    assert_eq!(rig.mac.receipts(), receipts);
    assert!(rig.records.all().is_empty(), "no record was written");
}

/// Catches: a file no receipt lists, or a file of the same identifier's receipt, being refused as `package_conflict`, or a hand install being installed over without a drift report.
#[test]
fn activate_installs_over_a_file_no_receipt_lists_and_over_the_same_identifiers_hand_install() {
    let rig = Rig::new("act-over");
    rig.write(&home("VERSION"), b"unknown origin", 0o644);
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    assert_eq!(
        rig.activate(&order, &dir, None).unwrap(),
        Activated { drift: None }
    );
    assert_eq!(rig.read_text(&home("VERSION")), "1.0");

    // The same identifier installed by hand at another version, with a file 1.0 does not list.
    let rig = Rig::new("act-over-hand");
    rig.mac
        .add(FakePkg::standard(SERVICE, IDENTIFIER, "0.9").file("legacy-09", "x", 0o644));
    rig.mac.install_by_hand("svc-0.9");
    rig.mac.add(v1());
    let dir = rig.stage(&order, "svc-1.0");
    let done = rig.activate(&order, &dir, None).unwrap();
    assert_eq!(
        done,
        Activated {
            drift: Some(Some("0.9".into()))
        }
    );
    assert!(!rig.exists(&home("legacy-09")));
    assert_eq!(rig.read_text(&home("VERSION")), "1.0");
    assert!(
        !rig.records.get(SERVICE).unwrap().created_dirs.is_empty(),
        "old/ is new"
    );
    assert!(
        !rig.records
            .get(SERVICE)
            .unwrap()
            .created_dirs
            .contains(&home("")),
        "svc/ was there"
    );
}

/// Catches: an install into a parent directory that another account owns or can write, or
/// that is a link or no directory, going ahead.
#[test]
fn activate_refuses_an_unsafe_parent_directory_and_installs_nothing() {
    type Break = fn(&Rig);
    let cases: Vec<(&str, Break)> = vec![
        ("world-writable", |r| r.mkdir("/Library", 0o777)),
        ("writable by a group but admin", |r| {
            r.mkdir("/Library", 0o775)
        }),
        ("a link", |r| {
            fs::create_dir(r.root.join("elsewhere")).unwrap();
            symlink(r.root.join("elsewhere"), r.real("/Library")).unwrap();
        }),
        ("a file", |r| {
            r.mkdir("/Library", 0o755);
            r.write("/Library/LaunchDaemons", b"x", 0o644);
        }),
    ];
    for (what, break_it) in cases {
        let rig = Rig::new("act-unsafe");
        rig.mac.add(v1());
        let order = rig.order_run("1.0", &BOTH);
        let dir = rig.stage(&order, "svc-1.0");
        break_it(&rig);
        let files = rig.snapshot();
        rig.mac.clear_calls();
        let err = rig.activate(&order, &dir, None).unwrap_err();
        assert_eq!(err.reason(), Reason::UnsafeDirectory, "{what}: {err}");
        assert_eq!(err.reason().class(), Class::Environment);
        no_install_ran(&rig);
        assert_eq!(rig.snapshot(), files, "{what}");
        assert!(rig.records.all().is_empty() && rig.mac.receipts().is_empty());
    }

    // A parent that another account owns: the host sees the scratch user as not root.
    let rig = Rig::new("act-unsafe-owner");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.mkdir("/Library", 0o755);
    let stranger = rig.host_with(Ids {
        root: rig.euid.wrapping_add(1),
        ..rig.ids
    });
    let result = rig.pkg_on(&stranger).activate(&order, &dir, None);
    assert_eq!(reason(result), Reason::UnsafeDirectory);
    no_install_ran(&rig);
    assert!(rig.mac.receipts().is_empty());
}

/// Catches: a parent directory that the group `admin` may write being refused as `unsafe_directory`.
#[test]
fn activate_accepts_a_parent_directory_only_the_admin_group_can_also_write() {
    let rig = Rig::new("act-admin");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.mkdir("/Library", 0o775);
    let admin = rig.host_with(Ids {
        root: rig.euid,
        wheel: rig.egid.wrapping_add(1),
        admin: rig.egid,
    });
    rig.pkg_on(&admin).activate(&order, &dir, None).unwrap();
}

/// Catches: a busy installer, in either case of its text, being reported as `installer_error` (a failed order) instead of a deferred one.
#[test]
fn a_busy_installer_defers_the_order_and_changes_nothing() {
    for (stdout, stderr) in [
        ("", "installer: Another install is already in progress.\n"),
        ("The Installer Is Busy\n", ""),
    ] {
        let rig = Rig::new("act-busy");
        let busy = Install::Fails {
            code: 1,
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        rig.mac.add(v1().with_install(busy));
        let order = rig.order_run("1.0", &BOTH);
        let dir = rig.stage(&order, "svc-1.0");
        let err = rig.activate(&order, &dir, None).unwrap_err();
        assert_eq!(err.reason(), Reason::PackageManagerBusy);
        assert_eq!(err.reason().outcome(), Outcome::Deferred);
        assert_eq!(err.reason().class(), Class::Environment);
        assert!(rig.snapshot().is_empty() && rig.mac.receipts().is_empty());
        assert!(rig.mac.calls_of(Tool::Launchctl).is_empty());
    }
}

fn other_bytes() -> Install {
    Install::OtherBytes {
        path: home("svc"),
        bytes: b"tampered".to_vec(),
    }
}

fn failing_installs() -> Vec<(&'static str, Install)> {
    vec![
        (
            "a non-zero exit",
            Install::Fails {
                code: 1,
                stdout: String::new(),
                stderr: "installer: Package name is svc\ninstaller: The install failed.\n\n".into(),
            },
        ),
        ("a version not shown after", Install::KeepsOldReceipt),
        ("an installed file that differs", other_bytes()),
    ]
}

/// Catches: a failed `installer` (non-zero exit, no version in the receipt, a file that is not
/// the sealed one) leaving the host changed after the core's rollback to the previous version.
#[test]
fn an_installer_error_on_an_update_is_followed_by_a_rollback_that_ends_as_before() {
    for (what, install) in failing_installs() {
        let i = installed("act-update-fails");
        let rig = &i.rig;
        rig.write(&home("data/user.db"), b"user data", 0o600);
        rig.mac.add(v2().with_install(install));
        let order2 = rig.order("2.0");
        let dir2 = rig.stage(&order2, "svc-2.0");
        let (files, receipts, record) =
            (rig.snapshot(), rig.mac.receipts(), rig.records.get(SERVICE));
        let err = rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap_err();
        assert_eq!(err.reason(), Reason::InstallerError, "{what}: {err}");
        assert_eq!(err.reason().class(), Class::Release);
        assert!(
            rig.records.get(SERVICE).unwrap().pending,
            "{what}: the record says an install was cut short"
        );
        if what == "a non-zero exit" {
            assert_eq!(
                err.detail(),
                "installer exited Some(1): installer: The install failed."
            );
        }
        assert_eq!(
            rig.activate(&i.order1, &i.dir1, Some(&dir2)).unwrap(),
            Activated { drift: None },
            "{what}"
        );
        assert_eq!(rig.snapshot(), files, "{what}");
        assert_eq!(rig.mac.receipts(), receipts, "{what}");
        assert_eq!(rig.records.get(SERVICE), record, "{what}");
        assert_eq!(rig.mac.enabled("svc.worker"), Some(true));
    }
}

/// Catches: a failed first install leaving files, directories or a receipt after the core's
/// removal.
#[test]
fn an_installer_error_on_a_first_install_is_followed_by_a_removal_that_ends_as_before() {
    for (what, install) in failing_installs() {
        let rig = Rig::new("act-first-fails");
        rig.mac.add(v1().with_install(install));
        let order = rig.order_run("1.0", &BOTH);
        let dir = rig.stage(&order, "svc-1.0");
        assert_eq!(
            reason(rig.activate(&order, &dir, None)),
            Reason::InstallerError,
            "{what}"
        );
        rig.pkg().remove(&order, &[&dir]).unwrap();
        assert!(rig.snapshot().is_empty(), "{what}: {:?}", rig.paths());
        assert!(rig.mac.receipts().is_empty(), "{what}");
    }
}

/// Catches: an `installer` that cannot run or times out being taken for a success, a failing `launchctl` ignored, or a receipt read that fails reported as a missing receipt.
#[test]
fn a_tool_that_cannot_run_or_fails_during_activate_is_reported_with_its_reason() {
    let installer_spawn = (
        Tool::Installer,
        "-allowUntrusted",
        Fault::Spawn,
        Reason::InstallerError,
    );
    let installer_time = (
        Tool::Installer,
        "-allowUntrusted",
        Fault::TimedOut,
        Reason::InstallerError,
    );
    let launchctl_exit = (
        Tool::Launchctl,
        "enable",
        Fault::Output(ToolOutput::exit(1, "no such service")),
        Reason::InstallerError,
    );
    let launchctl_spawn = (Tool::Launchctl, "enable", Fault::Spawn, Reason::IoError);
    let receipt_spawn = (
        Tool::Pkgutil,
        "--pkg-info-plist",
        Fault::Spawn,
        Reason::IoError,
    );
    let receipt_exit = (
        Tool::Pkgutil,
        "--pkg-info-plist",
        Fault::Output(ToolOutput::exit(2, "boom")),
        Reason::IoError,
    );
    let receipt_junk = (
        Tool::Pkgutil,
        "--pkg-info-plist",
        Fault::Output(ToolOutput::success("junk")),
        Reason::IoError,
    );
    for (tool, verb, fault, expected) in [
        installer_spawn,
        installer_time,
        launchctl_exit,
        launchctl_spawn,
        receipt_spawn,
        receipt_exit,
        receipt_junk,
    ] {
        let rig = Rig::new("act-tool-fails");
        rig.mac.add(v1());
        let order = rig.order_run("1.0", &BOTH);
        let dir = rig.stage(&order, "svc-1.0");
        rig.mac.fault(tool, &[verb], fault);
        assert_eq!(reason(rig.activate(&order, &dir, None)), expected, "{verb}");
    }
    // The receipt read after `installer` fails to run.
    let rig = Rig::new("act-tool-fails-prove");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.mac
        .fault_nth(Tool::Pkgutil, &["--pkg-info-plist"], 1, 1, Fault::Spawn);
    assert_eq!(reason(rig.activate(&order, &dir, None)), Reason::IoError);
}

/// Catches: a `--file-info-plist` that fails or prints no property list being read as "no receipt lists the file".
#[test]
fn a_file_info_that_fails_or_is_unreadable_during_activate_is_io_error() {
    for answer in [ToolOutput::exit(1, "boom"), ToolOutput::success("junk")] {
        let i = installed("act-file-info");
        let (dir2, order2) = i.stage_v2();
        i.rig
            .mac
            .fault(Tool::Pkgutil, &["--file-info-plist"], Fault::Output(answer));
        assert_eq!(
            reason(i.rig.activate(&order2, &dir2, Some(&i.dir1))),
            Reason::IoError
        );
        no_install_ran_after(&i);
    }
}

fn no_install_ran_after(i: &Installed) {
    assert_eq!(
        i.rig.mac.calls_of(Tool::Installer).len(),
        1,
        "only the first install ran installer"
    );
}

/// Catches: a version directory without a tree, a record store that fails to load or to save, or a lock that cannot be taken, going on to install.
#[test]
fn activate_reports_io_error_for_a_missing_tree_a_failing_record_store_or_lock() {
    let rig = Rig::new("act-io");
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    let empty = rig.packages.join("empty");
    fs::create_dir(&empty).unwrap();
    assert_eq!(
        reason(rig.activate(&order, &empty, None)),
        Reason::IoError,
        "no staged tree"
    );
    assert_eq!(
        reason(rig.activate(&order, &dir, Some(&empty))),
        Reason::IoError,
        "no kept tree"
    );

    rig.records
        .fail_load
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        reason(rig.activate(&order, &dir, None)),
        Reason::IoError,
        "record load"
    );
    rig.records
        .fail_load
        .store(false, std::sync::atomic::Ordering::SeqCst);
    rig.records.fail_save_nth(0);
    assert_eq!(
        reason(rig.activate(&order, &dir, None)),
        Reason::IoError,
        "pending save"
    );
    assert_eq!(rig.mac.calls_of(Tool::Installer).len(), 0);
    rig.records.fail_save_nth(1);
    assert_eq!(
        reason(rig.activate(&order, &dir, None)),
        Reason::IoError,
        "done save"
    );

    let mut rig = Rig::new("act-io-lock");
    rig.mac.add(v1());
    let dir = rig.stage(&order, "svc-1.0");
    rig.lock = hostkeeper_install::lock::InstallerLock::new(&rig.root.join("missing/dir/lock"));
    assert_eq!(
        reason(rig.activate(&order, &dir, None)),
        Reason::IoError,
        "lock"
    );
}
