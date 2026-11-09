//! The small pieces: the slots of a text, the parameters, a record's line, the gateway's JSON
//! and the machine's clock.

use std::collections::BTreeMap;

use hostkeeper_measure::clock::{Clock, SystemClock};
use hostkeeper_measure::coordinator::Answer;
use hostkeeper_measure::fleet::{self, Device, Entry, HostView, ServiceView};
use hostkeeper_measure::params::{ParamError, Params};
use hostkeeper_measure::record::{self, Record};
use hostkeeper_measure::template::{self, Piece, Slot, TemplateError, id_ok};
use serde_json::json;

use crate::plans::BASE;
use crate::support;

#[test]
fn a_text_splits_into_literal_pieces_and_slots() {
    assert_eq!(template::pieces(""), Ok(vec![]));
    assert_eq!(
        template::pieces("a {b} c{d:e}"),
        Ok(vec![
            Piece::Text("a ".to_owned()),
            Piece::Slot("b".to_owned()),
            Piece::Text(" c".to_owned()),
            Piece::Slot("d:e".to_owned()),
        ])
    );
    assert_eq!(
        template::pieces("{x}"),
        Ok(vec![Piece::Slot("x".to_owned())])
    );
    assert_eq!(
        template::pieces("a {b"),
        Err(TemplateError::Unclosed {
            text: "a {b".to_owned()
        })
    );
    assert_eq!(
        template::pieces("a } {b}"),
        Err(TemplateError::Unopened {
            text: "a } {b}".to_owned()
        })
    );
    assert_eq!(
        template::pieces("}"),
        Err(TemplateError::Unopened {
            text: "}".to_owned()
        })
    );
    for bad in ["{}", "{A}", "{a b}", &format!("{{{}}}", "a".repeat(65))] {
        assert!(
            matches!(template::pieces(bad), Err(TemplateError::BadSlot { .. })),
            "{bad}"
        );
    }
    assert!(template::pieces(&format!("{{{}}}", "a".repeat(64))).is_ok());
    assert_eq!(
        TemplateError::BadSlot {
            text: "{A}".to_owned(),
            slot: "A".to_owned()
        }
        .to_string(),
        "\"{A}\": the slot {A} is not 1 to 64 bytes of [a-z0-9.:-]"
    );
    assert_eq!(
        template::slots("{a} and {b}"),
        Ok(vec!["a".to_owned(), "b".to_owned()])
    );
    let mut fill = |s: &str| -> Result<String, TemplateError> { Ok(s.to_uppercase()) };
    assert_eq!(
        template::render("x{a}y{b}", &mut fill),
        Ok("xAyB".to_owned())
    );
    assert!(template::render("x{", &mut fill).is_err());
}

#[test]
fn a_slot_of_a_step_is_a_host_a_repetition_or_a_parameter_read_three_ways() {
    assert_eq!(Slot::of("host"), Some(Slot::Host));
    assert_eq!(Slot::of("rep"), Some(Slot::Rep));
    assert_eq!(Slot::of("version"), Some(Slot::Param("version".to_owned())));
    assert_eq!(Slot::of("file:index"), Some(Slot::File("index".to_owned())));
    assert_eq!(
        Slot::of("sha256:index"),
        Some(Slot::Sha256("index".to_owned()))
    );
    for bad in ["file:", "sha256:", "file:X", "a.b", "1a", "x:y"] {
        assert_eq!(Slot::of(bad), None, "{bad}");
    }
    assert_eq!(Slot::Host.param(), None);
    assert_eq!(Slot::Rep.param(), None);
    assert_eq!(Slot::File("i".to_owned()).param(), Some("i"));
    assert!(id_ok("a-1", 3));
    assert!(!id_ok("a-12", 3));
    assert!(!id_ok("", 3));
    assert!(!id_ok("-a", 3));
}

#[test]
fn parameters_are_read_and_held_to_the_plan() {
    let items = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let p = Params::parse(&items(&["a=1", "b@2=x=y", "b@1="])).expect("parameters");
    assert_eq!(p.get("a", 7), Some("1"));
    assert_eq!(p.get("b", 2), Some("x=y"));
    assert_eq!(p.get("b", 1), Some(""));
    assert_eq!(p.get("b", 3), None);
    assert_eq!(
        Params::parse(&items(&["nothing"])),
        Err(ParamError::Shape("nothing".to_owned()))
    );
    assert_eq!(
        Params::parse(&items(&["a@x=1"])),
        Err(ParamError::Shape("a@x=1".to_owned()))
    );
    assert_eq!(
        Params::parse(&items(&["a=1", "a=2"])),
        Err(ParamError::Twice("a".to_owned()))
    );
    assert_eq!(
        Params::parse(&items(&["a@1=1", "a@1=2"])),
        Err(ParamError::Twice("a@1".to_owned()))
    );
    let plan = support::plan(BASE);
    let check = |v: &[&str]| Params::parse(&items(v)).expect("parameters").check(&plan);
    assert_eq!(check(&["version@1=a", "version@2=b"]), Ok(()));
    let cases: [(&[&str], &str); 6] = [
        (
            &["version=a"],
            "\"version\" differs per repetition: give it as version@<repetition>=value",
        ),
        (&["other=a"], "the plan has no parameter \"other\""),
        (&["other@1=a"], "the plan has no parameter \"other\""),
        (
            &["version@0=a"],
            "repetition 0 of \"version\": the plan has repetitions 1 to 2",
        ),
        (
            &["version@3=a"],
            "repetition 3 of \"version\": the plan has repetitions 1 to 2",
        ),
        (&["version@1=a"], "no value of \"version\" for repetition 2"),
    ];
    for (given, want) in cases {
        assert_eq!(check(given).expect_err(want).to_string(), want);
    }
    let every = support::plan(&BASE.replace("per_repetition = true", "per_repetition = false"));
    let given = Params::parse(&items(&["version@1=a"])).expect("parameters");
    assert_eq!(
        given.check(&every).expect_err("per repetition").to_string(),
        "\"version\" is the same in every repetition: give it as version=value"
    );
}

#[test]
fn a_record_is_one_line_of_json_and_reads_back() {
    let full = Record {
        rep: 2,
        step: "s".to_owned(),
        event: "e".to_owned(),
        host: Some("host-1".to_owned()),
        runner_ms: 5,
        coordinator_unix: Some(7),
        detail: BTreeMap::from([("k".to_owned(), "v".to_owned())]),
    };
    assert_eq!(
        full.line(),
        "{\"coordinator_unix\":7,\"detail\":{\"k\":\"v\"},\"event\":\"e\",\"host\":\"host-1\",\"rep\":2,\"runner_ms\":5,\"step\":\"s\"}\n"
    );
    let bare = Record {
        host: None,
        coordinator_unix: None,
        detail: BTreeMap::new(),
        ..full.clone()
    };
    assert_eq!(
        bare.line(),
        "{\"event\":\"e\",\"rep\":2,\"runner_ms\":5,\"step\":\"s\"}\n"
    );
    let text = format!("{}\n  \n{}", full.line(), bare.line());
    assert_eq!(record::parse(&text).expect("records"), vec![full, bare]);
    let err = record::parse("\n{\"rep\":1}\n").expect_err("a short record");
    assert_eq!(err.line, 2);
    assert_eq!(
        err.to_string(),
        "records line 2: missing field `step` at line 1 column 9"
    );
    assert!(
        record::parse("{\"rep\":1,\"step\":\"s\",\"event\":\"e\",\"runner_ms\":1,\"x\":1}")
            .is_err()
    );
}

#[test]
fn the_gateway_json_reads_with_absent_fields_as_their_defaults() {
    assert_eq!(fleet::number(Some(&json!("12"))), Some(12));
    assert_eq!(fleet::number(Some(&json!(12))), Some(12));
    assert_eq!(fleet::number(Some(&json!("x"))), None);
    assert_eq!(fleet::number(Some(&json!(-1))), None);
    assert_eq!(fleet::number(None), None);
    let host = HostView::read(&json!({
        "nodeId": "n",
        "live": true,
        "lastHeartbeatUnix": "9",
        "services": [
            {"service": "a", "installedVersion": "v1", "holdsSlot": true, "health": "HEALTH_HEALTHY",
             "lastUpdate": {"target": "v1", "outcome": "OUTCOME_SUCCEEDED"}},
            {"service": "b"},
            {}
        ],
        "heartbeat": {"services": [{"name": "a", "updating": "v2"}, {"name": "c"}]}
    }));
    assert_eq!(
        host,
        HostView {
            node: "n".to_owned(),
            live: true,
            last_heartbeat: Some(9),
            first_heartbeat: None,
            first_serving: None,
            services: vec![
                ServiceView {
                    service: "a".to_owned(),
                    installed: Some("v1".to_owned()),
                    holds_slot: true,
                    health: "HEALTH_HEALTHY".to_owned(),
                    last_target: Some("v1".to_owned()),
                    last_outcome: Some("OUTCOME_SUCCEEDED".to_owned()),
                    updating: Some("v2".to_owned()),
                },
                ServiceView {
                    service: "b".to_owned(),
                    ..ServiceView::default()
                },
                ServiceView::default(),
            ],
        }
    );
    assert_eq!(host.service("b").map(|s| s.service.as_str()), Some("b"));
    assert_eq!(host.service("z"), None);
    assert_eq!(HostView::read(&json!(null)), HostView::default());
    assert_eq!(fleet::hosts_page(&json!({})), (vec![], String::new()));
    assert_eq!(
        fleet::audit_page(
            &json!({"entries": [{"seq": "3", "action": "x", "nodeId": "n", "atUnix": "4", "byName": "b"}, {}], "nextSeq": "3"})
        ),
        (
            vec![
                Entry {
                    seq: 3,
                    action: "x".to_owned(),
                    node: Some("n".to_owned()),
                    at_unix: Some(4),
                    by_name: Some("b".to_owned()),
                },
                Entry::default()
            ],
            3
        )
    );
    assert_eq!(fleet::audit_page(&json!({})), (vec![], 0));
    assert_eq!(
        fleet::devices_page(
            &json!({"devices": [{"enrollmentId": "d", "enrolledSinceUnix": "5"}, {}], "nextPageToken": "t"})
        ),
        (
            vec![
                Device {
                    id: "d".to_owned(),
                    enrolled_since: Some(5)
                },
                Device::default()
            ],
            "t".to_owned()
        )
    );
}

#[test]
fn a_refusal_is_read_from_its_line_of_stderr() {
    let answer = |stderr: &str| Answer {
        code: 2,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    };
    assert_eq!(
        answer("x\nrefused: held_bad \n").refused(),
        Some("held_bad")
    );
    assert_eq!(answer("usage: no\n").refused(), None);
}

#[test]
fn the_machines_clock_stamps_unix_milliseconds_and_never_goes_back() {
    let clock = SystemClock::default();
    // A stamp after the year the fake clock of these tests starts in is unix milliseconds.
    assert!(clock.stamp_ms() > 1_000_000_000_000);
    let first = clock.mono_ms();
    clock.sleep_ms(0);
    assert!(clock.mono_ms() >= first);
    assert!(format!("{:?}", SystemClock::new()).starts_with("SystemClock"));
}
