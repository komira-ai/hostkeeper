//! `hk xcode verify --mac <node-id> <archive.xip>` off macOS: the archive must be in the
//! coordinator's store, a farm Mac checks it, and the record the Mac's steps make is printed and
//! kept. What `hk xcode add` does with the same check is in `xcode_farm_add`.

use std::os::unix::fs::PermissionsExt as _;

use serde_json::{Value, json};

use crate::add_rig::{Store, archive_bytes, puts, sha256};
use crate::farm_rig::{
    CANCEL, Farm, MAC, REMOVE, RUN, SCHEDULE, Waits, asked, at, farm_calls, good_turn, got, linux,
    no_probe, passing, probe_of, refused, run_waiting, turn,
};
use crate::rig::STAMP;
use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceHostStep as Step, MaintenanceState};

/// The store of most tests: it holds the archive of the tests as 27.2 (27B52).
fn held() -> Store {
    Store::default().holding(&archive_bytes(), "27.2", "27B52")
}

/// The steps of an archive whose app is not the release the order names: the checks run to the
/// version and stop there.
fn wrong_build() -> Vec<crate::farm_rig::Step<'static>> {
    let mut steps: Vec<_> = passing()
        .into_iter()
        .filter(|step| !matches!(step.0, "codesign" | "verdict"))
        .collect();
    for step in &mut steps {
        if step.0 == "version" {
            step.2 = "27.2 27B99";
        }
    }
    steps.push((
        "verdict",
        Some(1),
        "xcode_build_mismatch: the app is Xcode 27.2 (27B99); the order names Xcode 27.2 (27B52)",
        false,
    ));
    steps
}

/// Proves: an archive the store does not hold is not checked (exit 1) saying so, with the line
/// `hk xcode add <path>` that uploads and checks it; nothing is scheduled and no record is written;
/// the archive's SHA-256 is the one in the words.
#[test]
fn an_archive_the_store_does_not_hold_fails_saying_how_to_upload_it() {
    let add = linux("verify-not-held");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), no_probe());
    let server = farm.serve(Store::default(), &add.rig);
    let ran = add.run(server.port, &["xcode", "verify", "--mac", MAC, &path]);
    assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: the coordinator's store holds no archive of these bytes (sha256 {sha}), and \
             {MAC} reads the archive from there: hk xcode add {path} uploads it and checks it\n"
        )),
        "{ran:?}"
    );
    assert!(farm_calls(&server).is_empty());
    assert!(!add.kept(&format!("{sha}.json")).exists());
}

/// Proves: a good check prints the record on stdout as it is written, exactly, and exits 0; the
/// record is the file `~/.hostkeeper/xcode/<sha256>.json` (mode 0600) and has the key `farm`; the
/// schedule's reason says `hk xcode verify`; stderr says `verdict pass; record kept in <path>`;
/// the archive is read from the store, not uploaded, and nothing is removed.
#[test]
fn a_good_check_prints_the_record_and_exits_0() {
    let add = linux("verify-pass");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_000, &passing()));
    let server = farm.serve(held(), &add.rig);
    let waits = Waits::new();
    let ran = run_waiting(
        &add,
        &waits,
        server.port,
        &["xcode", "verify", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    let file = add.kept(&format!("{sha}.json"));
    assert_eq!(
        ran.stdout,
        std::fs::read_to_string(&file).expect("a record")
    );
    assert_eq!(
        std::fs::metadata(&file)
            .expect("a record")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let record: Value = serde_json::from_str(&ran.stdout).expect("JSON");
    assert_eq!(
        record,
        json!({
            "record": "hk-xcode-verify-v1",
            "hk": STAMP,
            "macos": {
                "sw_vers": {
                    "exit": 0,
                    "stdout": "ProductName: macOS; ProductVersion: 26.1; BuildVersion: 25B78",
                    "stderr": ""
                },
                "uname": {"exit": 0, "stdout": "Darwin arm64", "stderr": ""}
            },
            "programs": null,
            "archive": {
                "name": "Xcode_27.2.xip",
                "size": archive_bytes().len(),
                "sha256": sha,
                "kind": "xar!"
            },
            "signature": null,
            "tamper": null,
            "toc": null,
            "expansion": {
                "free_before": null,
                "free_after": null,
                "exit": 0,
                "output": "",
                "seconds": null,
                "apps": ["Xcode.app"],
                "contents": [],
                "version_plist": false,
                "plutil": null,
                "version": "27.2",
                "build": "27B52",
                "codesign": {"exit": 0, "stdout": "", "stderr": ""},
                "spctl": null
            },
            "farm": record["farm"].clone(),
            "verdict": "pass"
        })
    );
    assert_eq!(record["farm"]["host"], MAC);
    assert_eq!(record["farm"]["run"], RUN);
    assert_eq!(record["farm"]["steps"].as_array().map(Vec::len), Some(8));
    assert!(
        ran.stderr.contains(&format!(
            "hk: verdict pass; record kept in {}\n",
            file.display()
        )),
        "{ran:?}"
    );
    assert_eq!(
        asked(&server, SCHEDULE),
        [json!({
            "kinds": ["MAINTENANCE_KIND_PROBE"],
            "hostIds": [MAC],
            "reason": format!("hk xcode verify: check Xcode 27.2 (27B52), sha256 {}", &sha[..12]),
            "takeLastHost": true,
            "confirm": true,
            "xcodeSha256": sha,
        })]
    );
    assert!(puts(&server).is_empty());
    assert_eq!(waits.secs(), [15; 7]);
    assert!(asked(&server, REMOVE).is_empty() && asked(&server, CANCEL).is_empty());
}

/// Proves: an archive the Mac says failed exits 1 and still prints and keeps the record, whose
/// verdict is `fail: <the Mac's verdict output>` and whose expansion holds the steps that ran (the
/// version the app has); the archive is not removed from the store (only `hk xcode add` does that)
/// and stderr says the `hk xcode remove --sha256 <sha256> --reason <why>` line that does.
#[test]
fn an_archive_that_failed_prints_its_record_exits_1_and_names_the_removal() {
    let add = linux("verify-fail");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(
        &sha,
        good_turn(),
        probe_of(RUN, 1_790_000_002, &wrong_build()),
    );
    let server = farm.serve(held(), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "verify", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    let record: Value = serde_json::from_str(&ran.stdout).expect("JSON");
    assert_eq!(
        record["verdict"],
        "fail: xcode_build_mismatch: the app is Xcode 27.2 (27B99); the order names Xcode 27.2 \
         (27B52)"
    );
    assert_eq!(record["expansion"]["build"], "27B99");
    assert_eq!(record["expansion"]["codesign"], Value::Null);
    assert_eq!(record["farm"]["probed_unix"], 1_790_000_002_u64);
    assert_eq!(
        ran.stdout,
        std::fs::read_to_string(add.kept(&format!("{sha}.json"))).expect("a record")
    );
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: the archive stays in the store; to remove it: hk xcode remove --sha256 {sha} \
             --reason <why>\n"
        )),
        "{ran:?}"
    );
    assert!(asked(&server, REMOVE).is_empty());
}

/// Proves: a deferral naming `xcode_archive_mismatch` is a failure with a record (the verdict
/// `fail: <token>: <detail>`) after the run is cancelled, exit 1, and nothing is removed.
#[test]
fn a_deferral_naming_the_archive_is_a_failure_with_a_record() {
    let add = linux("verify-deferral");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let reason = "stage: exit 75, xcode_archive_mismatch: the archive is 5 bytes, not 6";
    let deferred = got(MaintenanceState::Running, turn(Step::Deferred, reason));
    let farm = Farm::checking(&sha, vec![at(Step::Staging), deferred], no_probe());
    let server = farm.serve(held(), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "verify", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    let record: Value = serde_json::from_str(&ran.stdout).expect("JSON");
    assert_eq!(
        record["verdict"],
        "fail: xcode_archive_mismatch: the archive is 5 bytes, not 6"
    );
    assert_eq!(record["farm"]["reason"], reason);
    assert_eq!(asked(&server, CANCEL).len(), 1);
    assert!(asked(&server, REMOVE).is_empty());
}

/// Proves: a check that gave no verdict (a halted turn, a deferral for another reason) exits 1 with
/// nothing on stdout, says why on stderr and writes no record; and a check that is refused at the
/// schedule exits as the gateway's call path does (2 for `nothing_due`), saying so in words.
#[test]
fn a_check_with_no_verdict_exits_1_and_writes_no_record() {
    let add = linux("verify-none");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let halted = got(
        MaintenanceState::Halted,
        turn(Step::Halted, "verify: exit 1"),
    );
    let space = got(
        MaintenanceState::Running,
        turn(
            Step::Deferred,
            "stage: exit 75, free_space_short: 3 GB free",
        ),
    );
    for (reply, says) in [
        (
            halted,
            format!("hk: not checked: {MAC}'s turn of run {RUN} halted: verify: exit 1\n"),
        ),
        (
            space,
            format!(
                "hk: not checked: {MAC} deferred its turn of run {RUN} (the archive was not \
                 checked): stage: exit 75, free_space_short: 3 GB free\n"
            ),
        ),
    ] {
        let farm = Farm::checking(&sha, vec![reply], no_probe());
        let server = farm.serve(held(), &add.rig);
        let ran = run_waiting(
            &add,
            &Waits::new(),
            server.port,
            &["xcode", "verify", "--mac", MAC, &path],
        );
        assert_eq!((ran.code, ran.stdout.as_str()), (1, ""), "{ran:?}");
        assert!(ran.stderr.ends_with(&says), "{ran:?}");
        assert!(!add.kept(&format!("{sha}.json")).exists());
    }
    let farm = Farm::checking(&sha, good_turn(), no_probe()).on(
        SCHEDULE,
        vec![refused("FAILED_PRECONDITION", "host_in_maintenance")],
    );
    let server = farm.serve(held(), &add.rig);
    let ran = add.run(server.port, &["xcode", "verify", "--mac", MAC, &path]);
    assert_eq!((ran.code, ran.stdout.as_str()), (2, ""), "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: {MAC} is in another maintenance run (hk maintenance list shows the open ones); \
             run this again when it ends\nhk: the coordinator answered FAILED_PRECONDITION\n\
             refused: host_in_maintenance\n"
        )),
        "{ran:?}"
    );
    assert!(!add.kept(&format!("{sha}.json")).exists());
}

/// Proves: `--apple-root` is a usage error (exit 2) off macOS, where no program of Apple's runs
/// and nothing is fetched; it is refused before anything is read, made or asked.
#[test]
fn apple_root_is_a_usage_error_off_macos() {
    let add = linux("verify-root");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = Store::default().serve(&add.rig);
    let ran = add.run(
        server.port,
        &[
            "xcode",
            "verify",
            "--mac",
            MAC,
            "--apple-root",
            "root.cer",
            &path,
        ],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (
            2,
            "",
            "hk: --apple-root is for the check on this Mac, which fetches or reads Apple's root \
             certificate itself; this system is linux, and the archive is checked on a farm Mac\n"
        )
    );
    assert!(server.seen().is_empty());
    assert!(!add.rig.path("home/.hostkeeper").exists());
}

/// Proves: a path that is no file, and a command line with no path or two, are the errors they are
/// on a Mac, off macOS too.
#[test]
fn the_archive_must_be_one_readable_file() {
    let add = linux("verify-usage");
    let server = Store::default().serve(&add.rig);
    let ran = add.run(server.port, &["xcode", "verify", "--mac", MAC]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: hk xcode verify takes the path of one archive (~/Downloads/Xcode_27.2.xip)\n"
        )
    );
    let missing = add
        .rig
        .path("home/Downloads/none.xip")
        .display()
        .to_string();
    let ran = add.run(server.port, &["xcode", "verify", "--mac", MAC, &missing]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!("hk: cannot read {missing} (")),
        "{ran:?}"
    );
    assert!(server.seen().is_empty());
}

/// Proves: a path with a space in it is quoted for a shell where a line names it for the operator
/// to run: in `hk xcode add '<path>'` of an archive the store does not hold, and in the line that
/// checks an archive again after `hk xcode add` could not check it.
#[test]
fn a_path_with_a_space_is_quoted_where_a_line_names_it() {
    let add = linux("verify-quote");
    let path = add.file("My Downloads/Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), no_probe());
    let server = farm.serve(Store::default(), &add.rig);
    let ran = add.run(server.port, &["xcode", "verify", "--mac", MAC, &path]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .ends_with(&format!("hk xcode add '{path}' uploads it and checks it\n")),
        "{ran:?}"
    );
    let halted = got(
        MaintenanceState::Halted,
        turn(Step::Halted, "verify: exit 1"),
    );
    let farm = Farm::checking(&sha, vec![halted], no_probe());
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(&format!(
            "to check it again: hk xcode verify --mac {MAC} '{path}'\n"
        )),
        "{ran:?}"
    );
}
