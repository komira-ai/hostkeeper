//! Tests of the record's JSON form.

use super::*;

#[test]
fn a_settled_record_serialises_without_pending() {
    let mut record = PkgRecord::new("com.example.hello", "1.2.0");
    record
        .created_dirs
        .insert("/Library/Application Support/hello".into());
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(
        json,
        r#"{"installer":"macos-pkg","identifier":"com.example.hello","version":"1.2.0","created_dirs":["/Library/Application Support/hello"]}"#
    );
}

#[test]
fn a_pending_record_serialises_with_pending_true() {
    let mut record = PkgRecord::new("a.b", "1");
    record.pending = true;
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(
        json,
        r#"{"installer":"macos-pkg","identifier":"a.b","version":"1","created_dirs":[],"pending":true}"#
    );
}

/// Catches: a record written before `created_dirs` and `pending` existed refused on read.
#[test]
fn a_record_without_created_dirs_or_pending_reads_with_their_defaults() {
    let record: PkgRecord =
        serde_json::from_str(r#"{"installer":"macos-pkg","identifier":"a.b","version":"1"}"#)
            .unwrap();
    assert_eq!(record, PkgRecord::new("a.b", "1"));
}

#[test]
fn a_new_record_names_the_installer_and_has_no_created_directory() {
    let record = PkgRecord::new("a.b", "2");
    assert_eq!(record.installer, INSTALLER);
    assert!(record.created_dirs.is_empty());
    assert!(!record.pending);
}

#[test]
fn a_record_error_displays_its_text() {
    assert_eq!(
        RecordError("disk full".into()).to_string(),
        "the installed record: disk full"
    );
}
