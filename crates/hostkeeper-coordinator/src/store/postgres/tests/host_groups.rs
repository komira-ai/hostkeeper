//! The host groups in the database (migration `0008`): the release before is refused on a database
//! that holds it, naming it; this release reads the rows the release before wrote as naming no
//! group; the tables' checks refuse what the reader refuses; and a group's rows follow its commits.

use std::collections::BTreeMap;

use super::with_session;
use super::{SPENT_NOW, TestDatabase};
use crate::desired;
use crate::host_groups::tests::web;
use crate::host_groups::{HostGroup, HostGroups};
use crate::store::postgres::{migrate, parts, records};
use crate::store::{Backend, Commit};

/// Proves: the release before this one (migrations `0001` to `0007`) is refused on a database
/// this release migrated, its runner answering `the database's schema is newer than this
/// coordinator (migration 8, host_groups)` and running nothing, because `0008` says older
/// releases must not run on it; the database keeps every migration it had.
///
/// Catches: an older coordinator that starts on a database whose placement is its groups and
/// places services by roles, or writes host rows that name fewer columns than the table has.
#[test]
fn the_release_before_is_refused_on_a_database_with_0008() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    store.stop();
    let error = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..7]).expect("the release before's set");
        migrate::apply(session, &before).expect_err("the release before is refused")
    });
    assert!(matches!(
        error,
        migrate::ApplyError::Newer { version: 8, .. }
    ));
    assert_eq!(
        error.to_string(),
        "the database's schema is newer than this coordinator (migration 8, host_groups)"
    );
    assert_eq!(
        db.rows("SELECT version FROM schema_version ORDER BY version"),
        (1..=25)
            .map(|n| vec![Some(n.to_string())])
            .collect::<Vec<_>>()
    );
}

/// Proves: this release starts on a database the release before migrated (`0001` to `0007`) and
/// wrote a host, an inventory record and an audit line into, as that release writes them (naming
/// the columns it knows); it runs `0008` and the migrations after it, and reads the host with no group, no move and no
/// facts' generation, the record with no group, and the line concerning no group.
///
/// Catches: a row written before the groups read as naming a group (or refused), or a migration
/// this release does not run on the database of the release before.
#[test]
fn this_release_reads_the_rows_of_the_release_before_as_naming_no_group() {
    let db = TestDatabase::new();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..7]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, (1..=7).collect::<Vec<i32>>());
    db.run(
        "INSERT INTO enrollment_hosts (host_id, key_hash, machine_id, role, services, duplicate, \
         first_heartbeat_unix, first_serving_unix) \
         VALUES ('web-1', '\\x07', 'm-1', 'web', '{}', false, NULL, NULL); \
         INSERT INTO inventory (host_id, role, data_macs, bmc_mac, serial, notes, by_name, \
         at_unix, position) VALUES ('web-2', 'web', '{}', '', 'SN2', '', 'op', 9, 0); \
         INSERT INTO audit (seq, at_unix, by_name, action, service, node_id, reason) \
         VALUES (1, 1800000000, 'op', 'exclude', 'web', 'web-1', 'disk')",
    )
    .expect("the release before's rows");
    let (store, loaded) = db.open("this release").expect("this release starts");
    assert_eq!(loaded.ops.inventory.len(), 1);
    assert_eq!(loaded.ops.inventory[0].host_group, "");
    assert_eq!(loaded.ops.audit.len(), 1);
    assert_eq!(loaded.ops.audit[0].host_group, None);
    assert_eq!(loaded.desired.host_groups, HostGroups::default());
    store.stop();
    let read = with_session(&db, |session| {
        records::read_all(session)
            .expect("the records read")
            .expect("they decode")
    });
    let host = read.hosts.first().expect("the host");
    assert_eq!(
        (
            host.host_group.as_str(),
            host.moving_from.as_deref(),
            host.facts_generation
        ),
        ("", None, None)
    );
    assert_eq!(
        db.rows("SELECT version FROM schema_version ORDER BY version")
            .len(),
        25
    );
}

/// The name of the check of the database's error `error`, if it is a check's.
fn check_of(error: &postgres::Error) -> Option<String> {
    error
        .as_db_error()
        .and_then(|e| e.constraint())
        .map(str::to_owned)
}

/// Proves: the tables refuse what the state file's reader refuses, by their checks: a group name
/// that is not a role name (a capital, a digit first, `--`, a trailing `-`, 33 bytes), a group's
/// generation 0, a desired version's generation 0 or a write id with a space, and a facts'
/// generation below 0; a name the reader takes is taken.
///
/// Catches: a row written by hand (or by a bug) that a start would then refuse, found only at the
/// next restart rather than at the write.
#[test]
fn the_tables_refuse_what_the_reader_refuses() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    store.stop();
    let put = |name: &str, generation: i64| {
        db.run(&format!(
            "INSERT INTO host_groups (name, generation, policy, by_name, at_unix) \
             VALUES ('{name}', {generation}, '{{}}', 'op', 0)"
        ))
    };
    let longest = format!("a{}", "b".repeat(31));
    for good in ["web", "a", "web-canary-2", longest.as_str()] {
        put(good, 1).unwrap_or_else(|e| panic!("{good}: {e}"));
    }
    let too_long = format!("a{}", "b".repeat(32));
    for bad in ["Web", "1web", "we--b", "web-", "-web", too_long.as_str()] {
        let error = put(bad, 1).expect_err(bad);
        assert_eq!(
            check_of(&error).as_deref(),
            Some("host_groups_name"),
            "{bad}"
        );
    }
    let error = put("db", 0).expect_err("generation 0");
    assert_eq!(check_of(&error).as_deref(), Some("host_groups_generation"));
    let desired = |generation: i64, write_id: &str| {
        db.run(&format!(
            "INSERT INTO group_desired (host_group, service, version, generation, write_id) \
             VALUES ('web', 'tierstore', '1.0', {generation}, '{write_id}')"
        ))
    };
    let error = desired(0, "w-1").expect_err("generation 0");
    assert_eq!(
        check_of(&error).as_deref(),
        Some("group_desired_generation")
    );
    let error = desired(1, "has space").expect_err("a space");
    assert_eq!(check_of(&error).as_deref(), Some("group_desired_write_id"));
    desired(1, "w-1").expect("taken");
    let error = db
        .run(
            "INSERT INTO enrollment_hosts (host_id, key_hash, machine_id, role, services, \
             duplicate, facts_generation) VALUES ('h', '\\x07', 'm', 'web', '{}', false, -1)",
        )
        .expect_err("a negative generation");
    assert_eq!(
        check_of(&error).as_deref(),
        Some("enrollment_hosts_facts_generation")
    );
}

fn count(db: &TestDatabase, table: &str) -> usize {
    db.rows(&format!("SELECT 1 FROM {table}")).len()
}

/// Proves: a commit that puts a group writes its row, one row per package with its position and
/// one per desired version; a commit that reorders its packages and drops its desired version
/// rewrites the positions and deletes the version's row, which a restart reads back as committed;
/// a commit that removes the group leaves none of its rows.
///
/// Catches: a removed group whose packages or desired versions stay behind (a later group of the
/// same name would inherit them), or a reorder that a restart reads in the old order.
#[test]
fn a_groups_rows_follow_its_commits() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let first = desired::Held {
        host_groups: HostGroups(BTreeMap::from([("web".parse().expect("a name"), web())])),
        ..loaded.desired.clone()
    };
    let commit = |before: &desired::Held, after: &desired::Held| {
        store
            .commit(&Commit {
                desired: Some((before, after)),
                ..Commit::default()
            })
            .expect("committed");
    };
    commit(&loaded.desired, &first);
    assert_eq!(
        (
            count(&db, "host_groups"),
            count(&db, "host_group_packages"),
            count(&db, "group_desired")
        ),
        (1, 2, 1)
    );
    let mut second = first.clone();
    if let Some(group) = second.host_groups.0.values_mut().next() {
        *group = HostGroup {
            packages: vec!["hostkeeper".to_owned(), "tierstore".to_owned()],
            generation: 4,
            desired: BTreeMap::new(),
            ..group.clone()
        };
    }
    commit(&first, &second);
    assert_eq!(
        db.rows("SELECT service, position FROM host_group_packages ORDER BY position"),
        vec![
            vec![Some("hostkeeper".to_owned()), Some("0".to_owned())],
            vec![Some("tierstore".to_owned()), Some("1".to_owned())],
        ]
    );
    assert_eq!(count(&db, "group_desired"), 0);
    store.stop();
    let (store, again) = db.open("this release").expect("it starts again");
    assert_eq!(again.desired.host_groups, second.host_groups);
    let none = desired::Held {
        host_groups: HostGroups::default(),
        ..second.clone()
    };
    store
        .commit(&Commit {
            desired: Some((&second, &none)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(
        (
            count(&db, "host_groups"),
            count(&db, "host_group_packages"),
            count(&db, "group_desired")
        ),
        (0, 0, 0)
    );
    store.stop();
    let loaded = with_session(&db, |session| {
        parts::load(session, SPENT_NOW).expect("loads").0
    });
    assert_eq!(loaded.desired.host_groups, HostGroups::default());
}

/// Proves: a commit of a (group, package)'s every mark writes its rows of `group_min_versions`,
/// `group_services`, `group_rollouts` and `group_rollout_done`, which a restart reads back; a
/// commit that puts the plan back to its default but for its pause keeps the plan's row and
/// deletes its done hosts; a commit that puts every mark back to its default deletes every row,
/// and a restart reads none.
///
/// Catches: a mark cleared in memory (a halt lifted, a pause resumed, a minimum version taken
/// away) that comes back after a restart because its row was left behind.
#[test]
fn a_group_packages_mark_rows_follow_its_commits() {
    use crate::host_groups::tests::every_mark;
    use crate::host_groups::{GroupMarks, GroupsMarks};
    use crate::ops::OpsState;
    use crate::rollouts::Rollout;
    let db = TestDatabase::new();
    let (store, loaded) = db.open("this release").expect("the store opens");
    let with = |marks: GroupMarks| {
        let mut ops = loaded.ops.clone();
        ops.host_groups = GroupsMarks(BTreeMap::from([(
            "web".parse().expect("a name"),
            BTreeMap::from([("tierstore".to_owned(), marks)]),
        )]));
        ops
    };
    let commit = |before: &OpsState, after: &OpsState| {
        store
            .commit(&Commit {
                ops: Some((before, after)),
                ..Commit::default()
            })
            .expect("committed");
    };
    let tables = [
        "group_min_versions",
        "group_services",
        "group_rollouts",
        "group_rollout_done",
    ];
    let counts = |db: &TestDatabase| tables.map(|t| count(db, t));
    let marked = with(every_mark());
    commit(&loaded.ops, &marked);
    assert_eq!(counts(&db), [1, 1, 1, 1]);
    let read = with_session(&db, |session| {
        parts::load(session, SPENT_NOW).expect("loads").0
    });
    assert_eq!(read.ops.host_groups, marked.host_groups);
    let web: crate::host_groups::GroupName = "web".parse().expect("a name");
    assert_eq!(
        read.ops.host_groups.0[&web]["tierstore"].force,
        every_mark().force,
        "the standing force, whole"
    );
    let paused = with(GroupMarks {
        rollout: Rollout {
            paused: every_mark().rollout.paused,
            ..Rollout::default()
        },
        ..GroupMarks::default()
    });
    commit(&marked, &paused);
    assert_eq!(counts(&db), [0, 0, 1, 0]);
    store.stop();
    let (store, again) = db.open("this release").expect("it starts again");
    assert_eq!(again.ops.host_groups, paused.host_groups);
    let cleared = with(GroupMarks::default());
    store
        .commit(&Commit {
            ops: Some((&paused, &cleared)),
            ..Commit::default()
        })
        .expect("committed");
    assert_eq!(counts(&db), [0, 0, 0, 0]);
    store.stop();
    let (store, last) = db.open("this release").expect("it starts again");
    assert_eq!(last.ops.host_groups, GroupsMarks::default());
    store.stop();
}

/// Proves: migration `0018` on a database `0017` left (the built-in group `all` with its packages,
/// desired version, minimum version, marks, rollout position and a host recorded done, a group
/// that lists no `hostkeeper`, one that lists it with its own desired version, one that lists
/// nothing, and hosts recorded in `all`, in a group and in none): the group `all` and every row
/// keyed by it are gone; the hosts recorded in `all` are in none, the others keep their group;
/// each group that lacked `hostkeeper` lists it first, the rest of its list after it, at the
/// version `all` had for it unless the group has its own; and this release then loads the groups
/// that are left.
///
/// Catches: rows of the removed group left to be read as an ordinary group named `all`, a host
/// left recorded in it, a group left without the package every group must list (a start refused
/// by the strict reader), and a group's own desired version of `hostkeeper` overwritten.
#[test]
fn migration_0018_removes_the_group_all_and_makes_every_group_list_hostkeeper() {
    let db = TestDatabase::new();
    with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..17]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    db.run(
        "INSERT INTO host_groups (name, generation, policy, by_name, at_unix) VALUES \
           ('web', 1, '{}', 'op', 1), ('canary', 1, '{}', 'op', 1), ('empty', 1, '{}', 'op', 1); \
         UPDATE host_groups SET generation = 4, by_name = 'op' WHERE name = 'all'; \
         INSERT INTO host_group_packages (host_group, service, position) VALUES \
           ('all', 'tierstore', 1), ('web', 'tierstore', 0), ('web', 'base', 1), \
           ('canary', 'hostkeeper', 0), ('canary', 'tierstore', 1); \
         INSERT INTO group_desired (host_group, service, version, generation) VALUES \
           ('all', 'hostkeeper', '1.5', 3), ('all', 'tierstore', '1.0', 1), \
           ('canary', 'hostkeeper', '2.0', 2); \
         INSERT INTO group_min_versions (host_group, service, version) VALUES \
           ('all', 'hostkeeper', '1.0'), ('web', 'tierstore', '0.9'); \
         INSERT INTO group_services (host_group, service, last_good) VALUES \
           ('all', 'hostkeeper', '1.4'), ('web', 'tierstore', '0.9'); \
         INSERT INTO group_rollouts (host_group, service, version) VALUES \
           ('all', 'hostkeeper', '1.5'), ('web', 'tierstore', '1.0'); \
         INSERT INTO group_rollout_done (host_group, service, node_id) VALUES \
           ('all', 'hostkeeper', 'h1'), ('web', 'tierstore', 'h2'); \
         INSERT INTO enrollment_hosts (host_id, key_hash, machine_id, role, services, duplicate, \
           first_heartbeat_unix, first_serving_unix, host_group) VALUES \
           ('h1', '\\x01', 'm-1', 'ungrouped', '{}', false, NULL, NULL, 'all'), \
           ('h2', '\\x02', 'm-2', 'web', '{}', false, NULL, NULL, 'web'), \
           ('h3', '\\x03', 'm-3', 'ungrouped', '{}', false, NULL, NULL, NULL); \
         UPDATE enrollment_hosts SET moving_from = 'all' WHERE host_id = 'h2'; \
         INSERT INTO inventory (host_id, role, data_macs, bmc_mac, serial, notes, by_name, \
           at_unix, position, host_group) VALUES \
           ('i1', 'web', '{}', '', 'SN1', '', 'op', 9, 0, 'all'), \
           ('i2', 'web', '{}', '', 'SN2', '', 'op', 9, 1, 'web')",
    )
    .expect("the rows of 0017");
    with_session(&db, |session| {
        let all = migrate::checked(&migrate::SHIPPED).expect("this release's set");
        assert_eq!(
            migrate::apply(session, &all).expect("0018 to 0025 run"),
            vec![18, 19, 20, 21, 22, 23, 24, 25]
        );
    });

    for table in [
        "group_desired",
        "group_min_versions",
        "group_services",
        "group_rollouts",
        "group_rollout_done",
        "host_group_packages",
    ] {
        assert_eq!(
            db.rows(&format!("SELECT 1 FROM {table} WHERE host_group = 'all'")),
            Vec::<Vec<Option<String>>>::new(),
            "{table}"
        );
    }
    let text = |sql: &str| -> Vec<String> {
        db.rows(sql)
            .into_iter()
            .map(|row| row.into_iter().flatten().collect::<Vec<_>>().join(" "))
            .collect()
    };
    assert_eq!(
        text("SELECT name FROM host_groups ORDER BY name"),
        ["canary", "empty", "web"]
    );
    assert_eq!(
        text("SELECT host_group, service, position FROM host_group_packages ORDER BY 1, 3"),
        [
            "canary hostkeeper 0",
            "canary tierstore 1",
            "empty hostkeeper 0",
            "web hostkeeper 0",
            "web tierstore 1",
            "web base 2",
        ]
    );
    assert_eq!(
        text("SELECT host_group, service, version, generation FROM group_desired ORDER BY 1, 2"),
        [
            "canary hostkeeper 2.0 2",
            "empty hostkeeper 1.5 3",
            "web hostkeeper 1.5 3",
        ]
    );
    assert_eq!(
        text("SELECT host_group, service FROM group_min_versions"),
        ["web tierstore"]
    );
    assert_eq!(
        text("SELECT host_id, coalesce(host_group, '-') FROM enrollment_hosts ORDER BY 1"),
        ["h1 -", "h2 web", "h3 -"]
    );
    assert_eq!(
        text("SELECT host_id, coalesce(moving_from, '-') FROM enrollment_hosts ORDER BY 1"),
        ["h1 -", "h2 -", "h3 -"]
    );
    assert_eq!(
        text("SELECT host_id, coalesce(host_group, '-') FROM inventory ORDER BY 1"),
        ["i1 -", "i2 web"]
    );
    assert_eq!(
        text("SELECT host_group, service, node_id FROM group_rollout_done"),
        ["web tierstore h2"]
    );

    let (store, loaded) = db.open("this release").expect("this release starts");
    store.stop();
    assert_eq!(
        loaded
            .desired
            .host_groups
            .0
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["canary", "empty", "web"]
    );
}
