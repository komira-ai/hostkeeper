//! A record of a check on a farm Mac that cannot be kept: the Mac's verdict still decides. `hk
//! xcode add` still refuses an archive that failed, and `hk xcode verify`, whose answer is the
//! record, exits 1.

use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

use crate::add_rig::{Add, Store, archive_bytes, sha256};
use crate::farm_rig::{
    Farm, MAC, REMOVE, RUN, Waits, asked, good_turn, linux, passing, probe_of, run_waiting,
};

/// Puts a directory where the record of `sha` goes, in a `~/.hostkeeper/xcode/` that is the
/// user's own with mode 0700, so that writing the record fails.
fn block_the_record(add: &Add, sha: &str) {
    for dir in ["home/.hostkeeper", "home/.hostkeeper/xcode"] {
        let path = add.rig.path(dir);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("the directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("its mode");
    }
    fs::create_dir(add.rig.path(&format!("home/.hostkeeper/xcode/{sha}.json")))
        .expect("a directory in the record's place");
}

/// The steps of an archive whose app `codesign` refuses.
fn refused_by_codesign() -> Vec<crate::farm_rig::Step<'static>> {
    let mut steps: Vec<_> = passing()
        .into_iter()
        .filter(|step| step.0 != "verdict")
        .collect();
    for step in &mut steps {
        if step.0 == "codesign" {
            step.1 = Some(1);
        }
    }
    steps.push((
        "verdict",
        Some(1),
        "xcode_signature_invalid: codesign --verify exited 1",
        false,
    ));
    steps
}

/// Proves: when the record of an archive the Mac refused cannot be written, `hk xcode add` says
/// so and still asks `Xcode.Remove` for the archive, says `refused:` and exits 1.
///
/// Catches: a failure to keep the record that skips the removal, leaving an archive that failed
/// its check in the store.
#[test]
fn an_archive_that_failed_is_removed_though_its_record_cannot_be_kept() {
    let add = linux("record-blocked");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    block_the_record(&add, &sha);
    let farm = Farm::checking(
        &sha,
        good_turn(),
        probe_of(RUN, 1_790_000_001, &refused_by_codesign()),
    );
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stdout
            .ends_with("refused: fail: xcode_signature_invalid: codesign --verify exited 1\n"),
        "{ran:?}"
    );
    assert!(
        ran.stderr.contains("hk: the record could not be kept: "),
        "{ran:?}"
    );
    assert_eq!(asked(&server, REMOVE).len(), 1, "{ran:?}");
    assert_eq!(asked(&server, REMOVE)[0]["sha256"], sha.as_str());
}

/// Proves: when the record of an archive the Mac passed cannot be written, `hk xcode verify`
/// prints the record, says it was not kept and exits 1, and removes nothing.
///
/// Catches: a verify that exits 0 with no record kept, which the operator would take as a kept
/// pass.
#[test]
fn verify_exits_1_when_its_record_cannot_be_kept() {
    let add = linux("record-blocked-verify");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    block_the_record(&add, &sha);
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_001, &passing()));
    let server = farm.serve(
        Store::default().holding(&archive_bytes(), "27.2", "27B52"),
        &add.rig,
    );
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "verify", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(ran.stdout.contains("\"verdict\": \"pass\""), "{ran:?}");
    assert!(
        ran.stderr
            .ends_with("hk: verdict pass; the record was not kept\n"),
        "{ran:?}"
    );
    assert!(asked(&server, REMOVE).is_empty());
}
