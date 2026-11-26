//! The tests of the stage's check of Apple's signature on the archive (`pkgutil
//! --check-signature`, step 8), over the fakes of the stage's tests.

use super::*;
use crate::maintenance_hook::xcode::signature::tests::APPLE_REPORT;

impl Mac {
    /// The run of `pkgutil --check-signature` on the archive of `entry`.
    pub(super) fn pkgutil_run(&self, entry: &XcodeEntry) -> Spawn {
        let dir = self.path(&format!("{STAGING}/{}", entry.sha256));
        Spawn {
            program: PathBuf::from(PKGUTIL),
            args: vec![
                OsString::from("--check-signature"),
                dir.join(ARCHIVE).into_os_string(),
            ],
            env: vec![(
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            )],
            cwd: dir,
            deadline: self.deadline(PKGUTIL_SECS),
        }
    }
}

/// The deferral of an archive whose signature check ended as `why`.
fn not_apple(why: &str) -> Result<(), KindError> {
    deferral(
        REASON_ARCHIVE_NOT_APPLE,
        &format!("pkgutil --check-signature {why}"),
    )
}

/// Proves: `pkgutil` runs as `pkgutil --check-signature <archive>` in the entry's staging
/// directory, as root with only `PATH` and a deadline, after the archive's size and SHA-256 are
/// checked and before `xip`: the order of the runs is `pkgutil`, `xip`, `codesign`.
#[test]
fn pkgutil_checks_the_archive_before_xip_expands_it() {
    let mac = Mac::new("xcs-sig-order");
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    let ran = mac.runner.ran();
    let names: Vec<&Path> = ran.iter().map(|spawn| spawn.program.as_path()).collect();
    assert_eq!(
        names,
        [Path::new(PKGUTIL), Path::new(XIP), Path::new(CODESIGN)]
    );
    assert_eq!(ran[0], mac.pkgutil_run(&entry));
    assert_eq!(PKGUTIL, "/usr/sbin/pkgutil");
    assert_eq!(PKGUTIL_SECS, 3600);
}

/// Proves: a report that is not Apple's own (a developer's certificate) defers
/// `xcode_archive_not_apple` naming `pkgutil --check-signature` and the verdict's text, runs no
/// `xip`, and leaves no staging directory; so do a report with no status.
#[test]
fn a_report_that_is_not_apples_defers_before_xip_runs() {
    let developer = APPLE_REPORT.replace(
        "signed Apple Software",
        "signed by a developer certificate issued by Apple for distribution",
    );
    let cases = [
        (
            developer.as_str(),
            "its status is \"signed by a developer certificate issued by Apple for \
             distribution\", not \"signed Apple Software\"",
        ),
        ("Package \"x\":\n", "it gives no status"),
    ];
    for (report, why) in cases {
        let mac = Mac::new("xcs-sig-report");
        mac.runner.answers(PKGUTIL, reports(report));
        let entry = entry_of(ARCHIVE_BYTES);
        assert_eq!(
            mac.run(std::slice::from_ref(&entry)),
            not_apple(&format!(
                "does not report Apple's own signature on the archive: {why}"
            )),
            "{why}"
        );
        assert_eq!(mac.runner.ran(), [mac.pkgutil_run(&entry)], "{why}");
        mac.assert_clean();
    }
}

/// Proves: a `pkgutil` that exits 1 defers `xcode_archive_not_apple` naming the exit and the first
/// line of its standard error, even when its report says Apple; one that is killed at its
/// deadline, is ended by a signal, or cannot start defers the same way saying which; no `xip`
/// runs and no staging directory is left.
#[test]
fn a_pkgutil_that_fails_defers_naming_why() {
    let killed: Program = Box::new(|_| {
        Ok(Ran {
            code: None,
            timed_out: true,
            ..Ran::default()
        })
    });
    let signalled: Program = Box::new(|_| {
        Ok(Ran {
            code: None,
            ..Ran::default()
        })
    });
    let unstartable: Program = Box::new(|_| Err(io::Error::other("no such program")));
    let exits_saying_apple: Program = Box::new(|_| {
        Ok(Ran {
            code: Some(1),
            stdout: APPLE_REPORT.as_bytes().to_vec(),
            stderr: b"Error: the package is damaged\nmore\n".to_vec(),
            ..Ran::default()
        })
    });
    let cases: [(Program, &str); 4] = [
        (
            exits_saying_apple,
            "exited 1: Error: the package is damaged",
        ),
        (killed, "was killed at its deadline"),
        (signalled, "was ended by a signal"),
        (unstartable, "could not start: no such program"),
    ];
    for (pkgutil, why) in cases {
        let mac = Mac::new("xcs-sig-fails");
        mac.runner.answers(PKGUTIL, pkgutil);
        let entry = entry_of(ARCHIVE_BYTES);
        assert_eq!(
            mac.run(std::slice::from_ref(&entry)),
            not_apple(why),
            "{why}"
        );
        assert_eq!(mac.runner.ran(), [mac.pkgutil_run(&entry)], "{why}");
        mac.assert_clean();
    }
}

/// Proves: an app an earlier stage verified is checked again by `codesign` alone; `pkgutil`
/// does not run, since its archive is gone.
#[test]
fn an_already_staged_app_is_verified_without_pkgutil() {
    let entry = entry_of(ARCHIVE_BYTES);
    let app = format!("{STAGING}/{}/Xcode.app", entry.sha256);
    let mac = Mac::new("xcs-sig-staged");
    mac.trusted_staging();
    mac.file(&format!("{app}/Contents/version.plist"), V27);
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    assert_eq!(mac.runner.ran(), [mac.codesign_run(&app)]);
}
