//! The `mdm` package hook's record directory when it is not what the hook made: each failure
//! is the step's, class `environment`, reason `record`, and names the path.

mod common;
mod hook_support;

use std::fs;

use hook_support::*;
use hostkeeper_hook::Class;
use hostkeeper_mdm_step::Host;

/// Asserts `ended` is the record's failure naming `what` of `path`.
fn record_failure(ended: &Ended, what: &str, path: &std::path::Path) {
    assert_eq!(ended.code, 1, "{}", ended.err);
    let report = ended.report();
    assert_eq!(report.class, Class::Environment);
    assert_eq!(report.reason.as_deref(), Some("record"));
    let detail = report.detail.unwrap_or_default();
    assert!(
        detail.starts_with(&format!("cannot {what} {}: ", path.display())),
        "{detail}"
    );
}

/// Catches a step that runs with no record it can hold: a record directory under a regular
/// file cannot be made or opened, and a regular file in its place is not a directory; either
/// way the step fails before it reads the settings or runs anything.
#[test]
fn a_record_directory_that_cannot_be_made_or_is_a_file_fails_the_step() {
    let run = Run::new("record-open");
    fs::write(run.record(), b"").unwrap();
    let host = package_host();
    let ended = run.step(&host, &["cutover"], true);
    assert_eq!(ended.code, 1);
    assert_eq!(ended.report().reason.as_deref(), Some("record"));
    assert_eq!(
        ended.report().detail,
        Some(format!(
            "the record directory {} is not a directory",
            run.record().display()
        ))
    );

    let run = Run::new("record-under-a-file");
    fs::write(run.scratch.path().join("file"), b"").unwrap();
    let record = run.scratch.path().join("file/record");
    let params = run.scratch.path().join("params.toml");
    fs::write(
        &params,
        format!(
            "settings = \"{SETTINGS}\"\nrecord_dir = \"{}\"\n",
            record.display()
        ),
    )
    .unwrap();
    let ended = run_mdm(
        &host,
        &["mdm", "update-hook", "cutover", "--params"]
            .map(std::ffi::OsString::from)
            .into_iter()
            .chain([params.into_os_string()])
            .collect::<Vec<_>>(),
    );
    record_failure(&ended, "open the record directory", &record);
    assert!(host.calls().is_empty());
}

/// Catches a record that is read or written past an error: a key that is a directory cannot
/// be read (cutover) or removed (prepare's last write), a key whose temporary name is a
/// directory cannot be written, and a `saved` that is a file fails the step that keeps a file
/// (prepare), reads the kept files (rollback) or forgets them (cleanup).
#[test]
fn a_record_whose_files_cannot_be_read_written_or_removed_fails_the_step() {
    let run = Run::new("record-key-dir");
    let host = package_host();
    fs::create_dir_all(run.record().join("prepared")).unwrap();
    let ended = run.step(&host, &["cutover"], true);
    record_failure(&ended, "read", &run.record().join("prepared"));

    let run = Run::new("record-remove");
    let host = package_host();
    fs::create_dir_all(run.record().join("verified/x")).unwrap();
    let ended = run.prepare(&host, true);
    record_failure(&ended, "remove", &run.record().join("verified"));

    let run = Run::new("record-write");
    let host = package_host();
    fs::create_dir_all(run.record().join(".prepared.new")).unwrap();
    let ended = run.prepare(&host, true);
    record_failure(&ended, "write", &run.record().join("prepared"));

    let run = Run::new("record-saved-file");
    let host = package_host();
    fs::create_dir_all(run.record()).unwrap();
    fs::write(run.record().join("saved"), b"").unwrap();
    let before = host.paths();
    let ended = run.prepare(&host, true);
    record_failure(&ended, "create", &run.record().join("saved"));
    assert_eq!(
        host.paths(),
        before,
        "nothing is changed before the kept files are kept"
    );
    let ended = run.step(&host, &["rollback"], true);
    record_failure(
        &ended,
        "read",
        &run.record().join("saved/%etc%mdm%Caddyfile"),
    );
    fs::write(run.record().join("verified"), b"").unwrap();
    let ended = run.step(&host, &["cleanup"], true);
    record_failure(&ended, "read", &run.record().join("saved"));
    assert!(host.read(CADDYFILE).unwrap().is_none());
}
