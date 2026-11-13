use serde_json::json;

use super::*;
use crate::force::ForceTargets;
use crate::registry::Digest;
use crate::store::conformance_tests::{desired_at, normal, rich_ops, rich_verdicts};
use crate::testutil::version;

fn key(parts: &[&str]) -> Vec<KeyVal> {
    parts
        .iter()
        .map(|p| KeyVal::Text((*p).to_owned()))
        .collect()
}

fn text(s: &str) -> Val {
    Val::Text(s.to_owned())
}

/// The operations state a load gives back for `state`: its ring left empty, entries at their
/// default absent.
fn as_loaded(state: &OpsState) -> OpsState {
    let mut state = normal(state.clone());
    state.audit.clear();
    state
}

/// The state read back from its rows, its default entries (which a load may make for a service
/// it reads a row of, and the code reads as absent) left out.
fn round(state: &OpsState) -> OpsState {
    normal(ops_of(&ops_rows(state).expect("the rows")).expect("the state back"))
}

fn report() -> serde_json::Value {
    json!({"target": "2.0", "outcome": "failed", "phase": "boot", "reason": "x"})
}

/// An operations state with something in every table, in every shape a field takes.
fn full_ops() -> OpsState {
    let mut state = rich_ops();
    let kbf = state.assignments.get_mut("kbf").expect("kbf assignments");
    kbf.host_min_versions
        .insert("s9".to_owned(), version("1.5"));
    kbf.uninstall_generation = 4;
    state.assignments.insert(
        "only-assigned".to_owned(),
        serde_json::from_value(
            json!({"hosts": {"h1": "include"}, "uninstall": {"h1": 1}, "uninstall_generation": 1}),
        )
        .expect("assignments"),
    );
    state.holds.push(HoldMark {
        service: "kbf".to_owned(),
        scope: HoldScope::Role("storage".to_owned()),
        reason: "role".to_owned(),
        by: "op".to_owned(),
        at_unix: 3,
    });
    // One record with an approval window, used; the other with neither.
    state.inventory[0].approve_until_unix = 1_900_000_100;
    state.inventory[0].platform = Some(hostkeeper_wire::Platform::LinuxArm64);
    state.inventory[1].platform = Some(hostkeeper_wire::Platform::LinuxX86_64);
    state.inventory[0].used = Some(crate::inventory::RecordUse {
        request_id: "ab".repeat(32),
        at_unix: 1_900_000_050,
    });
    // A service whose only mark is the repository it is installed from has a row of its own.
    state.services.insert(
        "only-repository".to_owned(),
        crate::ops::ServiceOps {
            repository: Some("private".to_owned()),
            ..crate::ops::ServiceOps::default()
        },
    );
    // A package whose trusted publishers were all removed keeps a row: an empty list is not none.
    state.services.insert(
        "emptied".to_owned(),
        crate::ops::ServiceOps {
            trusted_publishers: Some(Vec::new()),
            ..crate::ops::ServiceOps::default()
        },
    );
    let marks = state.services.get_mut("kbf").expect("kbf marks");
    marks.repository = Some("komira-next".to_owned());
    marks.trusted_publishers = Some(vec![crate::trusted_publishers::TrustedPublisher {
        repository: "acme/kbf".to_owned(),
        workflow: ".github/workflows/release.yml".to_owned(),
        git_ref: "refs/heads/main".to_owned(),
        environment: "prod".to_owned(),
        repository_id: "42".to_owned(),
        called_workflows: vec!["acme/hk/.github/workflows/set.yml@abc".to_owned()],
        host_groups: Some(vec!["lab".to_owned()]),
    }]);
    marks.force = Some(ForceRecord {
        version: version("2.0"),
        generation: 2,
        reason: "now".to_owned(),
        by: "op".to_owned(),
        digest: Digest::parse(&"ab".repeat(32)).expect("a digest"),
        release_index: None,
        halt_generation: 3,
        targets: ForceTargets::Role("storage".to_owned()),
        include_held: true,
        hosts: vec!["s1".to_owned()],
    });
    marks.halt.as_mut().expect("a halt").report = None;
    marks.halt_acked[0].report = None;
    state.services.insert(
        "bare".to_owned(),
        serde_json::from_value(json!({"last_good": "0.9"})).expect("marks"),
    );
    let halt = |origin| Halt {
        node_id: "s1".to_owned(),
        check: "disk".to_owned(),
        target: version("1.0"),
        report: None,
        origin,
    };
    state.services.get_mut("bare").expect("bare").halt = Some(halt(HaltOrigin::Operator));
    state.services.entry("hooked".to_owned()).or_default().halt = Some(halt(HaltOrigin::Hook));
    state.rollouts.insert(
        "web".to_owned(),
        serde_json::from_value(json!({"version": "1.0", "entered": 2, "done": []}))
            .expect("a rollout"),
    );
    state.inventory[1].host_group = "web".to_owned();
    state.host_groups.0.insert(
        "web".parse().expect("a group name"),
        std::collections::BTreeMap::from([(
            "tierstore".to_owned(),
            crate::host_groups::tests::every_mark(),
        )]),
    );
    state
}

/// Proves: an operations state with something in every table (every scope of a hold and of an
/// assignment, a force, a halt and acknowledgements with and without their reports, a rollout
/// paused and not, plans with and without hosts done, a service known only by its assignments)
/// is written as rows and read back as the same state but for its ring, every table of the
/// part is written, and an entry at its default is not stored.
///
/// Catches: a field a table drops or garbles, a list that comes back out of order, and a
/// default entry that is stored and read back as something else.
#[test]
fn an_operations_state_round_trips_through_its_rows() {
    let state = full_ops();
    let tables = ops_rows(&state).expect("the rows");
    let empty: Vec<&str> = OPS
        .iter()
        .filter(|t| tables.get(t.name).is_none_or(Rows::is_empty))
        .map(|t| t.name)
        .collect();
    assert_eq!(empty, Vec::<&str>::new(), "tables without a row");
    assert_eq!(
        tables["services"].keys().cloned().collect::<Vec<_>>(),
        vec![
            key(&["bare"]),
            key(&["emptied"]),
            key(&["kbf"]),
            key(&["only-assigned"]),
            key(&["only-repository"])
        ],
        "the default entry `idle` is not stored"
    );
    assert_eq!(round(&state), as_loaded(&state));
}

/// Proves: an empty operations state has no rows and reads back empty, and a rollout whose
/// pause has a name but no reason (or the reverse) reads back unpaused.
///
/// Catches: a row made for nothing, and a half pause read as a pause.
#[test]
fn an_empty_operations_state_has_no_rows_and_a_half_pause_reads_unpaused() {
    let tables = ops_rows(&OpsState::default()).expect("the rows");
    assert!(tables.values().all(Rows::is_empty));
    assert_eq!(ops_of(&Tables::new()).expect("empty"), OpsState::default());
    let half = Tables::from([(
        "rollouts",
        Rows::from([(
            key(&["web"]),
            vec![Val::Null, Val::Int(1), text("op"), Val::Null],
        )]),
    )]);
    let state = ops_of(&half).expect("a half pause");
    assert_eq!(state.rollouts["web"].paused, None);
    assert_eq!(state.rollouts["web"].entered, 1);
}

/// Proves: where the code holds one key twice (an exclusion, a hold) the first stands, as the
/// first line of a file would, and a load reads one row for it.
///
/// Catches: the later duplicate overwriting the first.
#[test]
fn the_first_of_a_key_held_twice_stands() {
    let mut state = OpsState::default();
    let exclusion = |reason: &str| Exclusion {
        service: "kbf".to_owned(),
        node_id: "s2".to_owned(),
        reason: reason.to_owned(),
    };
    state.excluded = vec![exclusion("first"), exclusion("second")];
    let hold = |reason: &str| HoldMark {
        service: "kbf".to_owned(),
        scope: HoldScope::Fleet,
        reason: reason.to_owned(),
        by: "op".to_owned(),
        at_unix: 1,
    };
    state.holds = vec![hold("first"), hold("second")];
    let back = round(&state);
    assert_eq!(back.excluded, vec![exclusion("first")]);
    assert_eq!(back.holds, vec![hold("first")]);
}

/// A way to put a counter into a state, named.
type Break = (&'static str, fn(&mut OpsState, u64));

/// Proves: a counter above `i64::MAX` is refused at the write with the text that names the
/// number, wherever the state holds one (a generation, a hold's time, an inventory time, an
/// uninstall generation, the audit line's numbers).
///
/// Catches: a counter written wrapped to a negative number.
#[test]
fn a_counter_above_the_largest_the_database_keeps_is_refused() {
    let big = u64::MAX;
    let want =
        "database:counter: 18446744073709551615 is above the largest counter the database keeps";
    assert_eq!(int(i64::MAX as u64).expect("the largest"), i64::MAX);
    let err = int(big).expect_err("above");
    assert_eq!(err.to_string(), want);
    let breaks: [Break; 6] = [
        ("halt generation", |s, n| {
            s.services.get_mut("kbf").expect("kbf").halt_generation = n
        }),
        ("force generation", |s, n| {
            s.services.get_mut("kbf").expect("kbf").force_generation = n
        }),
        ("uninstall generation", |s, n| {
            s.assignments
                .get_mut("kbf")
                .expect("kbf")
                .uninstall_generation = n;
        }),
        ("uninstall intent", |s, n| {
            s.assignments
                .get_mut("kbf")
                .expect("kbf")
                .uninstall
                .insert("s5".to_owned(), n);
        }),
        ("hold time", |s, n| s.holds[0].at_unix = n),
        ("inventory time", |s, n| s.inventory[0].at_unix = n),
    ];
    for (what, set) in breaks {
        let mut state = rich_ops();
        set(&mut state, big);
        let err = ops_rows(&state).expect_err(what);
        assert_eq!(err.to_string(), want, "{what}");
    }
}

fn refusal(table: &'static str, row: &[&str], values: Vec<Val>) -> String {
    let keys: Vec<KeyVal> = row
        .iter()
        .map(|part| match part.parse::<i64>() {
            Ok(n) => KeyVal::Int(n),
            Err(_) => KeyVal::Text((*part).to_owned()),
        })
        .collect();
    let tables = Tables::from([(table, Rows::from([(keys, values)]))]);
    match ops_of(&tables) {
        Err(e) => e.to_string(),
        Ok(_) => "accepted".to_owned(),
    }
}

fn why(table: &str, row: &str, why: &str) -> String {
    format!("database:{table}: the row {row}: {why}")
}

/// Proves: a row of the operations tables that this build cannot read is refused with a text
/// that names its table and key and what is wrong (a key that is not text, a version that does
/// not parse, a negative counter, a counter that is not a number, a value of the wrong type, a
/// required value missing, a scope, entry, origin or kind it does not know, a list that is not
/// a list, a report that is empty or does not parse, stages entered above `u32`), and a row
/// whose list is short reads its missing values as empty.
///
/// Catches: a load that guesses at a row it cannot read, or that names no table and key.
#[test]
fn a_row_this_build_cannot_read_is_refused_naming_its_table_and_key() {
    let bad_version = Version::parse("nope")
        .expect_err("not a version")
        .to_string();
    let zero = || Val::Int(0);
    let svc = |values: Vec<Val>| refusal("services", &["kbf"], values);
    let mut cases: Vec<(String, String)> = vec![(
        refusal("services", &["7"], vec![]),
        why("services", "7", "a key column is not text"),
    )];
    cases.push((
        svc(vec![text("nope"), zero(), zero(), zero(), Val::Null]),
        why("services", "kbf", &bad_version),
    ));
    cases.push((
        svc(vec![Val::Int(1), zero(), zero(), zero(), Val::Null]),
        why("services", "kbf", "a column is not text"),
    ));
    cases.push((
        svc(vec![Val::Null, Val::Int(-1), zero(), zero(), Val::Null]),
        why("services", "kbf", "a counter is negative"),
    ));
    cases.push((
        svc(vec![Val::Null, text("1"), zero(), zero(), Val::Null]),
        why("services", "kbf", "a counter is not a number"),
    ));
    cases.push((
        svc(vec![Val::Null, zero(), zero(), zero(), Val::Int(3)]),
        why("services", "kbf", "a column is not JSON"),
    ));
    let not_a_force = svc(vec![Val::Null, zero(), zero(), zero(), Val::Json(json!(5))]);
    assert!(
        not_a_force.starts_with("database:services: the row kbf: invalid type: integer `5`"),
        "{not_a_force}"
    );
    cases.push((
        refusal(
            "bad_versions",
            &["kbf", "nope"],
            vec![Val::Bool(false), Val::Int(0)],
        ),
        why("bad_versions", "kbf/nope", &bad_version),
    ));
    cases.push((
        refusal(
            "bad_versions",
            &["kbf", "1.0"],
            vec![Val::Bool(false), Val::Null],
        ),
        why("bad_versions", "kbf/1.0", "a position is not a number"),
    ));
    cases.push((
        refusal("failure_acks", &["kbf", "s1"], vec![Val::Null, Val::Int(0)]),
        why("failure_acks", "kbf/s1", "the report is empty"),
    ));
    cases.push((
        refusal(
            "failure_acks",
            &["kbf", "s1"],
            vec![Val::Json(report()), Val::Null],
        ),
        why("failure_acks", "kbf/s1", "a position is not a number"),
    ));
    cases.push((
        refusal("host_labels", &["s1", "rack"], vec![]),
        why(
            "host_labels",
            "s1/rack",
            "a column that is required is empty",
        ),
    ));
    cases.push((
        refusal("exclusions", &["kbf", "s1"], vec![Val::Null, Val::Int(0)]),
        why("exclusions", "kbf/s1", "a column that is required is empty"),
    ));
    cases.push((
        refusal("exclusions", &["kbf", "s1"], vec![text("r"), text("0")]),
        why("exclusions", "kbf/s1", "a position is not a number"),
    ));
    let hold = |scope: &str, values: Vec<Val>| refusal("holds", &["kbf", scope, "t"], values);
    cases.push((
        hold(
            "galaxy",
            vec![text("r"), text("b"), Val::Int(1), Val::Int(0)],
        ),
        why(
            "holds",
            "kbf/galaxy/t",
            "the scope \"galaxy\" is not one this build reads",
        ),
    ));
    cases.push((
        hold(
            "host",
            vec![text("r"), text("b"), Val::Int(-1), Val::Int(0)],
        ),
        why("holds", "kbf/host/t", "a counter is negative"),
    ));
    cases.push((
        hold("host", vec![text("r"), text("b"), Val::Int(1), Val::Null]),
        why("holds", "kbf/host/t", "a position is not a number"),
    ));
    cases.push((
        refusal("assignments", &["kbf", "role", "r"], vec![text("maybe")]),
        why(
            "assignments",
            "kbf/role/r",
            "the entry \"maybe\" is not one this build reads",
        ),
    ));
    cases.push((
        refusal(
            "assignments",
            &["kbf", "galaxy", "r"],
            vec![text("include")],
        ),
        why(
            "assignments",
            "kbf/galaxy/r",
            "the scope \"galaxy\" is not one this build reads",
        ),
    ));
    cases.push((
        refusal("min_versions", &["kbf", "role", "r"], vec![Val::Null]),
        why("min_versions", "kbf/role/r", "the version is empty"),
    ));
    cases.push((
        refusal("min_versions", &["kbf", "galaxy", "r"], vec![text("1.0")]),
        why(
            "min_versions",
            "kbf/galaxy/r",
            "the scope \"galaxy\" is not one this build reads",
        ),
    ));
    cases.push((
        refusal("uninstall_intents", &["kbf", "s1"], vec![Val::Int(-2)]),
        why("uninstall_intents", "kbf/s1", "a counter is negative"),
    ));
    let inventory_used = |macs: Val, used: (Val, Val)| {
        vec![
            text("r"),
            macs,
            text("b"),
            text("s"),
            text("n"),
            text("op"),
            Val::Int(1),
            Val::Int(0),
            Val::Int(0),
            used.0,
            used.1,
            text(""),
        ]
    };
    let inventory = |macs: Val| inventory_used(macs, (Val::Null, Val::Null));
    let macs = || Val::Texts(Vec::new());
    for half in [(text("ab"), Val::Null), (Val::Null, Val::Int(5))] {
        cases.push((
            refusal("inventory", &["h1"], inventory_used(macs(), half)),
            why("inventory", "h1", "a record's use is half there"),
        ));
    }
    cases.push((
        refusal(
            "inventory",
            &["h1"],
            inventory_used(macs(), (text("ab"), Val::Int(-1))),
        ),
        why("inventory", "h1", "a counter is negative"),
    ));
    let mut darwin = inventory(macs());
    darwin[11] = text("darwin-arm64");
    cases.push((
        refusal("inventory", &["h1"], darwin),
        why(
            "inventory",
            "h1",
            "the platform \"darwin-arm64\" is not one a record states",
        ),
    ));
    cases.push((
        refusal("inventory", &["h1"], inventory(text("aa"))),
        why("inventory", "h1", "the data MACs are not a list"),
    ));
    cases.push((
        refusal("inventory", &["h1"], inventory(Val::Null)),
        why("inventory", "h1", "the data MACs are not a list"),
    ));
    let halt =
        |origin: &str, target: Val| vec![text("s1"), text("disk"), target, text(origin), Val::Null];
    cases.push((
        refusal("halts", &["kbf"], halt("alien", text("2.0"))),
        why(
            "halts",
            "kbf",
            "the origin \"alien\" is not one this build reads",
        ),
    ));
    cases.push((
        refusal("halts", &["kbf"], halt("hook", Val::Null)),
        why("halts", "kbf", "the target is empty"),
    ));
    cases.push((
        refusal("halts", &["kbf"], vec![]),
        why("halts", "kbf", "a column that is required is empty"),
    ));
    cases.push((
        refusal(
            "halt_reports",
            &["kbf", "weird", "s1"],
            vec![text("2.0"), Val::Null, Val::Int(0)],
        ),
        why(
            "halt_reports",
            "kbf/weird/s1",
            "the kind \"weird\" is not one this build reads",
        ),
    ));
    cases.push((
        refusal(
            "halt_reports",
            &["kbf", "seen", "s1"],
            vec![Val::Null, Val::Null, Val::Int(0)],
        ),
        why("halt_reports", "kbf/seen/s1", "the target is empty"),
    ));
    cases.push((
        refusal(
            "halt_reports",
            &["kbf", "seen", "s1"],
            vec![text("2.0"), Val::Null, Val::Null],
        ),
        why("halt_reports", "kbf/seen/s1", "a position is not a number"),
    ));
    cases.push((
        refusal(
            "rollouts",
            &["web"],
            vec![
                Val::Null,
                Val::Int(i64::from(u32::MAX) + 1),
                Val::Null,
                Val::Null,
            ],
        ),
        why("rollouts", "web", "the stages entered are too many"),
    ));
    cases.push((
        refusal(
            "rollouts",
            &["web"],
            vec![Val::Null, Val::Int(-1), Val::Null, Val::Null],
        ),
        why("rollouts", "web", "a counter is negative"),
    ));
    cases.push((
        refusal(
            "rollouts",
            &["web"],
            vec![text("nope"), Val::Int(1), Val::Null, Val::Null],
        ),
        why("rollouts", "web", &bad_version),
    ));
    for (got, want) in cases {
        assert_eq!(got, want);
    }
}

/// Proves: a bad version in a halt report, an unreadable report in a failure acknowledgement
/// and a key of a table that is not text are each refused at load rather than read as empty.
///
/// Catches: a report that does not parse being dropped quietly.
#[test]
fn a_report_that_does_not_parse_is_refused_at_load() {
    let unreadable = refusal(
        "failure_acks",
        &["kbf", "s1"],
        vec![Val::Json(json!({"x": 1})), Val::Int(0)],
    );
    assert!(
        unreadable.starts_with("database:failure_acks: the row kbf/s1: "),
        "{unreadable}"
    );
    let halt_report = refusal(
        "halt_reports",
        &["kbf", "acked", "s1"],
        vec![text("2.0"), Val::Json(json!([])), Val::Int(0)],
    );
    assert!(
        halt_report.starts_with("database:halt_reports: the row kbf/acked/s1: "),
        "{halt_report}"
    );
    let bytes = Tables::from([(
        "host_labels",
        Rows::from([(
            vec![KeyVal::Int(3), KeyVal::Text("k".to_owned())],
            vec![text("v")],
        )]),
    )]);
    assert_eq!(
        ops_of(&bytes)
            .expect_err("a key that is not text")
            .to_string(),
        "database:host_labels: the row 3/k: a key column is not text"
    );
}

fn with_index() -> desired::Held {
    let mut held = desired_at("1.0", 'a');
    let body = json!({
        "version": "2.0",
        "min_up": {"storage": 0, "frontend": 0},
        "release_index": {
            "digest": "ab".repeat(32),
            "packages": {"linux-x86_64": "cd".repeat(32), "macos-arm64": "ef".repeat(32)}
        }
    });
    held.desired.insert(
        "indexed".to_owned(),
        desired::Desired::parse(body.to_string().as_bytes())
            .expect("a desired state with an index"),
    );
    held
}

/// Proves: the desired state, with a package digest registry and with a release index, is
/// written as the rows of `desired` and `desired_digests` and read back as the same state, and
/// an empty state has no rows.
///
/// Catches: a digest, an index or a minimum a row loses, and a row kept for a state that holds
/// nothing.
#[test]
fn the_desired_state_round_trips_through_its_rows() {
    let held = with_index();
    let tables = desired_rows(&held).expect("the rows");
    assert_eq!(tables["desired"].len(), 2);
    assert_eq!(tables["desired_digests"].len(), 1);
    let indexed = &tables["desired"][&key(&["indexed"])];
    assert!(
        matches!(indexed[4], Val::Json(_)),
        "the index is a document"
    );
    assert_eq!(indexed[3], Val::Null, "no digest beside an index");
    assert_eq!(desired_of(&tables).expect("the state back"), held);
    let none = desired_rows(&desired::Held::default()).expect("no rows");
    assert!(none.values().all(Rows::is_empty));
    assert_eq!(desired_of(&none).expect("empty"), desired::Held::default());
}

/// Proves: rows the desired-state reader refuses (a version that does not parse, a digest row
/// with no service row, a service with no version) are refused at load naming the table, and
/// rows with a key that is not text, or values of another type than the column's, are passed
/// over rather than guessed at.
///
/// Catches: a load that is less strict than a start on the file.
#[test]
fn desired_rows_the_reader_refuses_are_refused_at_load() {
    let held = desired_at("1.0", 'a');
    let good = desired_rows(&held).expect("the rows");
    let mut bad_version = good.clone();
    bad_version
        .get_mut("desired")
        .expect("desired")
        .get_mut(&key(&["kbf"]))
        .expect("kbf")[0] = text("nope");
    let err = desired_of(&bad_version).expect_err("a bad version");
    assert!(err.to_string().starts_with("database:desired: "), "{err}");
    assert!(matches!(err, StoreError::Invalid { .. }));

    let mut orphan = good.clone();
    orphan.get_mut("desired_digests").expect("digests").insert(
        key(&["ghost", "1.0"]),
        vec![Val::Json(json!({"digest": "x"}))],
    );
    assert!(desired_of(&orphan).is_err());

    let mut no_release = good.clone();
    no_release
        .get_mut("desired_digests")
        .expect("digests")
        .insert(key(&["kbf", "2.0"]), vec![]);
    assert!(desired_of(&no_release).is_err());

    // A value of another type than the column's is left out, as the file leaves out a field it
    // does not hold.
    let mut passed_over = good.clone();
    passed_over
        .get_mut("desired")
        .expect("desired")
        .get_mut(&key(&["kbf"]))
        .expect("kbf")[3] = Val::Int(5);
    assert_eq!(desired_of(&passed_over).expect("read"), held);
    // A key that is not text names no service the reader takes.
    let mut not_text = good.clone();
    not_text
        .get_mut("desired_digests")
        .expect("digests")
        .insert(vec![KeyVal::Int(1), KeyVal::Int(2)], vec![Val::Null]);
    assert!(desired_of(&not_text).is_err());
    let mut int_key = good;
    int_key
        .get_mut("desired")
        .expect("desired")
        .insert(vec![KeyVal::Int(9)], vec![]);
    assert!(desired_of(&int_key).is_err());
}

/// Proves: the verdicts (a standing one, counted ones out of host order, a service with only
/// one of the two) are written as rows and read back as the same verdicts, and no verdicts is no
/// rows.
///
/// Catches: a verdict a row loses, and counted verdicts that come back reordered.
#[test]
fn the_verdicts_round_trip_through_their_rows() {
    let held: verdict_file::Held = serde_json::from_value(json!({"services": {
        "kbf": {
            "standing": {"s1": report()},
            "counted": [{"node_id": "s2", "report": report()}, {"node_id": "s1", "report": report()}]
        },
        "only-standing": {"standing": {"s1": report()}},
        "only-counted": {"counted": [{"node_id": "s3", "report": report()}]}
    }}))
    .expect("verdicts");
    let order = rich_verdicts();
    assert_eq!(
        verdicts_of(&verdicts_rows(&order).expect("rows")).expect("back"),
        order
    );
    let tables = verdicts_rows(&held).expect("the rows");
    assert_eq!(tables["verdict_standing"].len(), 2);
    assert_eq!(tables["verdict_counted"].len(), 3);
    assert_eq!(verdicts_of(&tables).expect("the verdicts back"), held);
    let none = verdicts_rows(&verdict_file::Held::default()).expect("no rows");
    assert!(none.values().all(Rows::is_empty));
    assert_eq!(
        verdicts_of(&none).expect("empty"),
        verdict_file::Held::default()
    );
}

/// Proves: verdict rows the reader refuses are refused at load with the file reader's text and
/// the table's place, and rows with a key that is not text are passed over.
///
/// Catches: a load of verdicts less strict than a start on the file.
#[test]
fn verdict_rows_the_reader_refuses_are_refused_at_load() {
    let tables = Tables::from([(
        "verdict_standing",
        Rows::from([(key(&["kbf", "s1"]), vec![Val::Json(json!({"x": 1}))])]),
    )]);
    let err = verdicts_of(&tables).expect_err("an unreadable report");
    assert!(err.to_string().starts_with("database:verdicts: "), "{err}");
    let counted = Tables::from([(
        "verdict_counted",
        Rows::from([(
            vec![KeyVal::Text("kbf".to_owned()), KeyVal::Int(0)],
            vec![Val::Null, Val::Null],
        )]),
    )]);
    assert!(verdicts_of(&counted).is_err());
    // A key that is not text names no host the reader takes.
    let not_text = Tables::from([(
        "verdict_standing",
        Rows::from([(vec![KeyVal::Int(1), KeyVal::Int(2)], vec![Val::Null])]),
    )]);
    assert!(verdicts_of(&not_text).is_err());
}

/// Proves: the place an error names is the table as `database:<table>`, and a hold's scope and a
/// halt's origin are spelt as the tables' checks allow.
///
/// Catches: a spelling a table's CHECK refuses.
#[test]
fn the_names_the_tables_check_are_the_ones_written() {
    assert_eq!(place("holds").display().to_string(), "database:holds");
    assert_eq!(hold_scope(&HoldScope::Host("h".to_owned())), ("host", "h"));
    assert_eq!(hold_scope(&HoldScope::Role("r".to_owned())), ("role", "r"));
    assert_eq!(hold_scope(&HoldScope::Fleet), ("fleet", ""));
    assert_eq!(entry_name(Entry::Include), "include");
    assert_eq!(entry_name(Entry::Exclude), "exclude");
    assert_eq!(origin_name(HaltOrigin::Hook), "hook");
    assert_eq!(origin_name(HaltOrigin::Updater), "updater");
    assert_eq!(origin_name(HaltOrigin::Operator), "operator");
}
