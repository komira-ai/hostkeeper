//! The hook's `detail` in the step record (`Progress::detail`): kept under the reason of its
//! line, given with a finished verdict of that reason, and additive on disk.

use hostkeeper_hook::{Report, Step, Verdict};
use hostkeeper_steps::Record;
use hostkeeper_steps::record::{HookDetail, HookSel, Progress, SCHEMA, State};

fn record(state: State, progress: Progress) -> Record {
    Record {
        schema: SCHEMA,
        service: "js-agent".to_owned(),
        target_version: "2.0.0".to_owned(),
        artifact_digest: "abc".to_owned(),
        tarball_digest: "0123".to_owned(),
        previous_digest: None,
        generation: 1,
        params: "/state/params.toml".into(),
        step: Step::Prepare,
        attempt: 1,
        state,
        hook: HookSel::New,
        progress,
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

fn halted(reason: Option<&str>) -> State {
    State::Finished {
        verdict: Verdict::Halt,
        reason: reason.map(str::to_owned),
    }
}

fn kept(reason: Option<&str>, text: &str) -> Option<HookDetail> {
    Some(HookDetail {
        reason: reason.map(str::to_owned),
        text: text.to_owned(),
    })
}

/// Catches a line without a detail wiping the one kept, and a detail kept under the wrong
/// reason: each line with a detail replaces the kept one under its own reason (none when it gave
/// none), and a line with none leaves it.
#[test]
fn a_line_with_a_detail_replaces_the_kept_one_and_a_line_without_leaves_it() {
    let mut progress = Progress::default();
    progress.keep_detail(&Report::read(r#"{"reason":"bad_unit","detail":"first"}"#));
    assert_eq!(progress.detail, kept(Some("bad_unit"), "first"));
    progress.keep_detail(&Report::read(r#"{"reason":"drain_bound"}"#));
    progress.keep_detail(&Report::read(""));
    assert_eq!(progress.detail, kept(Some("bad_unit"), "first"));
    progress.keep_detail(&Report::read(r#"{"detail":"second"}"#));
    assert_eq!(progress.detail, kept(None, "second"));
}

/// Catches the prose of a step that succeeded being kept, and later shown beside a verdict it
/// does not explain: the line of a step that exited 0 keeps nothing, and that of a step that
/// exited otherwise, or did not exit (killed), keeps its detail; either way the report is the
/// line's.
#[test]
fn only_the_line_of_a_step_that_did_not_succeed_keeps_its_detail() {
    let said = r#"{"reason":"bad_unit","detail":"why"}"#;
    let mut progress = Progress::default();
    let report = progress.read_line(Some(0), Some(r#"{"detail":"migrated 3 rows"}"#));
    assert_eq!(report.detail.as_deref(), Some("migrated 3 rows"));
    assert_eq!(progress.detail, None);
    for code in [Some(1), Some(75), Some(76), None] {
        let mut progress = Progress::default();
        let report = progress.read_line(code, Some(said));
        assert_eq!(report.reason.as_deref(), Some("bad_unit"), "{code:?}");
        assert_eq!(progress.detail, kept(Some("bad_unit"), "why"), "{code:?}");
    }
    let mut progress = Progress::default();
    assert_eq!(progress.read_line(Some(1), None).detail, None);
    assert_eq!(progress.detail, None);
}

/// Catches a detail given with a verdict it does not explain: it is given when the record is
/// finished, not `Succeeded`, with the reason it was kept under (none and none included), and not
/// when the record is unfinished, succeeded, is finished with another reason (one the updater
/// gave of its own), or kept none.
#[test]
fn the_finished_detail_is_the_one_kept_under_the_verdicts_reason() {
    let under = |reason, state| {
        let progress = Progress {
            detail: kept(reason, "why"),
            ..Progress::default()
        };
        record(state, progress)
    };
    assert_eq!(
        under(Some("state_invalid"), halted(Some("state_invalid"))).finished_detail(),
        Some("why")
    );
    assert_eq!(under(None, halted(None)).finished_detail(), Some("why"));
    assert_eq!(
        under(Some("state_invalid"), halted(Some("artifact_changed"))).finished_detail(),
        None
    );
    assert_eq!(
        under(None, halted(Some("artifact_changed"))).finished_detail(),
        None
    );
    assert_eq!(
        under(Some("state_invalid"), State::Pending).finished_detail(),
        None
    );
    let succeeded = State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    assert_eq!(under(None, succeeded).finished_detail(), None);
    let none = record(halted(Some("state_invalid")), Progress::default());
    assert_eq!(none.finished_detail(), None);
}

/// Catches a format change that is not additive: a record with a kept detail writes it as
/// `progress.detail` (`reason` left out when none), a record without one writes no key, and a
/// record written before the field existed loads with none.
#[test]
fn the_detail_is_written_only_when_kept_and_an_older_record_loads_without_it() {
    let with = record(
        halted(Some("state_invalid")),
        Progress {
            detail: kept(Some("state_invalid"), "why"),
            ..Progress::default()
        },
    );
    let json = serde_json::to_value(&with).expect("the record as JSON");
    assert_eq!(
        json["progress"]["detail"],
        serde_json::json!({"reason": "state_invalid", "text": "why"})
    );
    let reasonless = HookDetail {
        reason: None,
        text: "why".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&reasonless).expect("the detail as JSON"),
        r#"{"text":"why"}"#
    );
    let without = record(halted(Some("state_invalid")), Progress::default());
    let json = serde_json::to_value(&without).expect("the record as JSON");
    assert!(json["progress"].get("detail").is_none(), "{json}");
    let older = serde_json::to_string(&without).expect("the record as text");
    let loaded: Record = serde_json::from_str(&older).expect("an older record loads");
    assert_eq!(loaded.progress.detail, None);
    assert_eq!(loaded, without);
}
