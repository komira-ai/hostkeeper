//! The answers of the coordinator that the check on a farm Mac must take as they come: a refusal
//! whose reason is no token, an answer that is not JSON, a number of 64 bits written as a number,
//! and a call that gets no answer at all, at each call of the check.

use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceHostStep as Step, MaintenanceState};

use crate::add_rig::{Store, archive_bytes, sha256};
use crate::farm_rig::{
    CANCEL, Farm, MAC, REMOVE, RUN, SCHEDULE, Waits, at, done, good_turn, got, linux, no_probe,
    probe_of, refused, run_waiting, turn,
};
use crate::rig::Reply;

/// What `Hosts.Get` answers, written by hand, for a probe ended at `probed` (JSON text) whose only
/// step is a passing verdict.
fn raw_probe(probed: &str) -> Reply {
    Reply::new(
        200,
        format!(
            "{{\"host\":{{\"nodeId\":\"{MAC}\",\"maintenance\":{{\"probe\":{{\"runId\":\"{RUN}\",\
             \"probedUnix\":{probed},\"steps\":[{{\"name\":\"verdict\",\"exitCode\":0,\
             \"output\":\"pass\"}}]}}}}}}}}"
        ),
    )
}

/// One run of `hk xcode add --mac <mac> <archive>` against the archive the store waits for, with
/// the farm's answers as `farm` has them; the run, and what was written in the home.
fn run(name: &str, farm: Farm) -> (crate::rig::Ran, String, crate::add_rig::Add) {
    let add = linux(name);
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let server = farm.serve(Store::default().waiting("27.2", "27B52"), &add.rig);
    let ran = run_waiting(
        &add,
        &Waits::new(),
        server.port,
        &["xcode", "add", "--mac", MAC, &path],
    );
    (ran, path, add)
}

/// Proves: a refusal of the schedule whose reason is not a token (`[a-z0-9_]{1,64}`) is said with
/// the coordinator's code and exits as any call does, but is not printed as `refused: <reason>`.
#[test]
fn a_reason_that_is_no_token_is_said_with_the_code_alone() {
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), no_probe())
        .on(SCHEDULE, vec![refused("INTERNAL", "Store Failed")]);
    let (ran, _, _) = run("not-a-token", farm);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: the coordinator answered INTERNAL\nhk: the archive stays in the store"),
        "{ran:?}"
    );
    assert!(!ran.stderr.contains("refused:"), "{ran:?}");
}

/// Proves: an answer that is not the coordinator's JSON ends the run at the call it answers, with
/// the call's name: at the schedule it is the run's failure (exit 1); at `Hosts.Get`, whether
/// the Mac's `probedUnix` is no number or the whole answer is no JSON, the archive is not checked.
#[test]
fn an_answer_that_is_not_json_is_a_failure_naming_the_call() {
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, good_turn(), no_probe())
        .on(SCHEDULE, vec![Reply::new(200, "not json")]);
    let (ran, _, _) = run("schedule-text", farm);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: the answer of Maintenance.Schedule is not the coordinator's JSON ("),
        "{ran:?}"
    );
    for answer in [raw_probe("\"soon\""), Reply::new(200, "[1, 2")] {
        let farm = Farm::checking(&sha, vec![done()], answer);
        let (ran, _, add) = run("probe-text", farm);
        assert_eq!(ran.code, 1, "{ran:?}");
        assert!(
            ran.stderr.contains(
                "hk: not checked: the answer of Hosts.Get is not the coordinator's JSON ("
            ),
            "{ran:?}"
        );
        assert!(!add.kept(&format!("{sha}.json")).exists());
    }
}

/// Proves: a `probedUnix` that the gateway wrote as a number, not a string, is read as one: the
/// check passes and the record has it.
#[test]
fn a_probe_time_written_as_a_number_is_read() {
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, vec![done()], raw_probe("1790000000"));
    let (ran, _, add) = run("probe-number", farm);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        crate::farm_rig::record_of(&add, &sha)["farm"]["probed_unix"],
        1_790_000_000_u64
    );
}

/// Proves: a call that gets no answer is the failure it is at each place the check makes one: the
/// probe of a done turn is not checked saying so; a cancel after a deferral is said with the line
/// that stops the run, and the deferral still stands; a removal of an archive that failed is said
/// with the line that removes it; and a read of the probe is made once.
#[test]
fn a_call_that_gets_no_answer_says_so_where_it_is_made() {
    let sha = sha256(&archive_bytes());
    let farm = Farm::checking(&sha, vec![done()], Reply::hang_up());
    let (ran, _, add) = run("probe-hangs", farm);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr
            .contains("hk: not checked: no answer from https://127.0.0.1:"),
        "{ran:?}"
    );
    assert!(!add.kept(&format!("{sha}.json")).exists());
    let reason = "stage: exit 75, xcode_archive_mismatch: short";
    let deferred = got(MaintenanceState::Running, turn(Step::Deferred, reason));
    let farm = Farm::checking(&sha, vec![at(Step::Staging), deferred], no_probe())
        .on(CANCEL, vec![Reply::hang_up()]);
    let (ran, _, _) = run("cancel-hangs", farm);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "; stop run {RUN} with hk maintenance cancel --run-id {RUN} --reason <why>, or {MAC} \
             is offered the turn again\n"
        )),
        "{ran:?}"
    );
    assert!(
        ran.stderr.contains("hk: no answer from https://127.0.0.1:"),
        "{ran:?}"
    );
    let farm = Farm::checking(&sha, vec![done()], probe_of(RUN, 1, &failing()))
        .on(REMOVE, vec![Reply::hang_up()]);
    let (ran, _, _) = run("remove-hangs", farm);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.contains(&format!(
            "hk: the archive failed the check on {MAC} (run {RUN}), but it could not be removed \
             from the store (no answer from https://127.0.0.1:"
        )),
        "{ran:?}"
    );
    assert!(
        ran.stderr
            .contains(&format!("hk xcode remove --sha256 {sha} --reason <why>"))
    );
}

/// The steps of an archive whose codesign failed.
fn failing() -> Vec<crate::farm_rig::Step<'static>> {
    vec![(
        "verdict",
        Some(1),
        "xcode_signature_invalid: codesign exited 1",
        false,
    )]
}
