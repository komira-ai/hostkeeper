//! Tests of `remove` and `retire`.

use super::*;

/// Catches: a changed file the receipt lists being left, a file the receipt does not list being deleted, or the receipt not being forgotten.
#[test]
fn remove_deletes_every_file_the_receipt_lists_changed_or_not_and_forgets_the_receipt() {
    let i = installed("rm-all");
    let rig = &i.rig;
    rig.write(&home("svc"), b"changed since the install", 0o755);
    rig.write(&home("data/user.db"), b"user data", 0o600);
    rig.write(&home("cache.bin"), b"made by a script", 0o644);
    rig.mac.clear_calls();
    rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    for gone in ["svc", "VERSION", "old/legacy"] {
        assert!(!rig.exists(&home(gone)), "{gone}");
    }
    assert!(
        !rig.exists("/Library/LaunchDaemons/svc.plist") && !rig.exists("/Library/LaunchDaemons")
    );
    assert!(!rig.exists(&home("old")), "a created directory left empty");
    assert_eq!(rig.read_text(&home("data/user.db")), "user data");
    assert_eq!(rig.read_text(&home("cache.bin")), "made by a script");
    assert!(
        rig.exists("/Library/Application Support"),
        "its child holds files the receipt does not list"
    );
    assert!(rig.mac.receipts().is_empty());
    assert_eq!(rig.mac.runs_of(Tool::Pkgutil, "--forget"), 1);
    assert!(rig.mac.calls_of(Tool::Installer).is_empty());
}

/// Catches: a directory that was on the host before the install being deleted, a created one left behind, or a second `remove` failing.
#[test]
fn remove_of_a_clean_install_leaves_the_host_as_it_was_and_never_a_directory_that_was_there() {
    let rig = Rig::new("rm-clean");
    rig.mkdir("/Library", 0o755);
    rig.mac.add(v1());
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.stage(&order, "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    rig.pkg().remove(&order, &[&dir]).unwrap();
    assert_eq!(rig.paths(), ["/Library"]);
    assert!(rig.mac.receipts().is_empty());
    // Run again, it finds nothing left to do.
    rig.mac.clear_calls();
    rig.pkg().remove(&order, &[&dir]).unwrap();
    assert_eq!(rig.mac.runs_of(Tool::Pkgutil, "--forget"), 0);
}

/// Catches: a file that another package's receipt also lists being deleted.
#[test]
fn remove_leaves_a_file_a_receipt_of_another_identifier_also_lists() {
    let i = installed("rm-shared");
    let rig = &i.rig;
    rig.mac.set_receipt(
        "com.other.pkg",
        "3",
        &["Library/Application Support/svc/VERSION"],
    );
    rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    assert_eq!(rig.read_text(&home("VERSION")), "1.0");
    assert!(!rig.exists(&home("svc")));
    assert!(rig.mac.receipt("com.other.pkg").is_some());
}

/// Catches: a file being deleted through a link that replaced its directory.
#[test]
fn remove_deletes_nothing_through_a_linked_parent_and_no_link_target() {
    let i = installed("rm-link");
    let rig = &i.rig;
    let outside = rig.root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("legacy"), b"not ours").unwrap();
    fs::remove_file(rig.real(&home("old/legacy"))).unwrap();
    fs::remove_dir(rig.real(&home("old"))).unwrap();
    symlink(&outside, rig.real(&home("old"))).unwrap();
    rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    assert_eq!(fs::read(outside.join("legacy")).unwrap(), b"not ours");
    assert!(
        fs::symlink_metadata(rig.real(&home("old")))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!rig.exists(&home("svc")));
}

/// Catches: a link the receipt lists being left behind.
#[test]
fn remove_deletes_a_link_of_the_payload_itself() {
    let rig = Rig::new("rm-own-link");
    rig.mac
        .add(v1().put(&home("current"), Content::Link("svc".into()), 0o755));
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.manual_dir("1.0", "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    assert!(rig.exists(&home("current")));
    rig.pkg().remove(&order, &[&dir]).unwrap();
    assert!(rig.snapshot().is_empty());
}

/// Catches: a file only the previous `Bom` lists, or a file of a hand-installed receipt with no record, being left.
#[test]
fn remove_deletes_the_files_of_both_kept_versions_and_of_a_hand_install_without_a_record() {
    let (i, dir2, order2) = Installed::updated("rm-both");
    i.rig.write(&home("old/legacy"), b"back again", 0o644);
    i.rig.pkg().remove(&order2, &[&dir2, &i.dir1]).unwrap();
    assert!(
        !i.rig.exists(&home("old/legacy")),
        "a file only the previous Bom lists"
    );
    assert!(
        i.rig.exists(&home("old")),
        "a directory hostkeeper did not create"
    );

    let rig = Rig::new("rm-hand");
    let mut hand = v1().file("legacy-09", "x", 0o644);
    hand.name = "svc-0.9".into();
    rig.mac.add(hand);
    rig.mac.install_by_hand("svc-0.9");
    rig.pkg().remove(&rig.order("0.9"), &[]).unwrap();
    assert!(!rig.exists(&home("legacy-09")) && !rig.exists(&home("svc")));
    assert!(rig.mac.receipts().is_empty());
}

/// Catches: a missing receipt stopping the deletion of the kept `Bom`'s files, or `--forget` being run for no receipt.
#[test]
fn remove_after_a_hand_forget_still_deletes_what_a_kept_bom_lists() {
    let i = installed("rm-forgotten");
    i.rig.mac.forget(IDENTIFIER);
    i.rig.mac.clear_calls();
    i.rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    assert!(i.rig.snapshot().is_empty());
    assert_eq!(i.rig.mac.runs_of(Tool::Pkgutil, "--forget"), 0);
}

/// Catches: a failing `pkgutil`, a listing that may be cut, or an unreadable record or kept tree being taken as nothing to remove.
#[test]
fn remove_reports_io_error_when_a_listing_a_tool_or_the_records_fail() {
    type Rigged = (&'static str, Fault);
    let cases: Vec<(&str, &str, Rigged)> = vec![
        ("receipt", "--pkg-info-plist", ("spawn", Fault::Spawn)),
        (
            "files exit",
            "--only-files",
            ("exit", Fault::Output(ToolOutput::exit(1, "boom"))),
        ),
        (
            "files cut",
            "--only-files",
            (
                "cut",
                Fault::Output(ToolOutput::success(&"x".repeat(OUTPUT_MAX))),
            ),
        ),
        (
            "file-info",
            "--file-info-plist",
            ("exit", Fault::Output(ToolOutput::exit(1, "boom"))),
        ),
        (
            "forget",
            "--forget",
            ("exit", Fault::Output(ToolOutput::exit(1, "boom"))),
        ),
    ];
    for (what, verb, (_, fault)) in cases {
        let i = installed("rm-io");
        i.rig.mac.fault(Tool::Pkgutil, &[verb], fault);
        assert_eq!(
            reason(i.rig.pkg().remove(&i.order1, &[&i.dir1])),
            Reason::IoError,
            "{what}"
        );
    }
    let i = installed("rm-io-records");
    i.rig
        .records
        .fail_load
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        reason(i.rig.pkg().remove(&i.order1, &[&i.dir1])),
        Reason::IoError
    );
    let empty = i.rig.packages.join("empty");
    fs::create_dir(&empty).unwrap();
    assert_eq!(
        reason(i.rig.pkg().remove(&i.order1, &[&empty])),
        Reason::IoError,
        "no kept tree"
    );
}

/// Catches: a retired version leaving its directory or seal, touching another version, or running `installer` or `pkgutil`.
#[test]
fn retire_deletes_a_version_directory_and_its_seal_and_runs_no_tool() {
    let i = installed("retire");
    let (dir2, _) = i.stage_v2();
    let seal = PathBuf::from(format!("{}.seal", dir2.display()));
    fs::write(&seal, b"seal").unwrap();
    for entry in ["tree", "tree/Library"] {
        fs::set_permissions(dir2.join(entry), fs::Permissions::from_mode(0o555)).unwrap();
    }
    i.rig.mac.clear_calls();
    i.rig.pkg().retire(&dir2).unwrap();
    assert!(!dir2.exists() && !seal.exists());
    assert!(i.dir1.exists(), "another version is untouched");
    i.rig.pkg().retire(&dir2).unwrap();
    assert!(i.rig.mac.calls().is_empty(), "no installer, no pkgutil");
}

/// Catches: a recorded directory below a link that replaced its parent being deleted, or the
/// link's target being emptied.
#[test]
fn remove_touches_nothing_below_a_service_directory_that_became_a_link() {
    let i = installed("rm-home-link");
    let rig = &i.rig;
    let outside = rig.root.join("outside");
    fs::rename(rig.real(HOME), &outside).unwrap();
    symlink(&outside, rig.real(HOME)).unwrap();
    rig.pkg().remove(&i.order1, &[&i.dir1]).unwrap();
    assert_eq!(
        fs::read_to_string(outside.join("svc")).unwrap(),
        "program svc 1.0"
    );
    assert_eq!(
        fs::read_to_string(outside.join("old/legacy")).unwrap(),
        "legacy"
    );
    assert!(
        fs::symlink_metadata(rig.real(HOME))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        !rig.exists("/Library/LaunchDaemons"),
        "its own plists and directory were removed"
    );
}
