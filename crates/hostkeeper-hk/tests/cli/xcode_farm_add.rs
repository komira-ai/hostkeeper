//! `hk xcode add` off macOS, with a farm Mac that checks the archive: the upload, the maintenance
//! run of the kind probe that names the archive, the polling through the world's wait, what each
//! end of the Mac's turn makes of the run, the record, and the refusals of the coordinator. The
//! choice of the Mac and of the version is in `xcode_farm_choose`, `hk xcode verify` in
//! `xcode_farm_verify`.

use std::os::unix::fs::PermissionsExt as _;

use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceHostStep as Step, MaintenanceState};
use serde_json::{Value, json};

use crate::add_rig::{Store, archive_bytes, puts, sha256};
use crate::farm_rig::{
    CANCEL, Farm, GET, HOST, MAC, REMOVE, RUN, SCHEDULE, Waits, asked, at, done, farm_calls,
    good_turn, got, linux, no_probe, ok, passing, probe_of, refused, run_waiting, scheduled, turn,
};
use crate::rig::{Reply, STAMP, TOKEN};

/// The size of the archive of the tests.
fn size() -> usize {
    archive_bytes().len()
}

/// The record a good check of the archive of the tests leaves, as the Mac reported it.
fn passing_record(sha: &str) -> Value {
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
        "archive": {"name": "Xcode_27.2.xip", "size": size(), "sha256": sha, "kind": "xar!"},
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
        "farm": {
            "host": MAC,
            "run": RUN,
            "probed_unix": 1_790_000_000_u64,
            "steps": [
                {"name": "sw_vers", "exit": 0,
                 "output": "ProductName: macOS; ProductVersion: 26.1; BuildVersion: 25B78",
                 "cut": false},
                {"name": "uname", "exit": 0, "output": "Darwin arm64", "cut": false},
                {"name": "pkgutil", "exit": 0,
                 "output": "signed Apple Software, its chain ending at Apple Root CA",
                 "cut": false},
                {"name": "xip", "exit": 0, "output": "", "cut": false},
                {"name": "app", "exit": null, "output": "Xcode.app", "cut": false},
                {"name": "version", "exit": null, "output": "27.2 27B52", "cut": false},
                {"name": "codesign", "exit": 0, "output": "", "cut": false},
                {"name": "verdict", "exit": 0, "output": "pass", "cut": false}
            ]
        },
        "verdict": "pass"
    })
}

/// The Mac's steps of an archive whose signature is not Apple's: the checks stop at `pkgutil`.
fn unsigned() -> Vec<crate::farm_rig::Step<'static>> {
    vec![
        ("sw_vers", Some(0), "ProductName: macOS", false),
        ("uname", Some(0), "Darwin arm64", false),
        (
            "pkgutil",
            Some(0),
            "its status is \"no signature\", not \"signed Apple Software\"",
            false,
        ),
        (
            "verdict",
            Some(1),
            "xcode_archive_not_apple: its status is \"no signature\", not \"signed Apple Software\"",
            false,
        ),
    ]
}

/// The lines of `stderr` that say a step of the Mac's turn.
fn steps_said(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix(&format!("hk: {MAC}: ")))
        .collect()
}

/// Proves: off macOS a good run uploads the archive as on a Mac, schedules one run of the kind probe
/// on the named Mac alone (the request by equality: the kind, the host, `takeLastHost`, `confirm`,
/// the archive's SHA-256 and a reason naming the release and the first 12 hex digits), reads the
/// run through the world's wait (seven waits of 15 seconds for eight reads), says each step the
/// turn reaches once on stderr, reads the probe the Mac reports, prints `stored:` and then `checked
/// on <mac> (run <id>): pass` on stdout, and writes the record (mode 0600) by exact equality.
#[test]
fn a_good_archive_is_uploaded_checked_on_the_farm_mac_and_recorded() {
    let add = linux("pass");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_000, &passing()));
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let waits = Waits::new();
    let ran = run_waiting(
        &add,
        &waits,
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        ran.stdout,
        format!(
            "stored: Xcode 27.2 (27B52), sha256 {sha}, {} bytes\nchecked on {MAC} (run {RUN}): \
             pass\n",
            size()
        )
    );
    assert_eq!(puts(&server).len(), 1);
    assert_eq!(
        asked(&server, SCHEDULE),
        [json!({
            "kinds": ["MAINTENANCE_KIND_PROBE"],
            "hostIds": [MAC],
            "reason": format!("hk xcode add: check Xcode 27.2 (27B52), sha256 {}", &sha[..12]),
            "takeLastHost": true,
            "confirm": true,
            "xcodeSha256": sha,
        })]
    );
    assert_eq!(asked(&server, GET), vec![json!({"runId": RUN}); 8]);
    assert_eq!(asked(&server, HOST), [json!({"nodeId": MAC})]);
    assert!(asked(&server, CANCEL).is_empty() && asked(&server, REMOVE).is_empty());
    assert_eq!(waits.secs(), [15; 7]);
    assert_eq!(
        steps_said(&ran.stderr),
        [
            "waiting",
            "staging: fetching the archive from the store",
            "draining",
            "applying: checking the archive",
            "checking",
            "putting back",
        ]
    );
    assert!(
        ran.stderr.contains(&format!(
            "hk: run {RUN} checks the archive on {MAC}: the Mac is drained for it, and its turn \
             ends by itself\nhk: to stop it: hk maintenance cancel --run-id {RUN} --reason <why>\n"
        )),
        "{ran:?}"
    );
    let record = add.kept(&format!("{sha}.json"));
    assert_eq!(
        std::fs::metadata(&record)
            .expect("a record")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(crate::farm_rig::record_of(&add, &sha), passing_record(&sha));
    // The Macs summary of the run's end was asked after the check.
    let paths = crate::farm_rig::paths(&server);
    assert_eq!(
        paths.last().map(String::as_str),
        Some(crate::add_rig::FLEET[0])
    );
}

/// Proves: a later run of the same bytes (the store now holds them) finds the passing record the
/// farm check kept, which `reuse::release_of` reads, and ends as on a Mac with a record:
/// `already in the store:` and exit 0, with no `Maintenance.Schedule` and no other call of the
/// check.
#[test]
fn a_later_run_of_the_same_bytes_reuses_the_record_and_schedules_nothing() {
    let add = linux("reuse");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_000, &passing()));
    let first = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        first.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let second = Farm::new().serve(held, &add.rig);
    let ran = add.run(second.port, &["xcode", "add", "--mac", MAC, &path]);
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (0, "already in the store: Xcode 27.2 (27B52)\n"),
        "{ran:?}"
    );
    assert!(farm_calls(&second).is_empty());
    assert!(puts(&second).is_empty());
}

/// Proves: an archive of these bytes that the store holds, with no record of them, is checked on
/// the farm Mac without an upload: `already in the store:` and the check line, then exit 0, a
/// record written, one run scheduled, no `PUT`.
#[test]
fn a_held_archive_with_no_record_is_checked_and_not_uploaded() {
    let add = linux("held");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_000, &passing()));
    let store = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = farm.serve(store, &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            0,
            format!(
                "already in the store: Xcode 27.2 (27B52)\nchecked on {MAC} (run {RUN}): pass\n"
            )
            .as_str()
        ),
        "{ran:?}"
    );
    assert!(puts(&server).is_empty());
    assert_eq!(asked(&server, SCHEDULE).len(), 1);
    assert_eq!(crate::farm_rig::record_of(&add, &sha), passing_record(&sha));
}

/// Proves: an archive the Mac says failed (the verdict step exits 1 with `<reason>: <detail>`)
/// is refused: the record is written with the verdict `fail: <output>` and the steps that ran,
/// `Xcode.Remove` is asked with the SHA-256 and the reason naming the Mac, the run and the failure,
/// stdout says `refused: <verdict>` after `stored:`, stderr says the store no longer holds it, and
/// the exit is 1.
#[test]
fn an_archive_failure_in_the_verdict_removes_the_archive_and_exits_1() {
    let add = linux("fail");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1_790_000_001, &unsigned()));
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    let verdict = "fail: xcode_archive_not_apple: its status is \"no signature\", not \"signed \
                   Apple Software\"";
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            1,
            format!(
                "stored: Xcode 27.2 (27B52), sha256 {sha}, {} bytes\nrefused: {verdict}\n",
                size()
            )
            .as_str()
        ),
        "{ran:?}"
    );
    assert_eq!(
        asked(&server, REMOVE),
        [json!({
            "sha256": sha,
            "reason": format!(
                "hk xcode add: the check on {MAC} (run {RUN}) failed: xcode_archive_not_apple"
            ),
        })]
    );
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: the store no longer holds the archive: it failed the check on {MAC} (run \
             {RUN}): xcode_archive_not_apple\n"
        )),
        "{ran:?}"
    );
    let record = crate::farm_rig::record_of(&add, &sha);
    assert_eq!(record["verdict"], verdict);
    assert_eq!(record["expansion"], Value::Null);
    assert_eq!(record["farm"]["probed_unix"], 1_790_000_001_u64);
    assert_eq!(record["farm"]["steps"].as_array().map(Vec::len), Some(4));
    assert!(asked(&server, CANCEL).is_empty());
}

/// Proves: when the Mac's turn is deferred with `xcode_archive_mismatch` (the stage found the
/// bytes it fetched are not the archive), `Maintenance.Cancel` is asked first so that the Mac is
/// not offered the turn again, then the archive is removed from the store; the record has the
/// verdict `fail: <token>: <detail>`, no probe (`probed_unix` null, no steps) and the Mac's
/// reason, and `expansion` null; stdout says `refused:` and the exit is 1.
#[test]
fn a_deferral_naming_the_archive_cancels_the_run_and_removes_the_archive() {
    let add = linux("mismatch");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let reason = "stage: exit 75, xcode_archive_mismatch: the archive is 5 bytes, not 6";
    let deferred = got(MaintenanceState::Running, turn(Step::Deferred, reason));
    let farm = Farm::checking(&sha, vec![at(Step::Staging), deferred], no_probe());
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    let verdict = "fail: xcode_archive_mismatch: the archive is 5 bytes, not 6";
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            1,
            format!(
                "stored: Xcode 27.2 (27B52), sha256 {sha}, {} bytes\nrefused: {verdict}\n",
                size()
            )
            .as_str()
        ),
        "{ran:?}"
    );
    let order: Vec<String> = farm_calls(&server).into_iter().map(|s| s.target).collect();
    assert_eq!(
        order,
        [SCHEDULE, GET, GET, CANCEL, REMOVE].map(str::to_owned),
        "{ran:?}"
    );
    let cancel = asked(&server, CANCEL);
    assert_eq!(cancel[0]["runId"], RUN);
    assert_eq!(
        cancel[0]["reason"],
        format!("hk xcode add: {MAC} deferred its turn")
    );
    assert!(asked(&server, HOST).is_empty());
    assert_eq!(
        asked(&server, REMOVE)[0]["reason"],
        format!("hk xcode add: the check on {MAC} (run {RUN}) failed: xcode_archive_mismatch")
    );
    let record = crate::farm_rig::record_of(&add, &sha);
    assert_eq!(record["verdict"], verdict);
    assert_eq!(
        record["farm"],
        json!({"host": MAC, "run": RUN, "probed_unix": null, "steps": [], "reason": reason})
    );
    assert_eq!(
        record["macos"]["sw_vers"]["stderr"],
        format!("not reported by {MAC}")
    );
    assert_eq!(record["macos"]["uname"]["exit"], Value::Null);
}

/// Proves: a deferral that is not about the archive (`free_space_short`) is cancelled and not
/// checked: no `Xcode.Remove`, no record, the archive stays stored, exit 1, and stderr says why
/// and the line that checks it again; a deferral naming `xcode_archive_encrypted` is an archive
/// failure as `xcode_archive_mismatch` is, and one with no detail says `fail: <token>`; an archive
/// that was held already and failed prints `refused:` and not `already in the store:`.
#[test]
fn a_deferral_for_another_reason_is_not_checked_and_keeps_the_archive() {
    let add = linux("space");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let reason = "stage: exit 75, free_space_short: 3 GB free, 12 GB needed";
    let farm = Farm::checking(
        &sha,
        vec![got(MaintenanceState::Running, turn(Step::Deferred, reason))],
        no_probe(),
    );
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (
            1,
            format!(
                "stored: Xcode 27.2 (27B52), sha256 {sha}, {} bytes\n",
                size()
            )
            .as_str()
        ),
        "{ran:?}"
    );
    assert_eq!(asked(&server, CANCEL).len(), 1);
    assert!(asked(&server, REMOVE).is_empty());
    assert!(!add.kept(&format!("{sha}.json")).exists());
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: not checked: {MAC} deferred its turn of run {RUN} (the archive was not \
             checked): {reason}\nhk: the archive stays in the store; to check it again: hk xcode \
             verify --mac {MAC} {path}\n"
        )),
        "{ran:?}"
    );
    // The encrypted archive, and a reason with no detail.
    let encrypted = "stage: exit 75, xcode_archive_encrypted";
    let farm = Farm::checking(
        &sha,
        vec![got(
            MaintenanceState::Running,
            turn(Step::Deferred, encrypted),
        )],
        no_probe(),
    );
    let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = farm.serve(held, &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(
        (ran.code, ran.stdout.as_str()),
        (1, "refused: fail: xcode_archive_encrypted\n"),
        "{ran:?}"
    );
    assert_eq!(
        crate::farm_rig::record_of(&add, &sha)["verdict"],
        "fail: xcode_archive_encrypted"
    );
}

/// Proves: a halted turn is not checked and writes no record: stderr says the Mac stays drained
/// until an operator acts, with the `hk maintenance retry-host` and `hk maintenance put-back` lines
/// that name the run and the host, and the line that checks the archive again; exit 1; no cancel,
/// no removal.
#[test]
fn a_halted_turn_is_not_checked_and_says_how_to_go_on() {
    let add = linux("halted");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let halted = got(
        MaintenanceState::Halted,
        turn(Step::Halted, "verify: exit 1"),
    );
    let farm = Farm::checking(&sha, vec![at(Step::Draining), halted], no_probe());
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: {MAC} stays drained until an operator acts: hk maintenance retry-host --run-id \
             {RUN} --host-id {MAC} --reason <why> tries its turn again from the step that failed, \
             and hk maintenance put-back --run-id {RUN} --host-id {MAC} --reason <why> puts it \
             back in service\n"
        )),
        "{ran:?}"
    );
    assert!(
        ran.stderr.contains(&format!(
            "hk: not checked: {MAC}'s turn of run {RUN} halted: verify: exit 1\n"
        )),
        "{ran:?}"
    );
    assert!(asked(&server, CANCEL).is_empty() && asked(&server, REMOVE).is_empty());
    assert!(!add.kept(&format!("{sha}.json")).exists());
}

/// Proves: a done turn whose probe has no `verdict` step (a Mac whose hostkeeper is older and made
/// its reads), one of another run, and a Mac that reports no probe are not checked: no record, no
/// removal, exit 1, and the words say the Mac's hostkeeper made its reads and is older than this
/// hk or the store no longer held the archive.
#[test]
fn a_probe_that_is_no_check_of_this_run_is_not_checked() {
    let add = linux("older");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let reads: Vec<crate::farm_rig::Step<'_>> = passing()
        .into_iter()
        .filter(|step| step.0 != "verdict")
        .collect();
    let words = format!(
        "hk: not checked: {MAC}'s probe is not an archive check of run {RUN}: its hostkeeper made \
         its reads, which means it is older than this hk, or the store no longer held the archive \
         when its turn started\n"
    );
    let probes = [
        probe_of(RUN, 1, &reads),
        probe_of("run-6", 1, &passing()),
        probe_of("", 0, &[]),
    ];
    for probe in probes {
        let farm = Farm::checking(&sha, vec![done()], probe);
        let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
        let server = farm.serve(held, &add.rig);
        let ran = run_waiting(
            &add,
            &Waits::new(),
            server.port,
            &["xcode", "add", "--mac", MAC, &path],
        );
        assert_eq!(ran.code, 1, "{ran:?}");
        assert!(ran.stderr.contains(&words), "{ran:?}");
        assert!(asked(&server, REMOVE).is_empty());
        assert!(!add.kept(&format!("{sha}.json")).exists());
    }
    let farm = Farm::checking(&sha, vec![done()], no_probe());
    let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = farm.serve(held, &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: not checked: {MAC} reports no probe of run {RUN}\n"
        )),
        "{ran:?}"
    );
}

/// Proves: a verdict step that did not exit by itself (the check did not finish: the Mac's
/// `xcode_check_unfinished`), or whose exit is neither 0 with `pass` nor 1, is not checked and
/// says what the Mac wrote; nothing is removed or recorded.
#[test]
fn a_verdict_that_is_neither_pass_nor_failure_is_not_checked() {
    let add = linux("unfinished");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let cases: [(Option<i32>, &str, &str); 3] = [
        (
            None,
            "xcode_check_unfinished: /usr/bin/xip was killed at its deadline of 7200s",
            "hk: not checked: mac-farm-1's check did not finish (run run-7): \
             xcode_check_unfinished: /usr/bin/xip was killed at its deadline of 7200s\n",
        ),
        (
            Some(2),
            "odd",
            "hk: not checked: mac-farm-1's verdict of run run-7 is exit 2, odd, which this hk \
             does not read\n",
        ),
        (
            Some(0),
            "fine",
            "hk: not checked: mac-farm-1's verdict of run run-7 is exit 0, fine, which this hk \
             does not read\n",
        ),
    ];
    for (exit, output, says) in cases {
        let steps = [("verdict", exit, output, false)];
        let farm = Farm::checking(&sha, vec![done()], probe_of(RUN, 5, &steps));
        let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
        let server = farm.serve(held, &add.rig);
        let ran = run_waiting(
            &add,
            &Waits::new(),
            server.port,
            &["xcode", "add", "--mac", MAC, &path],
        );
        assert_eq!(ran.code, 1, "{output}: {ran:?}");
        assert!(ran.stderr.contains(says), "{output}: {ran:?}");
        assert!(asked(&server, REMOVE).is_empty(), "{output}");
        assert!(!add.kept(&format!("{sha}.json")).exists(), "{output}");
    }
}

/// Proves: a coordinator that answers the run with no `xcodeSha256` (older than this hk, so it
/// scheduled a plain probe) has its run cancelled with `Maintenance.Cancel`, and the run fails
/// (exit 1) saying so and that nothing was checked, with the archive uploaded once and never
/// repeated, no read of the run, no record.
#[test]
fn an_older_coordinator_has_its_plain_probe_cancelled() {
    let add = linux("old-coordinator");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), no_probe()).on(SCHEDULE, vec![scheduled(None)]);
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(
        ran.stdout,
        format!(
            "stored: Xcode 27.2 (27B52), sha256 {sha}, {} bytes\n",
            size()
        )
    );
    assert!(
        ran.stderr.contains(&format!(
            "hk: this coordinator cannot order an archive check: its run {RUN} on {MAC} does not \
             name the archive, so the Mac makes its ordinary reads; it is cancelled; nothing was \
             checked\n"
        )),
        "{ran:?}"
    );
    assert_eq!(
        asked(&server, CANCEL),
        [json!({
            "runId": RUN,
            "reason": "hk xcode add: this coordinator cannot order an archive check",
        })]
    );
    assert!(asked(&server, GET).is_empty());
    assert_eq!(puts(&server).len(), 1);
    assert!(!add.kept(&format!("{sha}.json")).exists());
    // A cancel that is refused too is said, with the line that stops the run.
    let farm = Farm::checking(&sha, good_turn(), no_probe())
        .on(SCHEDULE, vec![scheduled(None)])
        .on(CANCEL, vec![refused("FAILED_PRECONDITION", "run_ended")]);
    let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = farm.serve(held, &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "it could not be cancelled (Maintenance.Cancel was refused: run_ended \
             (FAILED_PRECONDITION)); stop it with hk maintenance cancel --run-id {RUN} --reason \
             <why>; nothing was checked\n"
        )),
        "{ran:?}"
    );
}

/// Proves: a Schedule that answers a run with no id is a failure saying so (exit 1), before any
/// read of the run.
#[test]
fn a_run_with_no_id_is_a_failure() {
    let add = linux("no-id");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), no_probe())
        .on(SCHEDULE, vec![Reply::new(200, "{\"run\":{}}")]);
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: the answer of Maintenance.Schedule names no run; nothing was checked\n"),
        "{ran:?}"
    );
    assert!(asked(&server, GET).is_empty());
}

/// Proves: each refusal of `Maintenance.Schedule` is said in words with what to do, then the
/// coordinator's code and `refused: <reason>`, and exits as the gateway's call path does (2 for
/// INVALID_ARGUMENT and FAILED_PRECONDITION, 1 for NOT_FOUND and INTERNAL, 3 for
/// PERMISSION_DENIED); a reason this hk has no words for says only the code and the reason. The
/// archive stays stored (the line that checks it again is said) and nothing is read or recorded.
#[test]
fn each_refusal_of_the_schedule_is_said_in_words_and_exits_as_a_call_does() {
    let add = linux("refusals");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let cases: [(&str, &str, u8, &str); 6] = [
        (
            "FAILED_PRECONDITION",
            "nothing_due",
            2,
            "hk: mac-farm-1 cannot check the archive: it must be a Mac whose hostkeeper lists \
             probe on demand, in a host group that keeps Xcode (xcode_keep 1 or more), so that it \
             can read the store\n",
        ),
        (
            "FAILED_PRECONDITION",
            "host_in_maintenance",
            2,
            "hk: mac-farm-1 is in another maintenance run (hk maintenance list shows the open \
             ones); run this again when it ends\n",
        ),
        (
            "NOT_FOUND",
            "unknown_host",
            1,
            "hk: the coordinator has not enrolled a host mac-farm-1; hk hosts list names the \
             hosts\n",
        ),
        (
            "INVALID_ARGUMENT",
            "invalid_request",
            2,
            "hk: the coordinator holds no archive of these bytes, or refused the request: this \
             coordinator may be older than this hk, which asks it for a check of one archive\n",
        ),
        ("INTERNAL", "store_failed", 1, ""),
        ("PERMISSION_DENIED", "forbidden", 3, ""),
    ];
    for (code, reason, exit, words) in cases {
        let farm =
            Farm::checking(&sha, good_turn(), no_probe()).on(SCHEDULE, vec![refused(code, reason)]);
        let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
        let ran = run_waiting(
            &add,
            &Waits::new(),
            server.port,
            &["xcode", "add", "--mac", MAC, &path],
        );
        assert_eq!(ran.code, exit, "{reason}: {ran:?}");
        let again = format!(
            "hk: the archive stays in the store; to check it again: hk xcode verify --mac {MAC} \
             {path}\n"
        );
        let tail =
            format!("{words}hk: the coordinator answered {code}\nrefused: {reason}\n{again}");
        assert!(ran.stderr.ends_with(&tail), "{reason}: {ran:?}");
        assert!(asked(&server, GET).is_empty(), "{reason}");
        assert!(!add.kept(&format!("{sha}.json")).exists(), "{reason}");
    }
}

/// Proves: every end of the Mac's turn that gives no verdict is not checked, exit 1, with its
/// own words: not back, not reached (the run ended before the turn started), skipped (with the
/// coordinator's reason, in lower case), a run that was cancelled, one whose window ended, a run
/// done with the turn not ended, and a run whose answer holds no turn of the Mac; and a run
/// that cannot be read any more (refused, or no answer) says so and that the run goes on.
#[test]
fn every_other_end_of_the_turn_is_not_checked_with_its_own_words() {
    use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
    use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceHost, MaintenanceSkip};
    let add = linux("ends");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let skipped = MaintenanceHost {
        skipped_because: MaintenanceSkip::MdmConflict as i32,
        ..turn(Step::Skipped, "")
    };
    let other = MaintenanceHost {
        host_id: "mac-other".to_owned(),
        kinds: vec![MaintenanceKind::Probe as i32],
        ..MaintenanceHost::default()
    };
    let cases: Vec<(Reply, String)> = vec![
        (
            got(MaintenanceState::Halted, turn(Step::NotBack, "")),
            format!("{MAC} did not report back, so its turn of run {RUN} has not ended"),
        ),
        (
            got(MaintenanceState::Cancelled, turn(Step::NotReached, "")),
            format!("run {RUN} ended before {MAC}'s turn started"),
        ),
        (
            got(MaintenanceState::Done, skipped),
            format!("run {RUN} skipped {MAC}: mdm_conflict"),
        ),
        (
            got(MaintenanceState::Cancelled, turn(Step::Waiting, "")),
            format!("run {RUN} was cancelled"),
        ),
        (
            got(MaintenanceState::WindowEnded, turn(Step::Waiting, "")),
            format!("the window of run {RUN} ended before {MAC}'s turn did"),
        ),
        (
            got(MaintenanceState::Done, turn(Step::Checking, "")),
            format!("run {RUN} is done, with {MAC}'s turn at MAINTENANCE_HOST_STEP_CHECKING"),
        ),
        (
            Reply::new(
                200,
                crate::summary_rig::gateway_json(
                    "Maintenance",
                    "Get",
                    &hostkeeper_proto::hostkeeper::operator::v1::GetMaintenanceRunResponse {
                        run: None,
                        hosts: vec![other],
                    },
                )
                .to_string(),
            ),
            format!("run {RUN} holds no turn of {MAC}"),
        ),
        (
            refused("NOT_FOUND", "unknown_run"),
            format!(
                "Maintenance.Get of run {RUN} was refused: unknown_run (NOT_FOUND); the run goes on"
            ),
        ),
    ];
    for (reply, says) in cases {
        let farm = Farm::checking(&sha, vec![reply], no_probe());
        let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
        let ran = run_waiting(
            &add,
            &Waits::new(),
            server.port,
            &["xcode", "add", "--mac", MAC, &path],
        );
        assert_eq!(ran.code, 1, "{says}: {ran:?}");
        assert!(
            ran.stderr.contains(&format!("hk: not checked: {says}\n")),
            "{says}: {ran:?}"
        );
        assert!(asked(&server, CANCEL).is_empty() && asked(&server, REMOVE).is_empty());
        assert!(!add.kept(&format!("{sha}.json")).exists(), "{says}");
    }
    // No answer to the read of the run: the server hangs up.
    let farm = Farm::checking(&sha, vec![Reply::hang_up()], no_probe());
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(" run {RUN} goes on\n")),
        "{ran:?}"
    );
    // A read of the probe that is refused.
    let farm = Farm::checking(&sha, vec![done()], no_probe())
        .on(HOST, vec![refused("NOT_FOUND", "unknown_host")]);
    let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = farm.serve(held, &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: not checked: Hosts.Get of {MAC} was refused: unknown_host (NOT_FOUND)\n"
        )),
        "{ran:?}"
    );
}

/// Proves: when the removal of an archive that failed is itself refused, the run says so with the
/// `hk xcode remove --sha256 <sha256> --reason <why>` line, still prints `refused:` and exits 1;
/// and a `Maintenance.Cancel` that fails after a deferral is said with the line that stops the
/// run, the verdict still standing.
#[test]
fn a_removal_or_a_cancel_that_fails_is_said_with_the_line_that_does_it() {
    let add = linux("removal");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), probe_of(RUN, 1, &unsigned())).on(
        REMOVE,
        vec![refused("FAILED_PRECONDITION", "partial_locked")],
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
            .contains("\nrefused: fail: xcode_archive_not_apple: "),
        "{ran:?}"
    );
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: the archive failed the check on {MAC} (run {RUN}), but it could not be removed \
             from the store (Xcode.Remove was refused: partial_locked (FAILED_PRECONDITION)); \
             remove it with: hk xcode remove --sha256 {sha} --reason <why>\n"
        )),
        "{ran:?}"
    );
    let reason = "stage: exit 75, xcode_archive_mismatch: short";
    let deferred = got(MaintenanceState::Running, turn(Step::Deferred, reason));
    let farm = Farm::checking(&sha, vec![deferred], no_probe())
        .on(CANCEL, vec![refused("FAILED_PRECONDITION", "run_ended")]);
    let held = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    let server = farm.serve(held, &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: Maintenance.Cancel was refused: run_ended (FAILED_PRECONDITION); stop run {RUN} \
             with hk maintenance cancel --run-id {RUN} --reason <why>, or {MAC} is offered the \
             turn again\n"
        )),
        "{ran:?}"
    );
    assert_eq!(
        ran.stdout, "refused: fail: xcode_archive_mismatch: short\n",
        "{ran:?}"
    );
}

/// Proves: the token is the bearer of every call of the check (the schedule, each read of the run,
/// the probe and the cancel), and is in no line of stdout or stderr and not in the record the run
/// keeps.
#[test]
fn the_token_is_only_the_bearer_of_each_call_of_the_check() {
    let add = linux("token");
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let sha = sha256(&archive_bytes());
    let reason = "stage: exit 75, xcode_archive_mismatch: short";
    let steps = vec![
        at(Step::Staging),
        got(MaintenanceState::Running, turn(Step::Deferred, reason)),
    ];
    let farm = Farm::checking(&sha, steps, ok());
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(!ran.stdout.contains(TOKEN) && !ran.stderr.contains(TOKEN));
    let calls = farm_calls(&server);
    assert_eq!(calls.len(), 5, "{calls:?}");
    for call in server.seen() {
        assert_eq!(
            call.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str()),
            "{}",
            call.target
        );
        assert!(!call.text().contains(TOKEN), "{}", call.target);
    }
    let record = std::fs::read_to_string(add.kept(&format!("{sha}.json"))).expect("a record");
    assert!(!record.contains(TOKEN));
}
