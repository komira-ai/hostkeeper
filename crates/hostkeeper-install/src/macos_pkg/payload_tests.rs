//! Tests of the `Bom` listing, the entry rules and the jobs' plists.

use std::fs;

use hostkeeper_testkit::Scratch;
use plist::{Dictionary, Value};

use super::*;

const SERVICE: &str = "hello";
const LABEL: &str = "com.example.hello";
const HOME: &str = "/Library/Application Support/hello";
const BIN: &str = "/Library/Application Support/hello/bin/hello";
const PLIST: &str = "/Library/LaunchDaemons/com.example.hello.plist";

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), name).unwrap()
}

fn label() -> Label {
    Label::parse(LABEL).unwrap()
}

fn meta(kind: Kind, mode: u32, gid: u32) -> Meta {
    Meta {
        kind,
        mode,
        uid: 0,
        gid,
    }
}

fn dir() -> Meta {
    meta(Kind::Dir, 0o755, GID_WHEEL)
}

fn file(mode: u32) -> Meta {
    meta(Kind::File, mode, GID_WHEEL)
}

/// The entries of a valid payload of the service `hello` with its one job.
fn valid() -> BTreeMap<String, Meta> {
    let mut tree = BTreeMap::new();
    for path in [
        "/Library",
        "/Library/Application Support",
        "/Library/LaunchDaemons",
        HOME,
        "/Library/Application Support/hello/bin",
    ] {
        tree.insert(path.to_owned(), dir());
    }
    tree.insert(BIN.to_owned(), file(0o755));
    tree.insert(PLIST.to_owned(), file(0o644));
    tree
}

fn verdict(tree: &BTreeMap<String, Meta>) -> Result<(), PayloadError> {
    check(tree, tree, SERVICE, &[label()])
}

#[test]
fn the_expected_paths_of_a_service_and_a_label() {
    assert_eq!(service_dir("hello"), HOME);
    assert_eq!(plist_path(&label()), PLIST);
}

#[test]
fn parse_bom_reads_dot_and_dot_slash_paths_with_mode_uid_and_gid() {
    let listing = ".\t40755\t0\t0\n./Library\t40755\t0\t80\n\n./Library/x\t100644\t0\t0\n./Library/l\t120755\t0\t0\n";
    let bom = parse_bom(listing).unwrap();
    assert_eq!(bom.len(), 3);
    assert_eq!(bom["/Library"], meta(Kind::Dir, 0o755, GID_ADMIN));
    assert_eq!(bom["/Library/x"], file(0o644));
    assert_eq!(bom["/Library/l"].kind, Kind::Link);
}

#[test]
fn parse_bom_keeps_the_setuid_bit_of_a_listed_file() {
    let bom = parse_bom("./x\t104755\t0\t0\n").unwrap();
    assert_eq!(bom["/x"].mode, 0o4755);
}

#[test]
fn parse_bom_of_nothing_is_empty() {
    assert!(parse_bom("").unwrap().is_empty());
    assert!(parse_bom(".\t40755\t0\t0\n").unwrap().is_empty());
}

/// Catches: a malformed listing line skipped, so the Bom reads as listing less.
#[test]
fn parse_bom_refuses_a_line_that_is_not_path_mode_uid_gid() {
    for line in [
        "./x\t100644\t0",
        "./x\t100644\t0\t0\textra",
        "x\t100644\t0\t0",
        "./x\tzz\t0\t0",
        "./x\t100644\troot\t0",
        "./x\t100644\t0\twheel",
        "./x\t100644\t-1\t0",
        "no tabs here",
    ] {
        assert_eq!(
            parse_bom(line),
            Err(PayloadError::BadLine(line.into())),
            "{line:?}"
        );
    }
}

#[test]
fn parse_bom_refuses_a_path_listed_twice() {
    let listing = "./x\t100644\t0\t0\n./x\t100644\t0\t0\n";
    assert_eq!(parse_bom(listing), Err(PayloadError::Twice("/x".into())));
}

#[test]
fn a_valid_payload_passes_and_a_payload_with_no_job_needs_no_plist() {
    assert_eq!(verdict(&valid()), Ok(()));
    let mut tree = valid();
    tree.remove(PLIST);
    assert_eq!(check(&tree, &tree, SERVICE, &[]), Ok(()));
}

/// Catches: a Bom that lists a file the payload lacks passing, so `installer` writes less.
#[test]
fn a_bom_entry_the_payload_lacks_is_refused() {
    let bom = valid();
    let mut payload = valid();
    payload.remove(BIN);
    assert_eq!(
        check(&bom, &payload, SERVICE, &[label()]),
        Err(PayloadError::NotInPayload(BIN.into()))
    );
}

#[test]
fn a_payload_entry_the_bom_does_not_list_is_refused() {
    let mut bom = valid();
    let payload = valid();
    bom.remove(BIN);
    assert_eq!(
        check(&bom, &payload, SERVICE, &[label()]),
        Err(PayloadError::NotInBom(BIN.into()))
    );
}

/// Catches: a payload file with a setuid bit that the Bom does not show.
#[test]
fn a_payload_file_whose_mode_differs_from_the_boms_is_refused() {
    let bom = valid();
    let mut payload = valid();
    payload.insert(BIN.into(), file(0o4755));
    assert_eq!(
        check(&bom, &payload, SERVICE, &[label()]),
        Err(PayloadError::Differs(BIN.into(), file(0o755), file(0o4755)))
    );
}

#[test]
fn an_entry_not_owned_by_root_is_refused() {
    let mut tree = valid();
    let owned = Meta {
        uid: 501,
        ..file(0o755)
    };
    tree.insert(BIN.into(), owned);
    assert_eq!(verdict(&tree), Err(PayloadError::NotRoot(BIN.into(), 501)));
}

/// Catches: group staff (20) accepted, so any admin-less login can rewrite the file.
#[test]
fn an_entry_of_group_staff_is_refused() {
    let mut tree = valid();
    tree.insert(BIN.into(), meta(Kind::File, 0o755, 20));
    assert_eq!(verdict(&tree), Err(PayloadError::Group(BIN.into(), 20)));
}

#[test]
fn an_entry_of_group_admin_is_accepted() {
    let mut tree = valid();
    tree.insert(BIN.into(), meta(Kind::File, 0o755, GID_ADMIN));
    assert_eq!(verdict(&tree), Ok(()));
}

#[test]
fn modes_with_a_setid_sticky_or_write_bit_for_group_or_others_are_refused() {
    for mode in [0o775, 0o757, 0o4755, 0o2755, 0o1755, 0o4000, 0o020, 0o002] {
        let mut tree = valid();
        tree.insert(BIN.into(), file(mode));
        assert_eq!(
            verdict(&tree),
            Err(PayloadError::Mode(BIN.into(), mode)),
            "{mode:o}"
        );
    }
}

#[test]
fn a_fifo_or_a_device_is_refused() {
    let mut tree = valid();
    tree.insert(BIN.into(), meta(Kind::Other, 0o644, GID_WHEEL));
    assert_eq!(verdict(&tree), Err(PayloadError::Special(BIN.into())));
}

/// Catches: files placed under `/usr/local` or in another service's directory.
#[test]
fn paths_outside_the_services_directory_and_its_plists_are_refused() {
    for path in [
        "/usr/local/bin/hello",
        "/Library/Application Support/other/x",
        "/Library/Application Support/hello2",
        "/Library/Application Support/hellox/y",
        "/Library/LaunchDaemons/other.plist",
        "/Library/LaunchDaemons/sub/x",
        "/etc/passwd",
    ] {
        let mut tree = valid();
        tree.insert(path.into(), file(0o644));
        assert_eq!(
            verdict(&tree),
            Err(PayloadError::Outside(path.into(), HOME.into())),
            "{path}"
        );
    }
}

#[test]
fn a_plist_that_is_not_a_regular_file_is_outside() {
    let mut tree = valid();
    tree.insert(PLIST.into(), meta(Kind::Link, 0o755, GID_WHEEL));
    assert_eq!(
        verdict(&tree),
        Err(PayloadError::Outside(PLIST.into(), HOME.into()))
    );
}

#[test]
fn a_directory_above_the_payload_must_be_a_directory() {
    for above in ABOVE {
        let mut tree = valid();
        tree.insert(above.into(), file(0o644));
        assert_eq!(verdict(&tree), Err(PayloadError::NotDir(above.into())));
    }
}

#[test]
fn the_service_directory_itself_and_files_below_it_are_inside() {
    let mut tree = valid();
    tree.insert(format!("{HOME}/deep/er/file"), file(0o600));
    assert_eq!(verdict(&tree), Ok(()));
}

fn job(entries: Vec<(&str, Value)>) -> Value {
    let mut dict = Dictionary::new();
    for (key, value) in entries {
        dict.insert(key.into(), value);
    }
    Value::Dictionary(dict)
}

fn good_job() -> Vec<(&'static str, Value)> {
    vec![
        ("Label", Value::String(LABEL.into())),
        ("Disabled", Value::Boolean(true)),
        ("Program", Value::String(BIN.into())),
    ]
}

/// A payload directory holding the job `value` as the label's plist, and the check of it.
fn jobs_verdict(name: &str, value: Option<Value>) -> Result<(), PayloadError> {
    let dir = scratch(name);
    if let Some(value) = value {
        let real = dir.path().join(PLIST.trim_start_matches('/'));
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        value.to_file_xml(&real).unwrap();
    }
    check_jobs(dir.path(), &valid(), &[label()])
}

#[test]
fn a_disabled_job_of_the_labels_name_with_a_payload_program_passes() {
    assert_eq!(jobs_verdict("pjgood", Some(job(good_job()))), Ok(()));
}

#[test]
fn no_run_label_needs_no_plist() {
    let dir = scratch("pjnone");
    assert_eq!(check_jobs(dir.path(), &valid(), &[]), Ok(()));
}

#[test]
fn a_run_label_whose_plist_the_bom_does_not_list_is_refused() {
    let tmp = scratch("pjnolist");
    let mut bom = valid();
    bom.remove(PLIST);
    assert_eq!(
        check_jobs(tmp.path(), &bom, &[label()]),
        Err(PayloadError::NoPlist(LABEL.into(), PLIST.into()))
    );
    bom.insert(PLIST.into(), dir());
    assert_eq!(
        check_jobs(tmp.path(), &bom, &[label()]),
        Err(PayloadError::NoPlist(LABEL.into(), PLIST.into()))
    );
}

#[test]
fn a_plist_the_payload_lacks_on_disk_is_no_job() {
    let got = jobs_verdict("pjmissing", None);
    assert!(matches!(got, Err(PayloadError::BadPlist(path, _)) if path == PLIST));
}

#[test]
fn a_plist_that_is_not_a_dictionary_is_refused() {
    let got = jobs_verdict("pjarray", Some(Value::Array(vec![])));
    assert_eq!(
        got,
        Err(PayloadError::BadPlist(
            PLIST.into(),
            "not a dictionary".into()
        ))
    );
}

#[test]
fn a_plist_that_is_not_a_plist_is_refused() {
    let dir = scratch("pjgarbage");
    let real = dir.path().join(PLIST.trim_start_matches('/'));
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, b"not a plist at all").unwrap();
    let got = check_jobs(dir.path(), &valid(), &[label()]);
    assert!(matches!(got, Err(PayloadError::BadPlist(path, _)) if path == PLIST));
}

/// Catches: a plist whose `Label` differs from the run label, which would enable another job.
#[test]
fn a_plist_whose_label_differs_or_is_missing_is_refused() {
    let mut other = good_job();
    other[0].1 = Value::String("com.example.other".into());
    assert_eq!(
        jobs_verdict("pjlabel", Some(job(other))),
        Err(PayloadError::Label(
            PLIST.into(),
            "com.example.other".into(),
            LABEL.into()
        ))
    );
    let mut none = good_job();
    none.remove(0);
    assert_eq!(
        jobs_verdict("pjnolabel", Some(job(none))),
        Err(PayloadError::Label(
            PLIST.into(),
            String::new(),
            LABEL.into()
        ))
    );
}

/// Catches: a job that starts at load because it is not shipped disabled.
#[test]
fn a_plist_not_disabled_is_refused() {
    let mut absent = good_job();
    absent.remove(1);
    let mut off = good_job();
    off[1].1 = Value::Boolean(false);
    let mut text = good_job();
    text[1].1 = Value::String("true".into());
    for (name, entries) in [("pjabsent", absent), ("pjoff", off), ("pjtext", text)] {
        assert_eq!(
            jobs_verdict(name, Some(job(entries))),
            Err(PayloadError::NotDisabled(PLIST.into()))
        );
    }
}

#[test]
fn program_arguments_name_the_program_when_program_is_absent() {
    let mut entries = good_job();
    entries[2] = (
        "ProgramArguments",
        Value::Array(vec![
            Value::String(BIN.into()),
            Value::String("--flag".into()),
        ]),
    );
    assert_eq!(jobs_verdict("pjargs", Some(job(entries))), Ok(()));
}

#[test]
fn program_wins_over_program_arguments() {
    let mut entries = good_job();
    entries.push((
        "ProgramArguments",
        Value::Array(vec![Value::String("/not/in/payload".into())]),
    ));
    assert_eq!(jobs_verdict("pjboth", Some(job(entries))), Ok(()));
    let mut wrong = good_job();
    wrong[2].1 = Value::String("/not/in/payload".into());
    wrong.push((
        "ProgramArguments",
        Value::Array(vec![Value::String(BIN.into())]),
    ));
    assert_eq!(
        jobs_verdict("pjwrong", Some(job(wrong))),
        Err(PayloadError::Program(
            PLIST.into(),
            "/not/in/payload".into()
        ))
    );
}

/// Catches: a program outside the payload, which the install would not have put there.
#[test]
fn a_program_that_is_no_file_of_the_payload_is_refused() {
    let mut dir_program = good_job();
    dir_program[2].1 = Value::String(HOME.into());
    let mut missing = good_job();
    missing.remove(2);
    let mut empty_args = good_job();
    empty_args[2] = ("ProgramArguments", Value::Array(vec![]));
    let mut odd_args = good_job();
    odd_args[2] = (
        "ProgramArguments",
        Value::Array(vec![Value::Integer(1.into())]),
    );
    for (name, entries, program) in [
        ("pjdir", dir_program, HOME),
        ("pjnoprog", missing, ""),
        ("pjempty", empty_args, ""),
        ("pjodd", odd_args, ""),
    ] {
        assert_eq!(
            jobs_verdict(name, Some(job(entries))),
            Err(PayloadError::Program(PLIST.into(), program.into()))
        );
    }
}

#[test]
fn labels_of_lists_the_launch_daemon_plists_with_valid_labels() {
    let mut tree = valid();
    tree.insert(
        "/Library/LaunchDaemons/second.job.plist".into(),
        file(0o644),
    );
    tree.insert("/Library/LaunchDaemons/notes.txt".into(), file(0o644));
    tree.insert("/Library/LaunchDaemons/bad label.plist".into(), file(0o644));
    tree.insert("/Library/LaunchDaemons/.plist".into(), file(0o644));
    tree.insert("/Library/Other/third.plist".into(), file(0o644));
    let got: Vec<String> = labels_of(&tree)
        .iter()
        .map(|l| l.as_str().to_owned())
        .collect();
    assert_eq!(got, [LABEL, "second.job"]);
}
