//! Tests of the observer's drift check and of the repair of each drift by the next update.

use super::*;

fn observe_now(i: &Installed, seen: &mut Seen) -> Option<Drift> {
    let record = i.rig.records.get(SERVICE);
    observe(
        &i.rig.host,
        IDENTIFIER,
        record.as_ref(),
        Some(&i.dir1),
        seen,
    )
    .unwrap()
}

/// Catches: an intact installation reported as drift, or a file whose size, mode and time are unchanged being hashed at every heartbeat.
#[test]
fn an_unchanged_installation_is_no_drift_and_its_files_are_not_hashed_again() {
    let i = installed("drift-none");
    let mut seen = Seen::default();
    assert_eq!(observe_now(&i, &mut seen), None);
    assert_eq!(
        seen.hashed().len(),
        5,
        "every file once: {:?}",
        seen.hashed()
    );
    assert_eq!(observe_now(&i, &mut seen), None);
    assert!(
        seen.hashed().is_empty(),
        "size, mode and time are unchanged"
    );
}

/// Catches: a receipt at a version other than the record's going unreported.
#[test]
fn a_receipt_at_another_version_is_drift() {
    let i = installed("drift-version");
    let files: Vec<&str> = vec!["Library/Application Support/svc/svc"];
    i.rig.mac.set_receipt(IDENTIFIER, "0.9", &files);
    let drift = observe_now(&i, &mut Seen::default()).unwrap();
    assert_eq!(
        drift,
        Drift {
            found: Some("0.9".into()),
            recorded: Some("1.0".into()),
            files: vec![]
        }
    );
}

/// Catches: a package installed by hand before hostkeeper's first install going unreported.
#[test]
fn a_receipt_where_no_record_names_the_package_is_drift() {
    let i = installed("drift-no-record");
    let drift = observe(&i.rig.host, IDENTIFIER, None, None, &mut Seen::default()).unwrap();
    assert_eq!(
        drift,
        Some(Drift {
            found: Some("1.0".into()),
            recorded: None,
            files: vec![]
        })
    );
}

/// Catches: a receipt forgotten by hand going unreported.
#[test]
fn a_record_with_no_receipt_is_drift() {
    let i = installed("drift-no-receipt");
    i.rig.mac.forget(IDENTIFIER);
    let drift = observe_now(&i, &mut Seen::default()).unwrap();
    assert_eq!(
        drift,
        Drift {
            found: None,
            recorded: Some("1.0".into()),
            files: vec![]
        }
    );
}

/// Catches: a missing or changed payload file going unreported, a file seen to differ being forgotten while it is unchanged, or a restored file still reported.
#[test]
fn a_deleted_payload_file_and_a_file_with_other_bytes_are_drift_and_stay_drift_until_repaired() {
    let i = installed("drift-files");
    let mut seen = Seen::default();
    assert_eq!(observe_now(&i, &mut seen), None);
    i.rig.remove_file(&home("VERSION"));
    i.rig
        .write(&home("svc"), b"tampered program, longer", 0o755);
    let drift = observe_now(&i, &mut seen).unwrap();
    assert_eq!(
        (drift.found.clone(), drift.recorded.clone()),
        (Some("1.0".into()), Some("1.0".into()))
    );
    assert_eq!(drift.files, [home("VERSION"), home("svc")]);
    assert_eq!(
        seen.hashed(),
        [home("svc")],
        "the deleted file needs no hash, the others are unchanged"
    );
    assert_eq!(observe_now(&i, &mut seen).unwrap().files.len(), 2);
    assert!(
        seen.hashed().is_empty(),
        "a file seen to differ is not hashed again while it is unchanged"
    );
    i.rig.write(&home("VERSION"), b"1.0", 0o644);
    i.rig.write(&home("svc"), b"program svc 1.0", 0o755);
    assert_eq!(observe_now(&i, &mut seen), None);
    assert_eq!(seen.hashed().len(), 2);
}

/// Catches: a changed mode not triggering a new hash, or a file with the same bytes under a new mode reported as drift.
#[test]
fn a_file_whose_mode_changed_is_hashed_again_and_found_intact() {
    let i = installed("drift-mode");
    let mut seen = Seen::default();
    observe_now(&i, &mut seen);
    fs::set_permissions(i.rig.real(&home("svc")), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(observe_now(&i, &mut seen), None);
    assert_eq!(seen.hashed(), [home("svc")]);
}

/// Catches: an `activate` still running being reported as drift, or a recorded version whose directory is gone hiding a receipt at another version.
#[test]
fn a_pending_record_is_not_drift_and_a_record_without_its_directory_checks_the_receipt_only() {
    let i = installed("drift-pending");
    let mut pending = i.rig.records.get(SERVICE).unwrap();
    pending.pending = true;
    i.rig.mac.set_receipt(IDENTIFIER, "0.9", &[]);
    assert_eq!(
        observe(
            &i.rig.host,
            IDENTIFIER,
            Some(&pending),
            Some(&i.dir1),
            &mut Seen::default()
        )
        .unwrap(),
        None
    );
    let record = i.rig.records.get(SERVICE).unwrap();
    let drift = observe(
        &i.rig.host,
        IDENTIFIER,
        Some(&record),
        None,
        &mut Seen::default(),
    )
    .unwrap();
    assert_eq!(
        drift,
        Some(Drift {
            found: Some("0.9".into()),
            recorded: Some("1.0".into()),
            files: vec![]
        })
    );
}

/// Catches: a receipt file that is corrupt or cannot be read being taken for "no receipt".
#[test]
fn an_unreadable_or_corrupt_receipt_file_is_an_error_not_drift() {
    let i = installed("drift-receipt-io");
    let path = i.rig.real("/var/db/receipts/dev.svc.server.plist");
    let record = i.rig.records.get(SERVICE);
    let read = |seen: &mut Seen| {
        observe(
            &i.rig.host,
            IDENTIFIER,
            record.as_ref(),
            Some(&i.dir1),
            seen,
        )
    };
    fs::write(&path, b"not a plist").unwrap();
    assert!(read(&mut Seen::default()).is_err());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(read(&mut Seen::default()).is_err());
}

/// Catches: a link of the payload moved to another target going unreported.
#[test]
fn a_link_of_the_payload_is_checked_by_its_target() {
    let rig = Rig::new("drift-link");
    rig.mac
        .add(v1().put(&home("current"), Content::Link("svc".into()), 0o755));
    let order = rig.order_run("1.0", &BOTH);
    let dir = rig.manual_dir("1.0", "svc-1.0");
    rig.activate(&order, &dir, None).unwrap();
    let record = rig.records.get(SERVICE);
    let mut seen = Seen::default();
    assert_eq!(
        observe(
            &rig.host,
            IDENTIFIER,
            record.as_ref(),
            Some(&dir),
            &mut seen
        )
        .unwrap(),
        None
    );
    fs::remove_file(rig.real(&home("current"))).unwrap();
    symlink("elsewhere", rig.real(&home("current"))).unwrap();
    let drift = observe(
        &rig.host,
        IDENTIFIER,
        record.as_ref(),
        Some(&dir),
        &mut seen,
    )
    .unwrap()
    .unwrap();
    assert_eq!(drift.files, [home("current")]);
}

/// The host after a clean update from 1.0 to 2.0, to compare a repair with.
fn clean_update() -> Installed {
    let (i, _, _) = Installed::updated("repair-control");
    i
}

fn repaired(name: &str, drifted: impl FnOnce(&Rig), found: Option<&str>) {
    let i = installed(name);
    drifted(&i.rig);
    let (dir2, order2) = i.stage_v2();
    let done = i.rig.activate(&order2, &dir2, Some(&i.dir1)).unwrap();
    assert_eq!(
        done,
        Activated {
            drift: Some(found.map(str::to_owned))
        }
    );
    let control = clean_update();
    assert_eq!(i.rig.snapshot(), control.rig.snapshot());
    assert_eq!(i.rig.mac.receipts(), control.rig.mac.receipts());
    assert_eq!(i.rig.records.get(SERVICE), control.rig.records.get(SERVICE));
    assert_eq!(i.rig.mac.enabled("svc.worker"), Some(false));
}

/// Catches: an update over a hand-installed other version that does not delete that version's files or does not report `drift_repaired`.
#[test]
fn the_next_update_repairs_a_receipt_at_another_version() {
    repaired(
        "repair-version",
        |rig| {
            let hand = v1().file("legacy-09", "x", 0o644);
            let mut hand = hand;
            hand.name = "svc-0.9".into();
            hand.version = "0.9".into();
            rig.mac.add(hand);
            rig.mac.install_by_hand("svc-0.9");
        },
        Some("0.9"),
    );
}

/// Catches: an update after a hand `pkgutil --forget` that does not report the missing receipt or leaves the old version's files.
#[test]
fn the_next_update_repairs_a_record_with_no_receipt() {
    repaired("repair-no-receipt", |rig| rig.mac.forget(IDENTIFIER), None);
}

/// Catches: an update that does not report a payload file deleted by hand.
#[test]
fn the_next_update_repairs_a_deleted_payload_file() {
    repaired(
        "repair-deleted",
        |rig| rig.remove_file(&home("VERSION")),
        Some("1.0"),
    );
}

/// Catches: an update that does not report a file with other bytes or a moved link, or does not put back what 2.0 lists.
#[test]
fn the_next_update_repairs_a_file_with_other_bytes_and_a_moved_link() {
    repaired(
        "repair-bytes",
        |rig| rig.write(&home("svc"), b"tampered", 0o755),
        Some("1.0"),
    );

    let rig = Rig::new("repair-link");
    rig.mac
        .add(v1().put(&home("current"), Content::Link("svc".into()), 0o755));
    let order = rig.order_run("1.0", &BOTH);
    let dir1 = rig.manual_dir("1.0", "svc-1.0");
    rig.activate(&order, &dir1, None).unwrap();
    fs::remove_file(rig.real(&home("current"))).unwrap();
    symlink("elsewhere", rig.real(&home("current"))).unwrap();
    rig.mac.add(v2());
    let order2 = rig.order("2.0");
    let dir2 = rig.stage(&order2, "svc-2.0");
    let done = rig.activate(&order2, &dir2, Some(&dir1)).unwrap();
    assert_eq!(
        done,
        Activated {
            drift: Some(Some("1.0".into()))
        }
    );
    assert!(
        !rig.exists(&home("current")),
        "the link 2.0 does not list is deleted"
    );
}

/// Catches: an install over a hand install that reports no drift, or not the version it found.
#[test]
fn the_next_install_over_a_hand_install_with_no_record_reports_the_version_it_found() {
    let rig = Rig::new("repair-no-record");
    let mut hand = v1().file("legacy-09", "x", 0o644);
    hand.name = "svc-0.9".into();
    hand.version = "0.9".into();
    rig.mac.add(hand);
    rig.mac.install_by_hand("svc-0.9");
    rig.mac.add(v2());
    let order = rig.order("2.0");
    let dir = rig.stage(&order, "svc-2.0");
    let done = rig.activate(&order, &dir, None).unwrap();
    assert_eq!(
        done,
        Activated {
            drift: Some(Some("0.9".into()))
        }
    );
    assert!(!rig.exists(&home("legacy-09")) && !rig.exists(&home("old/legacy")));
    assert_eq!(rig.mac.receipt(IDENTIFIER).unwrap().version, "2.0");
}

/// Catches: a listing of the found receipt's files that fails or may be cut being read as an empty list.
#[test]
fn a_receipt_listing_that_cannot_be_read_during_a_repair_is_io_error() {
    for answer in [
        ToolOutput::exit(1, "boom"),
        ToolOutput::success(&"x".repeat(OUTPUT_MAX)),
    ] {
        let i = installed("repair-files-fail");
        i.rig.mac.forget(IDENTIFIER);
        i.rig
            .mac
            .set_receipt(IDENTIFIER, "0.9", &["Library/Application Support/svc/svc"]);
        let (dir2, order2) = i.stage_v2();
        i.rig
            .mac
            .fault(Tool::Pkgutil, &["--only-files"], Fault::Output(answer));
        assert_eq!(
            reason(i.rig.activate(&order2, &dir2, Some(&i.dir1))),
            Reason::IoError
        );
    }
}

/// Catches: an install of the version `current` already names (no `from`) putting a file edited
/// by hand back without reporting `drift_repaired`.
#[test]
fn an_install_of_the_current_version_over_an_edited_file_repairs_it_as_drift() {
    let i = installed("repair-same-version");
    let files = i.rig.snapshot();
    i.rig.write(&home("svc"), b"tampered program", 0o755);
    assert_eq!(
        i.rig.activate(&i.order1, &i.dir1, None).unwrap(),
        Activated {
            drift: Some(Some("1.0".into()))
        }
    );
    assert_eq!(i.rig.snapshot(), files);
    assert_eq!(
        i.rig.activate(&i.order1, &i.dir1, None).unwrap(),
        Activated { drift: None },
        "once repaired, the same install finds nothing"
    );
}
