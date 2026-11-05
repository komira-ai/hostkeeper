//! The mirror of kbf's readers (`support::readers`) held to what those readers refuse and what
//! they take: each kind of value in the ops file, the halts file and the desired-state file, the
//! keys the lenient readers skip, the scalars, and the table (`strict-readers.json`) that drives
//! the mirror. `state_files.rs` holds the files this coordinator writes to the mirror; these
//! tests hold the mirror to kbf.

mod support;

use serde_json::{Value, json};
use support::readers::{Readers, is_role_name, is_token, is_version};
use support::recorded;

/// A copy of `base` with the value at the JSON pointer `path` set to `new`, or taken out when
/// `new` is none.
fn edited(base: &Value, path: &str, new: Option<Value>) -> Value {
    let mut value = base.clone();
    let (parent, key) = path.rsplit_once('/').expect("a pointer with a parent");
    match (value.pointer_mut(parent).expect("a place to edit"), new) {
        (Value::Object(map), Some(new)) => {
            map.insert(key.to_owned(), new);
        }
        (Value::Object(map), None) => {
            map.remove(key);
        }
        (Value::Array(items), Some(new)) => {
            items[key.parse::<usize>().expect("an index")] = new;
        }
        (other, _) => panic!("{path} is in {other}, which cannot be edited that way"),
    }
    value
}

/// An ops file with a record of every kind kbf's reader takes in it: a standing halt and a halt
/// acknowledged, each with its report (the reader takes one in the ops file too), a report
/// seen without one, the bad versions and a failure ack with its report.
const FULL_OPS: &str = r#"{
  "schema": 1,
  "hosts": {"s1": {"labels": {"rack": "r1"}}},
  "excluded": [{"service": "kbf", "node_id": "s2", "reason": "bad disk"}],
  "services": {"kbf": {
    "halt": {
      "node_id": "s1",
      "check": "disk_tainted",
      "target": "2.0",
      "report": {"target": "2.0", "outcome": "failed", "phase": "halt", "reason": "disk_tainted"}
    },
    "halt_acked": [{
      "node_id": "s3",
      "target": "1.5",
      "report": {"target": "1.5", "outcome": "failed", "phase": "after_commit", "class": "release"}
    }],
    "halt_seen": [{"node_id": "s4", "target": "2.0"}],
    "halt_generation": 2,
    "last_good": "1.0",
    "bad": ["1.5", "2.0"],
    "failure_acked": [{
      "node_id": "d1",
      "report": {"target": "2.0", "outcome": "failed", "phase": "gate_release", "reason": "smoke_test"}
    }]
  }},
  "audit": [{
    "seq": 1,
    "by": "ci",
    "action": "unmark_bad",
    "service": "kbf",
    "node_id": "d1",
    "reason": "2.0 was set by CI"
  }]
}"#;

/// A halts file with a record of every kind kbf writes in it: the standing halt, a report
/// acknowledged and one seen while the halt stood, the generation and the digest of the ops
/// file.
const FULL_HALTS: &str = r#"{
  "services": {"kbf": {
    "halt": {
      "node_id": "s1",
      "target": "2.0",
      "report": {"target": "2.0", "outcome": "failed", "phase": "halt", "reason": "disk_tainted"}
    },
    "halt_acked": [{
      "node_id": "s3",
      "target": "1.5",
      "report": {"target": "1.5", "outcome": "failed", "phase": "after_commit", "class": "release"}
    }],
    "halt_seen": [{
      "node_id": "s4",
      "target": "2.0",
      "report": {"target": "2.0", "outcome": "failed", "phase": "halt", "class": "environment", "reason": "state_unreadable"}
    }],
    "halt_generation": 3
  }},
  "ops": "b53b723f57e20124d54787869e86432b43edc7b6ab4115911493ec51e1d31f7e"
}"#;

fn full_ops() -> Value {
    serde_json::from_str(FULL_OPS).expect("a JSON ops file")
}

fn full_halts() -> Value {
    serde_json::from_str(FULL_HALTS).expect("a JSON halts file")
}

/// Asserts that the reader `(name, at)` refuses `base` once the value at each JSON pointer of
/// `cases` is set to the new one (taken out when none), with a reason that holds the text.
fn assert_refused(
    readers: &Readers,
    (name, at): (&str, &str),
    base: &Value,
    cases: &[(&str, Option<Value>, &str)],
) {
    for (path, new, want) in cases {
        let value = edited(base, path, new.clone());
        let why = readers.check(name, &value, at).expect_err(path);
        assert!(why.contains(want), "{path}: {why} lacks {want}");
    }
}

/// Catches: a mirror that checks the keys of a record and not its values. In the ops file, the
/// halts file and the desired-state file: a scalar of the wrong type (a `seq` or a generation as
/// text, a negative or a fractional one), an enum spelling kbf does not know in a report (an
/// `outcome`, a `phase`, a `class`) wherever the report is (a failure ack of the ops file, the
/// halt, the acknowledged and the seen records of the halts file), a version or a token that is
/// none, a map of the wrong value type (a label that is a number), limits `Desired::checked`
/// refuses (a key that is no role name, a count over 10000, a `min_up` without `frontend`), a
/// record that lacks a key kbf requires or holds it null, and a null in a map, a list or a count
/// (which serde refuses for a field that has a default and is no `Option`) are each refused,
/// saying where. The same files unedited are taken.
#[test]
fn the_mirror_of_kbfs_readers_refuses_the_values_they_refuse() {
    let readers = Readers::recorded();
    let ops = full_ops();
    let halts = full_halts();
    let map: Value = serde_json::from_str(&recorded("desired-map.json")).expect("kbf's map");
    readers
        .check("OpsState", &ops, "ops")
        .expect("a full ops file");
    readers
        .check("ReadHalts", &halts, "halts")
        .expect("a full halts file");
    readers
        .check("MapFile", &map, "desired")
        .expect("kbf's map");

    assert_refused(
        &readers,
        ("OpsState", "ops"),
        &ops,
        &[
            ("/audit/0/seq", Some(json!("1")), "ops.audit[0].seq"),
            (
                "/services/kbf/halt_generation",
                Some(json!("2")),
                "ops.services.kbf.halt_generation",
            ),
            (
                "/services/kbf/halt_generation",
                Some(json!(-1)),
                "ops.services.kbf.halt_generation",
            ),
            (
                "/services/kbf/halt_generation",
                Some(json!(2.5)),
                "ops.services.kbf.halt_generation",
            ),
            (
                "/services/kbf/failure_acked/0/report/outcome",
                Some(json!("exploded")),
                "failure_acked[0].report.outcome",
            ),
            (
                "/services/kbf/failure_acked/0/report/phase",
                Some(json!("mid_flight")),
                "failure_acked[0].report.phase",
            ),
            (
                "/services/kbf/failure_acked/0/report/class",
                Some(json!("Release")),
                "failure_acked[0].report.class",
            ),
            (
                "/services/kbf/failure_acked/0/report/reason",
                Some(json!("Smoke Test")),
                "failure_acked[0].report.reason",
            ),
            (
                "/services/kbf/failure_acked/0/report/target",
                Some(json!("-1.0")),
                "failure_acked[0].report.target",
            ),
            (
                "/services/kbf/last_good",
                Some(json!("1.0 beta")),
                "ops.services.kbf.last_good",
            ),
            (
                "/services/kbf/bad/1",
                Some(json!(2)),
                "ops.services.kbf.bad[1]",
            ),
            (
                "/hosts/s1/labels/rack",
                Some(json!(5)),
                "ops.hosts.s1.labels.rack",
            ),
            (
                "/services/kbf/halt/node_id",
                Some(json!(7)),
                "ops.services.kbf.halt.node_id",
            ),
            (
                "/audit/0/by",
                Some(Value::Null),
                "AuditEntry requires by, which is null",
            ),
            ("/audit/0/by", None, "AuditEntry requires by"),
            (
                "/services/kbf/failure_acked/0/report",
                None,
                "FailureAck requires report",
            ),
            (
                "/services/kbf/failure_acked/0/report/outcome",
                None,
                "UpdateResult requires outcome",
            ),
            (
                "/services/kbf/halt/report",
                Some(json!({"outcome": "failed"})),
                "UpdateResult requires target",
            ),
            (
                "/hosts",
                Some(Value::Null),
                "OpsState takes no null for hosts",
            ),
            (
                "/hosts/s1/labels",
                Some(Value::Null),
                "HostOps takes no null for labels",
            ),
            (
                "/services/kbf/bad",
                Some(Value::Null),
                "ServiceOps takes no null for bad",
            ),
            (
                "/services/kbf/halt_generation",
                Some(Value::Null),
                "ServiceOps takes no null for halt_generation",
            ),
        ],
    );

    assert_refused(
        &readers,
        ("ReadHalts", "halts"),
        &halts,
        &[
            (
                "/services/kbf/halt/report/outcome",
                Some(json!("exploded")),
                "halts.services.kbf.halt.report.outcome",
            ),
            (
                "/services/kbf/halt_acked/0/report/phase",
                Some(json!("mid_flight")),
                "halt_acked[0].report.phase",
            ),
            (
                "/services/kbf/halt_seen/0/report/class",
                Some(json!("other")),
                "halt_seen[0].report.class",
            ),
            (
                "/services/kbf/halt_seen/0/report/reason",
                Some(json!("Disk Tainted")),
                "halt_seen[0].report.reason",
            ),
            (
                "/services/kbf/halt_acked/0/target",
                Some(json!("not a version")),
                "halt_acked[0].target",
            ),
            (
                "/services/kbf/halt_generation",
                Some(json!("3")),
                "halts.services.kbf.halt_generation",
            ),
            ("/ops", Some(json!(7)), "halts.ops"),
            (
                "/services/kbf/halt/node_id",
                None,
                "Standing requires node_id",
            ),
            (
                "/services/kbf/halt_seen/0/report",
                None,
                "Kept requires report",
            ),
            (
                "/services/kbf/halt_acked/0",
                Some(json!(5)),
                "Kept is not an object",
            ),
            (
                "/services/kbf",
                Some(json!([])),
                "ReadService is not an object",
            ),
            ("/services", Some(json!([])), "not an object"),
            (
                "/services",
                Some(Value::Null),
                "ReadHalts takes no null for services",
            ),
            (
                "/services/kbf/halt_acked",
                Some(Value::Null),
                "ReadService takes no null for halt_acked",
            ),
            (
                "/services/kbf/halt/report",
                Some(Value::Null),
                "Standing takes no null for report",
            ),
        ],
    );

    assert_refused(
        &readers,
        ("MapFile", "desired"),
        &map,
        &[
            (
                "/services/kbf/version",
                Some(json!("v2")),
                "desired.services.kbf.version",
            ),
            (
                "/services/kbf/max_updating/daemon",
                Some(json!("2")),
                "desired.services.kbf.max_updating.daemon",
            ),
            (
                "/services/kbf/max_updating/daemon",
                Some(json!(10_001)),
                "desired.services.kbf.max_updating.daemon",
            ),
            (
                "/services/kbf/max_updating/Cache",
                Some(json!(1)),
                "desired.services.kbf.max_updating.Cache",
            ),
            (
                "/services/kbf/min_up/frontend",
                None,
                "desired.services.kbf.min_up",
            ),
            (
                "/services/tierstore/min_up/storage",
                Some(json!(-3)),
                "desired.services.tierstore.min_up.storage",
            ),
            (
                "/services/kbf/min_up",
                Some(json!(3)),
                "desired.services.kbf.min_up",
            ),
            (
                "/services/kbf/max_updating",
                Some(Value::Null),
                "Desired takes no null for max_updating",
            ),
        ],
    );
}

/// Catches: a mirror that refuses a null where kbf's readers read none. A field they read as an
/// `Option` (a halt, the last good version, the report of a halt or of its acknowledgement, the
/// service, node or reason of an audit entry, the phase, class or reason of a report, the digest
/// of the halts file) may be null in the ops file and in the halts file; a null in a map, a list
/// or a count is refused in the test above.
#[test]
fn the_mirror_of_kbfs_readers_takes_a_null_where_they_read_none() {
    let readers = Readers::recorded();
    let mut ops = full_ops();
    for path in [
        "/services/kbf/halt/report",
        "/services/kbf/halt_acked/0/report",
        "/services/kbf/halt_seen/0/report",
        "/services/kbf/last_good",
        "/audit/0/service",
        "/audit/0/node_id",
        "/audit/0/reason",
        "/services/kbf/failure_acked/0/report/phase",
        "/services/kbf/failure_acked/0/report/class",
        "/services/kbf/failure_acked/0/report/reason",
        "/services/kbf/halt",
    ] {
        ops = edited(&ops, path, Some(Value::Null));
    }
    readers
        .check("OpsState", &ops, "ops")
        .expect("kbf reads a null Option in the ops file as none");

    let mut halts = full_halts();
    for path in [
        "/services/kbf/halt_acked/0/report/phase",
        "/services/kbf/halt_seen/0/report/class",
        "/services/kbf/halt_seen/0/report/reason",
        "/services/kbf/halt",
        "/ops",
    ] {
        halts = edited(&halts, path, Some(Value::Null));
    }
    readers
        .check("ReadHalts", &halts, "halts")
        .expect("kbf reads a null Option in the halts file as none");
}

/// Catches: a mirror that is stricter than kbf's lenient readers, or as lenient as its strict
/// ones. The halts file, the records in it and the report skip a key they do not know (so a
/// field a later build adds is no refusal), and the structs of the ops file refuse it.
#[test]
fn the_mirror_of_kbfs_readers_skips_the_keys_their_lenient_readers_skip() {
    let readers = Readers::recorded();
    let mut halts = full_halts();
    for path in [
        "/future",
        "/services/kbf/future",
        "/services/kbf/halt/future",
        "/services/kbf/halt_acked/0/future",
        "/services/kbf/halt_seen/0/report/future",
    ] {
        halts = edited(&halts, path, Some(json!(true)));
    }
    readers
        .check("ReadHalts", &halts, "halts")
        .expect("kbf's reader of the halts file skips a key it does not know");
    let report = "/services/kbf/failure_acked/0/report/future";
    let ops = edited(&full_ops(), report, Some(json!({"any": [1, null]})));
    readers
        .check("OpsState", &ops, "ops")
        .expect("kbf's reader of a report skips a key it does not know");
    for path in [
        "/future",
        "/services/kbf/future",
        "/services/kbf/halt/future",
        "/services/kbf/halt_acked/0/future",
        "/services/kbf/failure_acked/0/future",
    ] {
        let value = edited(&full_ops(), path, Some(json!(true)));
        let why = readers.check("OpsState", &value, "ops").expect_err(path);
        assert!(why.contains("future"), "{path}: {why}");
    }
}

/// Catches: an `assignments` key that kbf's reader of the ops file, the release before on this
/// file, would pass over: it refuses the key, so a coordinator rolled back below this one stops
/// rather than order hosts an operator took a service off, while the file without the key (as
/// this release writes it when no assignment stands) still reads.
#[test]
fn the_ops_files_reader_of_the_release_before_refuses_the_assignments_key() {
    let readers = Readers::recorded();
    readers
        .check("OpsState", &full_ops(), "ops")
        .expect("without the key");
    let assigned = json!({"kbf": {"hosts": {"s1": "include"}, "uninstall": {"s2": 1}}});
    let with = edited(&full_ops(), "/assignments", Some(assigned));
    let why = readers
        .check("OpsState", &with, "ops")
        .expect_err("kbf's reader of the ops file is strict");
    assert!(why.contains("assignments"), "{why}");
}

/// What kbf-updater's `Version::parse` takes, and what it refuses, at the tag (its `ACCEPTED`
/// and `REJECTED`).
const VERSIONS_TAKEN: &[&str] = &[
    "1",
    "0.1.0",
    "1.2.3-1",
    "1.2.3~rc1-1",
    "20261021.1+git9ba38c2-2",
    "7.0.0-1~deb12u1",
];

const VERSIONS_REFUSED: &[&str] = &[
    "",
    "-1.0",
    "--version",
    "-o Dpkg::Options::=--force-all",
    "1.0\n",
    "\n1.0",
    "1.0\nfoo",
    "1.0\r",
    "1.0\0",
    "1.0\0foo",
    "1:2.0",
    "0:1.0",
    " 1.0",
    "1.0 ",
    "1.0 2.0",
    "1.0\t",
    "a1.0",
    ".1",
    "1.0;id",
    "1.0&&id",
    "1.0|id",
    "$(id)",
    "1.0`id`",
    "1.0'",
    "1.0\"",
    "1.0\\",
    "1.0/2",
    "1.0_2",
    "1.0=2",
    "\u{ff11}.0",
    "1.0\u{e9}",
    "1.0\u{2028}",
];

/// Catches: a mirror whose idea of a version, a token or a role name is not kbf's, which would
/// take a value kbf refuses or refuse one it takes: every version kbf's tests take is taken and
/// every one they refuse is refused, and a version, a token and a role name are as long as
/// kbf's grammar lets them be and no longer.
#[test]
fn the_mirrors_scalars_are_the_ones_kbf_parses() {
    for text in VERSIONS_TAKEN {
        assert!(is_version(text), "{text:?} is a version");
    }
    for text in VERSIONS_REFUSED {
        assert!(!is_version(text), "{text:?} is no version");
    }
    let longest = format!("1{}", "0".repeat(127));
    assert!(is_version(&longest), "128 bytes are a version");
    assert!(!is_version(&format!("{longest}0")), "129 bytes are none");

    let (token, too_long) = ("a".repeat(64), "a".repeat(65));
    for text in ["disk_tainted", "a", "0_9", token.as_str()] {
        assert!(is_token(text), "{text:?} is a token");
    }
    for text in [
        "",
        "Disk",
        "disk-tainted",
        "disk tainted",
        too_long.as_str(),
    ] {
        assert!(!is_token(text), "{text:?} is no token");
    }

    let (role, too_long) = ("a".repeat(32), "a".repeat(33));
    for text in ["gpu", "frontend", "mac-daemon", "a", "a1", role.as_str()] {
        assert!(is_role_name(text), "{text:?} is a role name");
    }
    for text in [
        "",
        "Cache",
        "a b",
        "-x",
        "x-",
        "a--b",
        "1a",
        "a_b",
        too_long.as_str(),
    ] {
        assert!(!is_role_name(text), "{text:?} is no role name");
    }
}

/// Catches: a table that quietly takes anything. A key that is neither typed nor nested (so its
/// value is never looked at), one that is both, a key listed that is not allowed, a key that is
/// both required and nullable, a type the mirror has no name for, a struct nested that the table
/// lacks and a file that names no struct are each reported; the recorded table has none of them.
#[test]
fn the_table_of_kbfs_readers_types_every_key_it_allows() {
    let readers = Readers::recorded();
    assert_eq!(readers.problems(), Vec::<String>::new());
    let broken: Readers = serde_json::from_value(json!({
        "from": "a table with holes",
        "files": {"ops": "Nothing"},
        "structs": {"Hole": {
            "source": "nowhere:1",
            "allowed": ["typed", "bare", "both"],
            "required": ["absent", "typed"],
            "nullable": ["typed", "stray"],
            "nested": {"both": "Hole", "gone": "[Missing]"},
            "types": {"typed": "integer", "both": "string"}
        }}
    }))
    .expect("a table");
    let problems = broken.problems();
    for want in [
        "file ops: no reader named Nothing",
        "Hole: bare has no type and is not nested",
        "Hole: both is typed and nested",
        "Hole: absent is not an allowed key",
        "Hole: stray is not an allowed key",
        "Hole: gone is not an allowed key",
        "Hole: typed is required and nullable",
        "Hole.typed: no type named integer",
        "Hole.gone: no reader named Missing",
    ] {
        assert!(
            problems.iter().any(|p| p == want),
            "{want} is not in {problems:?}"
        );
    }
}
