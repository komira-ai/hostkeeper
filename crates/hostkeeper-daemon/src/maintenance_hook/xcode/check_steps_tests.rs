//! The tests of the archive check's steps (`cutover`, on the drained Mac), over the fakes and the
//! helpers of the check's other tests: each step by exact name, exit code and output, each failure
//! by its verdict, each program run by equality of the whole list.

use super::*;

/// Proves: a check of an archive Apple signed runs the eight steps in order with exactly these
/// names, exit codes and outputs, and each program of the check as the stage runs its own (its
/// path, arguments, working directory, environment and deadline: 60 s for `sw_vers` and `uname`);
/// `xip` saw the fetched archive; and after the steps nothing of the check is left.
///
/// Catches: a step left out, reordered or given another name, a program run with another
/// argument or deadline, an output not joined as the probe joins it, or a check directory left in
/// `/Applications`.
#[test]
fn a_good_archive_passes_in_eight_steps_and_leaves_nothing() {
    let mac = mac("chk-pass");
    let steps = checked(&mac);
    assert_eq!(
        steps,
        [
            recorded(),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode.app"),
                kept("version", None, "27.2 27B52"),
                kept("codesign", Some(0), ""),
                kept("verdict", Some(0), "pass"),
            ]
        ]
        .concat()
    );
    let mut runs = recording_runs(&mac);
    runs.extend([pkgutil_run(&mac), xip_run(&mac), codesign_run(&mac)]);
    assert_eq!(mac.runner.ran(), runs);
    assert_eq!(*lock(&mac.runner.archives), [ARCHIVE_BYTES.to_vec()]);
    assert_gone(&mac);
}

/// Proves: an archive a step refuses ends the steps at that step, with the verdict exit 1 and
/// `<reason>: <detail>`, no later program runs, and the check directory is removed. The cases:
/// `pkgutil` exits non-zero; `pkgutil` exits 0 for an archive not signed by Apple itself;
/// `xip` exits non-zero; `xip` makes no app; `xip` makes two apps (and a plain file and a
/// directory that are not apps); the app has no `version.plist`; its `version.plist` cannot be
/// read; it lacks the version, or the build; it is another version; `codesign` exits non-zero.
///
/// Catches: a failing archive that passes, a refusal under the wrong reason, a step that runs
/// after a refusal, or an output that does not say why.
#[test]
fn each_archive_failure_ends_the_steps_with_its_reason_and_verdict() {
    let developer = APPLE_REPORT.replace("signed Apple Software", "signed by a developer");
    let why = "its status is \"signed by a developer\", not \"signed Apple Software\"";
    // The plain file's name differs from `Xcode.app` in more than case: the Mac's volume is case
    // insensitive, so `xcode.app` there is the directory `Xcode.app` and cannot be written.
    let two_apps: Program = Box::new(|spawn| {
        for dir in ["Xcode.app", "Xcode-beta.app", "Other"] {
            fs::create_dir_all(spawn.cwd.join(dir))?;
        }
        fs::write(spawn.cwd.join("Notes.app"), b"a file, not an app")?;
        Ok(Ran {
            code: Some(0),
            ..Ran::default()
        })
    });
    // The program to answer, its answer, what the steps after sw_vers and uname are, and how
    // many programs ran in all.
    let cases: Vec<(&str, Program, Vec<MaintenanceProbeStep>, usize)> = vec![
        (
            PKGUTIL,
            says(Some(1), "", "Error: not signed\n"),
            vec![
                kept("pkgutil", Some(1), "Error: not signed"),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_archive_not_apple: pkgutil --check-signature exited 1: Error: not signed",
                ),
            ],
            3,
        ),
        (
            PKGUTIL,
            says(Some(0), &developer, ""),
            vec![
                kept("pkgutil", Some(0), why),
                kept(
                    "verdict",
                    Some(1),
                    &format!("xcode_archive_not_apple: pkgutil --check-signature: {why}"),
                ),
            ],
            3,
        ),
        (
            XIP,
            says(Some(1), "", "xip: signature not trusted\n"),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(1), "xip: signature not trusted"),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_expand_failed: xip --expand exited 1: xip: signature not trusted",
                ),
            ],
            4,
        ),
        (
            XIP,
            says(Some(0), "", ""),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "none"),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_expand_failed: xip --expand made 0 apps, not one Xcode",
                ),
            ],
            4,
        ),
        (
            XIP,
            two_apps,
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode-beta.app, Xcode.app"),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_expand_failed: xip --expand made 2 apps, not one Xcode",
                ),
            ],
            4,
        ),
        (
            XIP,
            xip_making("Xcode.app", V26),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode.app"),
                kept("version", None, "26.3 17C52"),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_build_mismatch: the app is Xcode 26.3 (17C52); the order names Xcode 27.2 (27B52)",
                ),
            ],
            4,
        ),
        (
            XIP,
            xip_making("Xcode.app", NO_BUILD),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode.app"),
                kept("version", None, "version.plist has no ProductBuildVersion"),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_build_mismatch: the app is Xcode 27.2 (<none>); the order names Xcode 27.2 (27B52)",
                ),
            ],
            4,
        ),
        (
            XIP,
            xip_making("Xcode.app", INTEGER),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode.app"),
                kept(
                    "version",
                    None,
                    "version.plist has no CFBundleShortVersionString",
                ),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_build_mismatch: the app is Xcode <none> (27B52); the order names Xcode 27.2 (27B52)",
                ),
            ],
            4,
        ),
        (
            CODESIGN,
            says(Some(3), "", "Xcode.app: a sealed resource is missing\n"),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode.app"),
                kept("version", None, "27.2 27B52"),
                kept(
                    "codesign",
                    Some(3),
                    "Xcode.app: a sealed resource is missing",
                ),
                kept(
                    "verdict",
                    Some(1),
                    "xcode_signature_invalid: codesign --verify exited 3: Xcode.app: a sealed resource is missing",
                ),
            ],
            5,
        ),
    ];
    for (at, (program, answer, tail, ran)) in cases.into_iter().enumerate() {
        let mac = mac(&format!("chk-refuse-{at}"));
        mac.runner.answers(program, answer);
        assert_eq!(checked(&mac), [recorded(), tail].concat(), "case {at}");
        assert_eq!(mac.runner.ran().len(), ran, "case {at}");
        assert_gone(&mac);
    }
}

/// Proves: an app whose `version.plist` is not there is refused `xcode_build_mismatch`, and one
/// whose `version.plist` cannot be read (not a property list) is not checked
/// (`xcode_staging_failed`, no exit code), each with the step saying which file and why (no exit
/// code, as the step runs no program).
///
/// Catches: a missing or unreadable version list taken as a match, or a read that fails on the
/// Mac taken as a refusal of the archive, which would remove it from the store.
#[test]
fn a_version_list_that_is_missing_is_a_build_mismatch_and_one_that_cannot_be_read_is_not_checked() {
    let plist = format!("{CHECK_DIR}/expand/Xcode.app/Contents/version.plist");
    let missing = format!("{plist} is missing");
    let mac = mac("chk-no-plist");
    mac.runner.answers(
        XIP,
        Box::new(|spawn| {
            fs::create_dir_all(spawn.cwd.join("Xcode.app"))?;
            Ok(Ran {
                code: Some(0),
                ..Ran::default()
            })
        }),
    );
    assert_eq!(
        checked(&mac)[4..],
        [
            kept("app", None, "Xcode.app"),
            kept("version", None, &missing),
            kept(
                "verdict",
                Some(1),
                &format!("xcode_build_mismatch: {missing}")
            ),
        ]
    );

    let mac = self::mac("chk-bad-plist");
    mac.runner
        .answers(XIP, xip_making("Xcode.app", b"this is not a property list"));
    assert_eq!(
        checked(&mac)[4..],
        [
            kept("app", None, "Xcode.app"),
            kept("version", None, &format!("{plist} is not a property list")),
            kept(
                "verdict",
                None,
                &format!("xcode_staging_failed: {plist} is not a property list")
            ),
        ]
    );
}

/// Proves: a program that cannot be started, one killed at its deadline and one ended by a signal
/// (`pkgutil`, `xip` and `codesign` in turn) give a step with no exit code saying what happened,
/// and a verdict with no exit code `xcode_check_unfinished: <that>`, with no later step; the
/// archive is neither passed nor refused.
///
/// Catches: a program that did not finish taken as a refusal of the archive (which would remove
/// a good archive from the store) or as a pass.
#[test]
fn a_program_that_does_not_finish_leaves_the_archive_unchecked() {
    let pkg = "/usr/sbin/pkgutil could not start: no such program";
    let xip = "/usr/bin/xip was killed at its deadline of 7200s";
    let sign = "/usr/bin/codesign was ended by a signal";
    let unfinished = |what: &str| kept("verdict", None, &format!("xcode_check_unfinished: {what}"));
    let cases: Vec<(&str, Program, Vec<MaintenanceProbeStep>)> = vec![
        (
            PKGUTIL,
            cannot_start(),
            vec![kept("pkgutil", None, pkg), unfinished(pkg)],
        ),
        (
            XIP,
            killed(),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", None, xip),
                unfinished(xip),
            ],
        ),
        (
            CODESIGN,
            says(None, "", ""),
            vec![
                kept("pkgutil", Some(0), APPLE_SIGNED),
                kept("xip", Some(0), ""),
                kept("app", None, "Xcode.app"),
                kept("version", None, "27.2 27B52"),
                kept("codesign", None, sign),
                unfinished(sign),
            ],
        ),
    ];
    for (at, (program, answer, tail)) in cases.into_iter().enumerate() {
        let mac = mac(&format!("chk-unfinished-{at}"));
        mac.runner.answers(program, answer);
        assert_eq!(checked(&mac), [recorded(), tail].concat(), "case {at}");
        assert_gone(&mac);
    }
}

/// Proves: `sw_vers` and `uname` are recorded only: one that cannot start and one killed at its
/// deadline of 60 s are steps with no exit code saying so, and the check goes on and passes.
///
/// Catches: a record-only program that fails the check.
#[test]
fn sw_vers_and_uname_never_fail_the_check() {
    let mac = mac("chk-record");
    mac.runner.answers(SW_VERS, cannot_start());
    mac.runner.answers(UNAME, killed());
    let steps = checked(&mac);
    assert_eq!(
        steps[..2],
        [
            kept(
                "sw_vers",
                None,
                "/usr/bin/sw_vers could not start: no such program"
            ),
            kept(
                "uname",
                None,
                "/usr/bin/uname was killed at its deadline of 60s"
            ),
        ]
    );
    assert_eq!(steps.last(), Some(&kept("verdict", Some(0), "pass")));
    assert_eq!(steps.len(), 8);
}

/// Proves: a check directory that is not the user's alone, one with no archive in it (the
/// prepare was skipped or its archive is gone), and one whose archive is not a file give a
/// verdict with no exit code `xcode_staging_failed: <why>`, with no program of steps 3 to 7 run,
/// and nothing is left. `sw_vers` and `uname` run (in the check directory) when the directory is
/// the user's alone, and are `not run` with no program started when it is not.
///
/// Catches: Apple's tools run on a path an admin of the Mac swapped in, a program run in such a
/// directory (where the runner writes its output files), or a missing archive taken as a refusal
/// of the archive.
#[test]
fn an_archive_that_is_gone_is_staging_failed_and_runs_no_check() {
    let cases = ["no directory", "no archive", "a directory", "open"];
    for case in cases {
        let mac = mac("chk-gone");
        let dir = mac.path(CHECK_DIR);
        let archive = dir.join(ARCHIVE);
        let detail = match case {
            "no directory" => format!(
                "{} is not a directory only this process's user can write",
                dir.display()
            ),
            "no archive" => {
                dir_with_mode(&mac, CHECK_DIR, 0o700);
                format!(
                    "cannot read {}: No such file or directory (os error 2)",
                    archive.display()
                )
            }
            "a directory" => {
                dir_with_mode(&mac, CHECK_DIR, 0o700);
                fs::create_dir(&archive).expect("a directory in the archive's place");
                format!("{} is not a file", archive.display())
            }
            _ => {
                mac.file(&format!("{CHECK_DIR}/Xcode.xip"), ARCHIVE_BYTES);
                dir_with_mode(&mac, CHECK_DIR, 0o755);
                format!(
                    "{} is not a directory only this process's user can write",
                    dir.display()
                )
            }
        };
        let first = match case {
            "no archive" | "a directory" => recorded(),
            _ => not_run(&dir),
        };
        assert_eq!(
            check(&mac).steps(&order()),
            [
                first,
                vec![kept(
                    "verdict",
                    None,
                    &format!("xcode_staging_failed: {detail}")
                )]
            ]
            .concat(),
            "{case}"
        );
        let ran = match case {
            "no archive" | "a directory" => recording_runs(&mac),
            _ => Vec::new(),
        };
        assert_eq!(mac.runner.ran(), ran, "{case}");
        assert_gone(&mac);
    }
}

/// Proves: a file of the check that cannot be made (the check directory replaced by a file while
/// `pkgutil` ran) and an expansion directory that is gone after `xip` give a verdict with no exit
/// code `xcode_staging_failed`, after the step that ran; not a refusal of the archive.
///
/// Catches: a failure of the Mac's own files that is reported as the archive's.
#[test]
fn files_of_the_check_that_cannot_be_made_or_read_are_not_a_refusal() {
    let mac = mac("chk-make");
    let dir = mac.path(CHECK_DIR);
    let replaced = dir.clone();
    mac.runner.answers(
        PKGUTIL,
        Box::new(move |_| {
            fs::remove_dir_all(&replaced)?;
            fs::write(&replaced, b"a file")?;
            Ok(Ran {
                code: Some(0),
                stdout: APPLE_REPORT.as_bytes().to_vec(),
                ..Ran::default()
            })
        }),
    );
    let expand = dir.join(EXPAND);
    assert_eq!(
        checked(&mac)[2..],
        [
            kept("pkgutil", Some(0), APPLE_SIGNED),
            kept(
                "verdict",
                None,
                &format!(
                    "xcode_staging_failed: cannot make {}: Not a directory (os error 20)",
                    expand.display()
                )
            ),
        ]
    );
    assert_gone(&mac);

    let mac = self::mac("chk-read");
    let expand = mac.path(CHECK_DIR).join(EXPAND);
    let gone = expand.clone();
    mac.runner.answers(
        XIP,
        Box::new(move |_| {
            fs::remove_dir_all(&gone)?;
            Ok(Ran {
                code: Some(0),
                ..Ran::default()
            })
        }),
    );
    assert_eq!(
        checked(&mac)[2..],
        [
            kept("pkgutil", Some(0), APPLE_SIGNED),
            kept("xip", Some(0), ""),
            kept(
                "verdict",
                None,
                &format!(
                    "xcode_staging_failed: cannot read {}: No such file or directory (os error 2)",
                    expand.display()
                )
            ),
        ]
    );
    assert_gone(&mac);
}

/// Proves: a cutover whose order names two archives records `sw_vers` and `uname`, runs none of
/// the check, and gives a verdict with no exit code `xcode_order_invalid`.
///
/// Catches: an order the prepare refused that is checked all the same, on its first archive.
#[test]
fn an_order_of_two_archives_is_not_checked_at_cutover() {
    let mac = mac("chk-two");
    dir_with_mode(&mac, CHECK_DIR, 0o700);
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(
        check(&mac).steps(&[entry.clone(), entry]),
        [
            recorded(),
            vec![kept(
                "verdict",
                None,
                "xcode_order_invalid: a probe checks one archive; the order names 2"
            )]
        ]
        .concat()
    );
    assert_eq!(mac.runner.ran(), recording_runs(&mac));
}

/// Proves: an output longer than the probe's bound is cut to 128 bytes and the step says it was
/// cut, in a program's step (`pkgutil`'s standard output of 300 bytes) and in the verdict (a
/// detail that does not fit).
///
/// Catches: an output over the report's bound, which the coordinator refuses whole, losing the
/// probe and with it the verdict.
#[test]
fn an_output_over_the_bound_is_cut_and_says_so() {
    let mac = mac("chk-cut");
    mac.runner
        .answers(PKGUTIL, says(Some(1), &"x".repeat(300), ""));
    let steps = checked(&mac);
    assert_eq!(
        steps[2],
        MaintenanceProbeStep {
            name: "pkgutil".to_owned(),
            exit_code: Some(1),
            output: "x".repeat(128),
            cut: true,
        }
    );
    let mac = self::mac("chk-cut-verdict");
    mac.runner.answers(
        XIP,
        says(Some(1), "", &format!("xip: {}\n", "y".repeat(300))),
    );
    let verdict = checked(&mac).pop().expect("a verdict");
    let full = format!(
        "xcode_expand_failed: xip --expand exited 1: xip: {}",
        "y".repeat(300)
    );
    assert_eq!(
        verdict,
        MaintenanceProbeStep {
            name: "verdict".to_owned(),
            exit_code: Some(1),
            output: full[..128].to_owned(),
            cut: true,
        }
    );
}

/// Proves: a volume that has lost, since the prepare, the room the expansion needs (three times
/// the archive's size) ends the steps after `pkgutil` with a verdict with no exit code
/// `free_space_short: <detail>`, and one whose free space cannot be read with
/// `free_space_unreadable: <detail>`; `xip` does not run and nothing is left.
///
/// Catches: an `xip` that fails for want of room on the Mac reported as a refusal of the archive
/// (verdict exit 1), which `hk xcode add` answers by removing a good archive from the store.
#[test]
fn a_volume_short_of_the_expansion_at_cutover_is_not_checked() {
    let expansion = ARCHIVE_BYTES.len() as u64 * 3;
    let short = format!(
        "free_space_short: {} bytes are free on the volume of /Applications; the check needs \
         {expansion}",
        expansion - 1
    );
    for (free, verdict) in [
        (Ok(expansion - 1), short),
        (
            Err("statvfs failed".to_owned()),
            "free_space_unreadable: statvfs failed".to_owned(),
        ),
    ] {
        let mut mac = mac("chk-room");
        assert_eq!(check(&mac).prepare(&order()), Ok(()));
        mac.space.free = free;
        assert_eq!(
            check(&mac).steps(&order()),
            [
                recorded(),
                vec![
                    kept("pkgutil", Some(0), APPLE_SIGNED),
                    kept("verdict", None, &verdict),
                ]
            ]
            .concat()
        );
        let mut runs = recording_runs(&mac);
        runs.push(pkgutil_run(&mac));
        assert_eq!(mac.runner.ran(), runs);
        assert_gone(&mac);
    }
}

/// Proves: the prepare of a plain probe (no archive in the order) removes a check directory a
/// run that ended before its cutover left, with its archive, and fetches nothing.
///
/// Catches: a multi-GB archive left in `/Applications` by a cancelled check until another
/// archive is checked on the Mac.
#[test]
fn a_plain_probe_removes_what_an_ended_check_left() {
    let mac = mac("chk-left");
    assert_eq!(check(&mac).prepare(&order()), Ok(()));
    assert!(mac.path(CHECK_DIR).join(ARCHIVE).is_file());
    let asked = mac.store.asked().len();
    assert_eq!(check(&mac).prepare(&[]), Ok(()));
    assert_eq!(mac.store.asked().len(), asked);
    assert!(mac.runner.ran().is_empty());
    assert_gone(&mac);
}
