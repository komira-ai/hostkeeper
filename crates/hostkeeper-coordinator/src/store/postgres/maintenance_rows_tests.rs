use super::*;
use crate::maintenance_runs::tests::sample;

fn key(parts: &[&str]) -> Vec<KeyVal> {
    parts.iter().map(|p| text(p)).collect()
}

fn s(text: &str) -> Val {
    Val::Text(text.to_owned())
}

fn names_of(names: &[&str]) -> Val {
    Val::Texts(names.iter().map(|n| (*n).to_owned()).collect())
}

fn rows_of(runs: &[Run]) -> Tables {
    let mut tables = Tables::new();
    insert_runs(runs, &mut tables).expect("the rows");
    tables
}

/// A run with every optional value set, and two hosts.
fn full(id: &str) -> Run {
    Run {
        id: id.to_owned(),
        kinds: vec![Kind::Macos, Kind::Xcode],
        host_ids: vec!["h1".to_owned(), "h2".to_owned()],
        host_group: "web".to_owned(),
        window_start_unix: 1_900_000_005,
        window_end_unix: 1_900_000_900,
        batch: Some(3),
        drain_deadline_secs: Some(600),
        take_last_host: true,
        reason: "patch run".to_owned(),
        by: "alice".to_owned(),
        scheduled_unix: 1_900_000_000,
        cancelled_unix: Some(1_900_000_100),
        hosts: vec![
            RunHost {
                host_id: "h2".to_owned(),
                host_group: "web".to_owned(),
                step: Step::Done,
                since_unix: 1_900_000_050,
                kinds: vec![Kind::Macos],
                applied: vec![Kind::Macos],
                skipped_because: None,
                reason: String::new(),
                offered_again: 0,
                baked_beats: 3,
                queued: None,
                order_try: 0,
            },
            RunHost {
                host_id: "h1".to_owned(),
                host_group: String::new(),
                step: Step::Skipped,
                since_unix: 0,
                kinds: vec![Kind::Xcode, Kind::Macos],
                applied: Vec::new(),
                skipped_because: Some(Skip::Excluded),
                reason: "why".to_owned(),
                offered_again: 4,
                baked_beats: 0,
                queued: Some(Act::PutBack),
                order_try: 3,
            },
        ],
    }
}

/// A run with nothing optional set and one bare host.
fn bare(id: &str) -> Run {
    Run {
        id: id.to_owned(),
        kinds: Vec::new(),
        host_ids: Vec::new(),
        host_group: String::new(),
        window_start_unix: 0,
        window_end_unix: 0,
        batch: None,
        drain_deadline_secs: None,
        take_last_host: false,
        reason: "r".to_owned(),
        by: "bob".to_owned(),
        scheduled_unix: 1_900_000_001,
        cancelled_unix: None,
        hosts: vec![RunHost {
            host_id: "h3".to_owned(),
            host_group: String::new(),
            step: Step::Waiting,
            since_unix: 0,
            kinds: Vec::new(),
            applied: Vec::new(),
            skipped_because: None,
            reason: String::new(),
            offered_again: 0,
            baked_beats: 0,
            queued: None,
            order_try: 0,
        }],
    }
}

/// Proves: the runs the core's sample holds are written as rows and read back equal, and a state
/// with no run has two empty tables and reads back as no run, as does a database without them.
///
/// Catches: a field of a run or of a host's turn that a row drops or garbles.
#[test]
fn the_sample_runs_round_trip_through_their_rows() {
    let runs = sample();
    assert_eq!(runs_of(&rows_of(&runs)).expect("the runs back"), runs);
    let none = rows_of(&[]);
    assert!(none["maintenance_runs"].is_empty());
    assert!(none["maintenance_hosts"].is_empty());
    assert_eq!(runs_of(&none).expect("none"), Vec::<Run>::new());
    assert_eq!(
        runs_of(&Tables::new()).expect("no table"),
        Vec::<Run>::new()
    );
}

/// Proves: a run with every value set is one row of `maintenance_runs` (kinds and hosts by name,
/// the window, batch, drain deadline, cancel and position as numbers) and each of its hosts one
/// row of `maintenance_hosts` (step and skip by name, an empty group `NULL`, its offers after a
/// deferral and its bake as numbers, a queued act by its name and `NULL` for none, and the last
/// order try as a number), and a run with
/// nothing set has `NULL` for its group, batch, drain deadline and cancel, and an empty list for
/// its kinds and hosts.
///
/// Catches: a value stored as a zero or an empty text where the run left it out, and a name
/// written other than the one the file writes.
#[test]
fn the_rows_of_a_run_and_of_a_host_hold_the_values_by_name_and_null_for_none() {
    let tables = rows_of(&[full("run-a"), bare("run-b")]);
    assert_eq!(
        tables["maintenance_runs"][&key(&["run-a"])],
        vec![
            names_of(&["macos", "xcode"]),
            names_of(&["h1", "h2"]),
            s("web"),
            Val::Int(1_900_000_005),
            Val::Int(1_900_000_900),
            Val::Int(3),
            Val::Int(600),
            Val::Bool(true),
            s("patch run"),
            s("alice"),
            Val::Int(1_900_000_000),
            Val::Int(1_900_000_100),
            Val::Int(0),
        ]
    );
    assert_eq!(
        tables["maintenance_runs"][&key(&["run-b"])],
        vec![
            names_of(&[]),
            names_of(&[]),
            Val::Null,
            Val::Int(0),
            Val::Int(0),
            Val::Null,
            Val::Null,
            Val::Bool(false),
            s("r"),
            s("bob"),
            Val::Int(1_900_000_001),
            Val::Null,
            Val::Int(1),
        ]
    );
    assert_eq!(
        tables["maintenance_hosts"][&key(&["run-a", "h2"])],
        vec![
            s("web"),
            s("done"),
            Val::Int(1_900_000_050),
            names_of(&["macos"]),
            names_of(&["macos"]),
            Val::Null,
            s(""),
            Val::Int(0),
            Val::Int(0),
            Val::Int(3),
            Val::Null,
            Val::Int(0),
        ]
    );
    assert_eq!(
        tables["maintenance_hosts"][&key(&["run-a", "h1"])],
        vec![
            Val::Null,
            s("skipped"),
            Val::Int(0),
            names_of(&["xcode", "macos"]),
            names_of(&[]),
            s("excluded"),
            s("why"),
            Val::Int(1),
            Val::Int(4),
            Val::Int(0),
            s("put_back"),
            Val::Int(3),
        ]
    );
    assert_eq!(
        tables["maintenance_hosts"][&key(&["run-b", "h3"])][5],
        Val::Null
    );
    assert_eq!(
        tables["maintenance_hosts"][&key(&["run-b", "h3"])][10..],
        [Val::Null, Val::Int(0)]
    );
    assert_eq!(tables["maintenance_hosts"].len(), 3);
}

/// Proves: the runs read back in the order they are held and each run's hosts in theirs, not in
/// the order of the keys the rows sort by.
///
/// Catches: runs or hosts read back in id order, which would list a farm's runs and order its
/// hosts differently after a restart.
#[test]
fn runs_and_hosts_read_back_in_their_positions_not_in_key_order() {
    let runs = vec![full("run-b"), bare("run-a")];
    let tables = rows_of(&runs);
    let keys: Vec<_> = tables["maintenance_runs"].keys().cloned().collect();
    assert_eq!(keys, vec![key(&["run-a"]), key(&["run-b"])]);
    let back = runs_of(&tables).expect("the runs back");
    assert_eq!(back, runs);
    assert_eq!(
        back[0]
            .hosts
            .iter()
            .map(|h| h.host_id.as_str())
            .collect::<Vec<_>>(),
        vec!["h2", "h1"]
    );
}

/// Proves: a time above the largest `bigint` is refused when the rows are made, naming the
/// counter.
///
/// Catches: a time wrapped to a negative number in the database.
#[test]
fn a_time_above_the_largest_the_database_keeps_is_refused_when_written() {
    let mut run = bare("run-a");
    run.scheduled_unix = u64::MAX;
    assert!(insert_runs(&[run], &mut Tables::new()).is_err());
    let mut host_run = bare("run-a");
    host_run.hosts[0].since_unix = u64::MAX;
    assert!(insert_runs(&[host_run], &mut Tables::new()).is_err());
    let mut tried = bare("run-a");
    tried.hosts[0].order_try = u64::MAX;
    assert!(insert_runs(&[tried], &mut Tables::new()).is_err());
}

fn refused(tables: &Tables) -> String {
    runs_of(tables).expect_err("refused").to_string()
}

/// The rows of `full("run-a")` and `bare("run-b")`, edited by `change`, and what reading them
/// says.
fn edited(change: impl FnOnce(&mut Tables)) -> String {
    let mut tables = rows_of(&[full("run-a"), bare("run-b")]);
    change(&mut tables);
    refused(&tables)
}

fn set_run(tables: &mut Tables, column: usize, value: Val) {
    if let Some(row) = tables
        .get_mut("maintenance_runs")
        .and_then(|rows| rows.get_mut(&key(&["run-a"])))
    {
        row[column] = value;
    }
}

fn set_host(tables: &mut Tables, column: usize, value: Val) {
    if let Some(row) = tables
        .get_mut("maintenance_hosts")
        .and_then(|rows| rows.get_mut(&key(&["run-a", "h2"])))
    {
        row[column] = value;
    }
}

/// Proves: a run row this build cannot read is refused at load naming `database:maintenance_runs`,
/// the run and the reason: a kind it does not know, kinds that are not a list, a hosts column that
/// is not a list, a group that is not text, a negative counter, a counter that is not a number, a
/// batch above `u32::MAX`, a `take_last_host` that is not a boolean, a reason or author that is
/// missing, and a position that is not a number.
///
/// Catches: a load less strict than a start on the files, which would run a schedule this
/// build cannot read.
#[test]
fn a_run_row_this_build_cannot_read_is_refused_naming_the_run_and_the_reason() {
    let at = |why: &str| format!("database:maintenance_runs: the row run-a: {why}");
    assert_eq!(
        edited(|t| set_run(t, 0, names_of(&["macos", "weekly"]))),
        at("\"weekly\" is not a maintenance kind")
    );
    assert_eq!(
        edited(|t| set_run(t, 0, s("macos"))),
        at("a column is not a list of texts")
    );
    assert_eq!(
        edited(|t| set_run(t, 1, Val::Null)),
        at("a column is not a list of texts")
    );
    assert_eq!(
        edited(|t| set_run(t, 2, Val::Int(1))),
        at("a column is not text")
    );
    assert_eq!(
        edited(|t| set_run(t, 3, Val::Int(-1))),
        at("a counter is negative")
    );
    assert_eq!(
        edited(|t| set_run(t, 4, s("later"))),
        at("a counter is not a number")
    );
    assert_eq!(
        edited(|t| set_run(t, 5, Val::Int(i64::from(u32::MAX) + 1))),
        at("the batch is too large")
    );
    assert_eq!(
        edited(|t| set_run(t, 6, Val::Int(-5))),
        at("a counter is negative")
    );
    assert_eq!(
        edited(|t| set_run(t, 7, s("yes"))),
        at("a column is not a boolean")
    );
    assert_eq!(
        edited(|t| set_run(t, 8, Val::Null)),
        at("a column that is required is empty")
    );
    assert_eq!(
        edited(|t| set_run(t, 9, Val::Null)),
        at("a column that is required is empty")
    );
    assert_eq!(
        edited(|t| set_run(t, 12, s("first"))),
        at("a position is not a number")
    );
    assert_eq!(
        edited(|t| set_host(t, 10, s("nap"))),
        at("\"nap\" is not an act queued for a host")
    );
    assert_eq!(
        edited(|t| set_host(t, 10, Val::Int(1))),
        at("a column is not text")
    );
    assert_eq!(
        edited(|t| set_host(t, 11, Val::Int(-1))),
        at("a counter is negative")
    );
    assert_eq!(
        edited(|t| set_host(t, 11, Val::Null)),
        at("a counter is not a number")
    );
    let mut ok = rows_of(&[full("run-a")]);
    set_host(&mut ok, 10, s("retry"));
    set_host(&mut ok, 11, Val::Int(i64::MAX));
    let queued = &runs_of(&ok).expect("a queued act and the largest try")[0].hosts[0];
    assert_eq!(
        (queued.queued, queued.order_try),
        (Some(Act::Retry), i64::MAX.unsigned_abs())
    );
    set_run(&mut ok, 5, Val::Int(i64::from(u32::MAX)));
    assert_eq!(
        runs_of(&ok).expect("the largest batch")[0].batch,
        Some(u32::MAX)
    );
}

/// Proves: a host row this build cannot read is refused at load naming
/// `database:maintenance_hosts`, the run and the host and the reason: a step it does not know, a
/// missing step, a reason for leaving it out it does not know, kinds or applied kinds it does not
/// know, a counter that is negative, a position that is not a number, an offer count or a bake
/// above `u32::MAX` or below 0 (`u32::MAX` itself is read), and a host of a run no row holds.
///
/// Catches: a host's turn read as another step than the one stored, and a host row kept for a
/// run that is gone.
#[test]
fn a_host_row_this_build_cannot_read_is_refused_naming_the_run_the_host_and_the_reason() {
    let at = |why: &str| format!("database:maintenance_hosts: the row run-a/h2: {why}");
    assert_eq!(
        edited(|t| set_host(t, 1, s("sleeping"))),
        at("\"sleeping\" is not a step of a host's turn")
    );
    assert_eq!(
        edited(|t| set_host(t, 1, Val::Null)),
        at("a column that is required is empty")
    );
    assert_eq!(
        edited(|t| set_host(t, 5, s("lazy"))),
        at("\"lazy\" is not why a host is left out")
    );
    assert_eq!(
        edited(|t| set_host(t, 3, names_of(&["tea"]))),
        at("\"tea\" is not a maintenance kind")
    );
    assert_eq!(
        edited(|t| set_host(t, 4, names_of(&["macos", "tea"]))),
        at("\"tea\" is not a maintenance kind")
    );
    assert_eq!(
        edited(|t| set_host(t, 2, Val::Int(-1))),
        at("a counter is negative")
    );
    assert_eq!(
        edited(|t| set_host(t, 7, Val::Null)),
        at("a position is not a number")
    );
    for column in [8, 9] {
        assert_eq!(
            edited(|t| set_host(t, column, Val::Int(i64::from(u32::MAX) + 1))),
            at("a count is above what a host's turn keeps"),
            "column {column}"
        );
        assert_eq!(
            edited(|t| set_host(t, column, Val::Int(-1))),
            at("a counter is negative"),
            "column {column}"
        );
    }
    let mut ok = rows_of(&[full("run-a")]);
    set_host(&mut ok, 8, Val::Int(i64::from(u32::MAX)));
    set_host(&mut ok, 9, Val::Int(i64::from(u32::MAX)));
    let largest = &runs_of(&ok).expect("the largest counts")[0].hosts[0];
    assert_eq!(
        (largest.offered_again, largest.baked_beats),
        (u32::MAX, u32::MAX)
    );
    assert_eq!(
        edited(|t| {
            if let Some(rows) = t.get_mut("maintenance_runs") {
                rows.remove(&key(&["run-a"]));
            }
        }),
        "database:maintenance_hosts: the row run-a/h1: no run holds the host's turn"
    );
}
