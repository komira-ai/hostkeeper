use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::host_groups::tests::{every_mark, web};
use crate::host_groups::{GroupDesired, HostGroup};
use crate::testutil::version;

fn key(parts: &[&str]) -> Vec<KeyVal> {
    parts.iter().map(|p| text(p)).collect()
}

fn name(text: &str) -> GroupName {
    text.parse().expect("a group name")
}

/// Two groups: `web`, two packages with a desired version, and `canary`, `hostkeeper` alone with
/// a policy.
fn groups() -> HostGroups {
    let mut canary = HostGroup {
        packages: vec!["hostkeeper".to_owned()],
        generation: 1,
        desired: BTreeMap::from([(
            "hostkeeper".to_owned(),
            GroupDesired {
                version: version("1.0"),
                generation: 1,
                write_id: None,
                replaced_version: None,
            },
        )]),
        ..web()
    };
    canary.policy.bake_beats = 3;
    HostGroups(BTreeMap::from([
        (name("web"), web()),
        (name("canary"), canary),
    ]))
}

fn rows_of(groups: &HostGroups) -> Tables {
    let mut tables = Tables::new();
    insert_groups(groups, &mut tables).expect("the rows");
    tables
}

/// Proves: the groups are written as one row of `host_groups` each (generation, policy as the
/// file's JSON, author, time), one row of `host_group_packages` per package with its position in
/// the list, and one row of `group_desired` per desired version (an absent write id and replaced
/// version `NULL`), and read back equal, the list in its order; no group, no row.
///
/// Catches: a package list read back in name order instead of the group's order (which is the
/// order of installs), and a field a row drops.
#[test]
fn the_groups_round_trip_through_their_rows_in_list_order() {
    let tables = rows_of(&groups());
    assert_eq!(
        tables["host_groups"][&key(&["canary"])],
        vec![
            Val::Int(1),
            Val::Json(json!({"bake_beats": 3})),
            Val::Text("alice".to_owned()),
            Val::Int(1_800_000_000),
        ]
    );
    assert_eq!(
        tables["host_group_packages"]
            .iter()
            .map(|(k, v)| (key_text(k, 0), key_text(k, 1), v[0].clone()))
            .collect::<Vec<_>>(),
        vec![
            ("canary".to_owned(), "hostkeeper".to_owned(), Val::Int(0)),
            ("web".to_owned(), "hostkeeper".to_owned(), Val::Int(1)),
            ("web".to_owned(), "tierstore".to_owned(), Val::Int(0)),
        ]
    );
    assert_eq!(
        tables["group_desired"][&key(&["canary", "hostkeeper"])],
        vec![
            Val::Text("1.0".to_owned()),
            Val::Int(1),
            Val::Null,
            Val::Null
        ]
    );
    assert_eq!(
        tables["group_desired"][&key(&["web", "tierstore"])][2..],
        [
            Val::Text("ci-run-7".to_owned()),
            Val::Text("2.3.0".to_owned())
        ]
    );
    assert_eq!(groups_of(&tables).expect("the groups back"), groups());
    let none = rows_of(&HostGroups::default());
    assert!(none.values().all(Rows::is_empty));
    assert_eq!(groups_of(&none).expect("none"), HostGroups::default());
    assert_eq!(
        groups_of(&Tables::new()).expect("no table"),
        HostGroups::default()
    );
}

fn refused(tables: &Tables) -> String {
    groups_of(tables).expect_err("refused").to_string()
}

/// Proves: rows the state file's reader would refuse are refused at load naming
/// `database:host_groups` and the group: a group with no package row, packages or a desired
/// version of a group with no row, a policy key out of range, a name that is no group name, and a
/// generation of 0.
///
/// Catches: a load less strict than a start on the files, which would place hosts by a group a
/// start on the files refuses.
#[test]
fn group_rows_the_reader_refuses_are_refused_at_load() {
    let good = rows_of(&groups());
    let edit = |f: &dyn Fn(&mut Tables)| {
        let mut tables = good.clone();
        f(&mut tables);
        refused(&tables)
    };
    let at = |why: &str| format!("database:host_groups: the desired state is malformed: {why}");
    let cases = [
        (
            edit(&|t| {
                t.insert("host_group_packages", Rows::new());
            }),
            at("host group canary: lists 0 packages, not 1 to 64"),
        ),
        (
            edit(&|t| {
                t.get_mut("host_groups")
                    .map(|rows| rows.remove(&key(&["web"])));
            }),
            at("the packages of web, a group not held"),
        ),
        (
            edit(&|t| {
                t.get_mut("host_groups")
                    .map(|rows| rows.remove(&key(&["canary"])));
                if let Some(rows) = t.get_mut("host_group_packages") {
                    rows.retain(|k, _| key_text(k, 0) != "canary");
                }
            }),
            at("a desired version of canary, a group not held"),
        ),
        (
            edit(&|t| {
                if let Some(row) = t
                    .get_mut("host_groups")
                    .and_then(|rows| rows.get_mut(&key(&["canary"])))
                {
                    row[1] = Val::Json(json!({"bake_beats": 101}));
                }
            }),
            at("host group canary: policy bake_beats: is over 100"),
        ),
        (
            edit(&|t| {
                if let Some(row) = t
                    .get_mut("host_groups")
                    .and_then(|rows| rows.get_mut(&key(&["canary"])))
                {
                    row[0] = Val::Int(0);
                }
            }),
            at("host group canary: the group has generation 0"),
        ),
    ];
    for (got, want) in cases {
        assert_eq!(got, want);
    }
    let mut misnamed = rows_of(&HostGroups(BTreeMap::from([(name("web"), web())])));
    for table in ["host_groups", "host_group_packages", "group_desired"] {
        let rows = misnamed.remove(table).unwrap_or_default();
        let renamed = rows
            .into_iter()
            .map(|(mut k, v)| {
                k[0] = text("Web");
                (k, v)
            })
            .collect();
        misnamed.insert(table, renamed);
    }
    assert!(
        refused(&misnamed).starts_with(
            "database:host_groups: the desired state is malformed: host_groups: not a host group name"
        ),
        "{}",
        refused(&misnamed)
    );
}

fn marks() -> GroupsMarks {
    let mut bare = GroupMarks {
        last_good: Some(version("1.0")),
        ..GroupMarks::default()
    };
    bare.rollout.done.insert("c-1".to_owned());
    GroupsMarks(BTreeMap::from([
        (
            name("web"),
            BTreeMap::from([
                ("tierstore".to_owned(), every_mark()),
                ("hostkeeper".to_owned(), GroupMarks::default()),
            ]),
        ),
        (
            name("canary"),
            BTreeMap::from([("hostkeeper".to_owned(), bare)]),
        ),
    ]))
}

fn mark_rows(marks: &GroupsMarks) -> Tables {
    let mut tables = Tables::new();
    insert_marks(marks, &mut tables).expect("the rows");
    tables
}

/// Proves: the marks of each (group, package) are written as rows of `group_min_versions`,
/// `group_services` (the halt, its reports and the force as their JSON, a mark at its default
/// `NULL` or 0), `group_rollouts` and `group_rollout_done`, a pair whose marks are all at their
/// default having no row, and read back equal but for that pair; a plan recorded with done hosts
/// alone has its row.
///
/// Catches: a group's halt or plan position lost at a restart on the database, or a row stored
/// for a pair that holds nothing.
#[test]
fn the_marks_round_trip_through_their_rows() {
    let tables = mark_rows(&marks());
    assert_eq!(
        tables["group_services"].keys().cloned().collect::<Vec<_>>(),
        vec![key(&["canary", "hostkeeper"]), key(&["web", "tierstore"])],
        "the pair at its default has no row"
    );
    assert_eq!(
        tables["group_services"][&key(&["canary", "hostkeeper"])],
        vec![
            Val::Text("1.0".to_owned()),
            Val::Int(0),
            Val::Int(0),
            Val::Null,
            Val::Null,
            Val::Null,
            Val::Null
        ]
    );
    assert_eq!(
        tables["group_services"][&key(&["web", "tierstore"])][5],
        Val::Json(json!([{"node_id": "web-02", "target": "2.4.0"}]))
    );
    assert_eq!(
        tables["group_min_versions"],
        Rows::from([(
            key(&["web", "tierstore"]),
            vec![Val::Text("2.0.0".to_owned())]
        )])
    );
    assert_eq!(
        tables["group_rollouts"][&key(&["web", "tierstore"])],
        vec![
            Val::Text("2.4.0".to_owned()),
            Val::Int(2),
            Val::Text("alice".to_owned()),
            Val::Text("looking".to_owned())
        ]
    );
    assert_eq!(
        tables["group_rollouts"][&key(&["canary", "hostkeeper"])],
        vec![Val::Null, Val::Int(0), Val::Null, Val::Null],
        "done hosts alone keep the row they hang from"
    );
    assert_eq!(
        tables["group_rollout_done"]
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            key(&["canary", "hostkeeper", "c-1"]),
            key(&["web", "tierstore", "web-01"])
        ]
    );
    assert_eq!(
        marks_of(&tables).expect("the marks back"),
        marks().without_defaults()
    );
    let none = mark_rows(&GroupsMarks::default());
    assert!(none.values().all(Rows::is_empty));
    assert_eq!(
        marks_of(&Tables::new()).expect("no table"),
        GroupsMarks::default()
    );
}

fn mark_refusal(table: &'static str, row: &[&str], values: Vec<Val>) -> String {
    let tables = Tables::from([(table, Rows::from([(key(row), values)]))]);
    marks_of(&tables).expect_err("refused").to_string()
}

/// Proves: a row of the marks' tables this build cannot read is refused naming its table and key
/// and what is wrong: a group key that is no group name, a minimum version that is empty or does
/// not parse, a counter that is negative, a halt that does not parse, stages entered above `u32`,
/// and a package key that is no package name; a pause half there reads as no pause.
///
/// Catches: a load that guesses at a row it cannot read, or names no table and key.
#[test]
fn a_marks_row_this_build_cannot_read_is_refused_naming_its_table_and_key() {
    let why = |table: &str, row: &str, why: &str| format!("database:{table}: the row {row}: {why}");
    let services = |values: Vec<Val>| mark_refusal("group_services", &["web", "kbf"], values);
    let no_marks = || vec![Val::Null, Val::Int(0), Val::Int(0)];
    let bad_version = Version::parse("nope").expect_err("no version").to_string();
    let cases = [
        (
            mark_refusal(
                "group_min_versions",
                &["Web", "kbf"],
                vec![Val::Text("1.0".to_owned())],
            ),
            why(
                "group_min_versions",
                "Web/kbf",
                "not a host group name: 1 to 32 bytes of a-z, 0-9 and -, starting with a letter",
            ),
        ),
        (
            mark_refusal("group_min_versions", &["web", "kbf"], vec![Val::Null]),
            why("group_min_versions", "web/kbf", "the version is empty"),
        ),
        (
            mark_refusal(
                "group_min_versions",
                &["web", "kbf"],
                vec![Val::Text("nope".to_owned())],
            ),
            why("group_min_versions", "web/kbf", &bad_version),
        ),
        (
            services(vec![Val::Null, Val::Int(-1), Val::Int(0)]),
            why("group_services", "web/kbf", "a counter is negative"),
        ),
        (
            services(
                [
                    no_marks(),
                    vec![Val::Null, Val::Json(json!({"node_id": 1}))],
                ]
                .concat(),
            ),
            why(
                "group_services",
                "web/kbf",
                "invalid type: integer `1`, expected a string",
            ),
        ),
        (
            mark_refusal(
                "group_rollouts",
                &["web", "kbf"],
                vec![Val::Null, Val::Int(i64::from(u32::MAX) + 1)],
            ),
            why(
                "group_rollouts",
                "web/kbf",
                "the stages entered are too many",
            ),
        ),
    ];
    for (got, want) in cases {
        assert_eq!(got, want);
    }
    let bad_package = Tables::from([(
        "group_services",
        Rows::from([(
            key(&["web", "a b"]),
            vec![Val::Text("1.0".to_owned()), Val::Int(0), Val::Int(0)],
        )]),
    )]);
    assert_eq!(
        marks_of(&bad_package).expect_err("refused").to_string(),
        "database:group_services: host group web: \"a b\" is not a package name"
    );
    let half = Tables::from([(
        "group_rollouts",
        Rows::from([(
            key(&["web", "kbf"]),
            vec![
                Val::Null,
                Val::Int(1),
                Val::Text("alice".to_owned()),
                Val::Null,
            ],
        )]),
    )]);
    let read = marks_of(&half).expect("read");
    assert_eq!(read.0[&name("web")]["kbf"].rollout.paused, None);
    assert_eq!(read.0[&name("web")]["kbf"].rollout.entered, 1);
}
