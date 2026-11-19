//! The number a desired version is set under (`queue_seq`, migration `0025`): the queue of the
//! services that wait for their turn follows it on every coordinator of one database, a commit
//! takes one only for a version it sets or changes, and a start gives a number to the rows a
//! release before wrote or changed, in the order of its queue.
//!
//! Two coordinators are two stores opened on one database. Nothing sleeps and no time is measured:
//! the first-seen stamps of the in-memory queue are given by the test, against the numbers.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;

use super::edits::{Instance, instance, version};
use super::{TestDatabase, with_session};
use crate::host_groups::{GroupDesired, GroupName, HostGroup, HostGroups, Policy};
use crate::queue::{Cluster, Entry};
use crate::services::ServiceDef;
use crate::store::Part;
use crate::store::postgres::migrate;
use crate::store::postgres::writer::try_lock;
use crate::testutil::{desired, version as parsed};

/// The number and the version it was taken for of each row: `service` for an ungrouped one,
/// `group/package` for a group's.
type Numbers = BTreeMap<String, (Option<i64>, Option<String>)>;

fn numbers(db: &TestDatabase) -> Numbers {
    db.rows(
        "SELECT service, set_seq, set_for FROM desired \
         UNION ALL \
         SELECT host_group || '/' || service, set_seq, set_for FROM group_desired",
    )
    .into_iter()
    .map(|row| {
        let key = row[0].clone().expect("a key");
        let seq = row[1].as_deref().map(|n| n.parse().expect("a number"));
        (key, (seq, row[2].clone()))
    })
    .collect()
}

/// The number of the row `key`.
fn seq(db: &TestDatabase, key: &str) -> i64 {
    numbers(db)[key].0.expect("a numbered row")
}

fn name(text: &str) -> GroupName {
    text.parse().expect("a group name")
}

/// A group listing `packages`, last changed at `at_unix`, wanting each of `wanted`.
fn group(packages: &[&str], at_unix: u64, wanted: &[(&str, &str)]) -> HostGroup {
    HostGroup {
        packages: packages.iter().map(|p| (*p).to_owned()).collect(),
        policy: Policy::default(),
        generation: 1,
        by: "alice".to_owned(),
        at_unix,
        desired: wanted
            .iter()
            .map(|(package, at)| {
                (
                    (*package).to_owned(),
                    GroupDesired {
                        version: parsed(at),
                        generation: 1,
                        write_id: None,
                        replaced_version: None,
                    },
                )
            })
            .collect(),
    }
}

/// Puts `groups` over the groups `on` holds, in one commit.
fn put(on: &Instance, groups: Vec<(&str, HostGroup)>) {
    let (staged, ()) = on
        .desired
        .stage_host_groups(|_| {
            let all = groups.into_iter().map(|(n, g)| (name(n), g)).collect();
            Ok::<_, Infallible>((HostGroups(all), ()))
        })
        .expect("staged");
    on.desired.commit_staged(staged).expect("committed");
}

/// The entries of the queue `on` holds: every package of every group that wants a version, with
/// the number its coordinator's memory holds.
fn entries(on: &Instance) -> Vec<Entry> {
    let mut out = Vec::new();
    for (group, listed) in &on.desired.host_groups().0 {
        let (wants, seqs) = (
            on.desired.group_desireds(group),
            on.desired.group_set_seqs(group),
        );
        for package in &listed.packages {
            let Some(want) = wants.get(package) else {
                continue;
            };
            let mut def = ServiceDef::new(package, &[]);
            def.scope = Some(crate::group_scope::GroupScope {
                group: group.clone(),
                live_rollouts: None,
                max_updating: 1,
            });
            out.push(Entry {
                def,
                want: Some(want.clone()),
                at_unix: listed.at_unix,
                set_seq: seqs.get(package).copied(),
            });
        }
    }
    out
}

/// The entries as `group/package`, first in line of `cluster` first.
fn line(cluster: &Cluster, entries: &[Entry]) -> Vec<String> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|entry| cluster.order(entry));
    sorted
        .iter()
        .map(|e| format!("{}/{}", e.def.group().expect("a group"), e.def.name))
        .collect()
}

/// Proves: two coordinators on one database that set desired versions of three groups' packages
/// by turns queue the packages in the order of the sets: after each set, both read the part again
/// and their queues are the order of the numbers the database holds, whatever each one's
/// first-seen stamps say (the stamps are given in the opposite order, so a queue that falls back
/// to them is found), and the one that made the set holds the number it took after it reads again.
///
/// Catches: a queue ordered by what each coordinator saw first, which two coordinators disagree
/// on, and a commit that leaves its coordinator's copy labelled current with numbers it never
/// read.
#[test]
fn queue_follows_set_seq_on_both_tasks() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    put(
        &a,
        vec![
            ("g1", group(&["hostkeeper", "web"], 300, &[("web", "1.0")])),
            ("g2", group(&["hostkeeper", "db"], 100, &[("db", "1.0")])),
            (
                "g3",
                group(&["hostkeeper", "cache"], 200, &[("cache", "1.0")]),
            ),
        ],
    );
    let b = instance(&db, "b");
    let sets = [
        (&a, "g2", "db", "2.0"),
        (&b, "g1", "web", "2.0"),
        (&a, "g3", "cache", "2.0"),
        (&b, "g2", "db", "3.0"),
    ];
    let mut clusters = [Cluster::default(), Cluster::default()];
    let stamp = |step: usize| Duration::from_secs(1000 - 10 * step as u64);
    let mut last = 0;
    for (step, (by, group, package, to)) in sets.into_iter().enumerate() {
        by.desired
            .set_group_version(&name(group), package, &parsed(to))
            .expect("the version is set");
        let taken = seq(&db, &format!("{group}/{package}"));
        assert!(taken > last, "{taken} follows {last}");
        last = taken;
        let by_numbers: Vec<String> = db
            .rows("SELECT host_group || '/' || service FROM group_desired ORDER BY set_seq")
            .into_iter()
            .map(|row| row[0].clone().expect("a key"))
            .collect();
        for (one, cluster) in [&a, &b].into_iter().zip(&mut clusters) {
            one.desired.refresh();
            let queued = entries(one);
            *cluster = cluster.refreshed(queued.clone(), stamp(step));
            assert_eq!(line(cluster, &queued), by_numbers, "step {step}");
        }
        assert_eq!(
            by.desired.group_set_seqs(&name(group))[package],
            taken,
            "the coordinator that set it reads the number it took"
        );
    }
    assert_eq!(
        line(&clusters[0], &entries(&a)),
        ["g1/web", "g3/cache", "g2/db"]
    );
    let queued = entries(&b);
    let (web, db_entry) = (&queued[0], &queued[1]);
    assert!(
        clusters[1].stamp_of(db_entry) < clusters[1].stamp_of(web),
        "the stamps run against the numbers"
    );
}

/// Proves: a commit numbers exactly the desired versions it sets or changes: a change of another
/// column of a row (a group's write id and generation, a service's `min_up`) keeps its number and
/// the version it was taken for; a new version, a rollback to the version before and a new
/// ungrouped version each take a larger one, in the transaction that writes the row, so the
/// part's version moves on once; a commit the rows refuse takes none and changes nothing.
///
/// Catches: a diff that rewrites the number with the row (an edit of a policy sends a package to
/// the back of the queue), a rollback that keeps its old place, and a number taken by a commit
/// that did not land.
#[test]
fn a_commit_numbers_only_the_versions_it_sets() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    put(
        &a,
        vec![("g1", group(&["hostkeeper", "web"], 300, &[("web", "1.0")]))],
    );
    a.desired
        .set("svc", desired("1.0", 1, 1))
        .expect("an ungrouped service is set");
    let first = numbers(&db);
    assert_eq!(first["g1/web"], (Some(1), Some("1.0".to_owned())));
    assert_eq!(first["svc"], (Some(2), Some("1.0".to_owned())));
    let moved = version(&db, Part::Desired);

    // Another column of both rows changes, and the group's own row with it.
    let (staged, ()) = a
        .desired
        .stage_host_groups(|groups| {
            let mut all = groups.clone();
            let one = all.0.get_mut(&name("g1")).expect("the group");
            one.by = "bob".to_owned();
            let wanted = one.desired.get_mut("web").expect("the version");
            wanted.write_id = Some("w-2".to_owned());
            wanted.generation = 2;
            Ok::<_, Infallible>((all, ()))
        })
        .expect("staged");
    a.desired.commit_staged(staged).expect("committed");
    a.desired
        .set("svc", desired("1.0", 2, 2))
        .expect("the limits change");
    assert_eq!(numbers(&db), first, "the numbers stay with their rows");
    assert_eq!(version(&db, Part::Desired), moved + 2);
    assert_eq!(
        db.rows("SELECT write_id, generation FROM group_desired"),
        vec![vec![Some("w-2".to_owned()), Some("2".to_owned())]]
    );

    // A new version takes the next number, in the commit that writes it.
    a.desired
        .set_group_version(&name("g1"), "web", &parsed("2.0"))
        .expect("set");
    assert_eq!(numbers(&db)["g1/web"], (Some(3), Some("2.0".to_owned())));
    assert_eq!(version(&db, Part::Desired), moved + 3);
    // A rollback to the version before is a change of version and takes one too.
    a.desired
        .set_group_version(&name("g1"), "web", &parsed("1.0"))
        .expect("rolled back");
    assert_eq!(numbers(&db)["g1/web"], (Some(4), Some("1.0".to_owned())));
    a.desired
        .set("svc", desired("2.0", 2, 2))
        .expect("a new ungrouped version");
    assert_eq!(numbers(&db)["svc"], (Some(5), Some("2.0".to_owned())));
    assert_eq!(version(&db, Part::Desired), moved + 5);
    a.desired.refresh();
    assert_eq!(a.desired.group_set_seqs(&name("g1"))["web"], 4);

    // A commit the table's check refuses takes no number and writes nothing.
    let before = (numbers(&db), version(&db, Part::Desired));
    let (staged, ()) = a
        .desired
        .stage_host_groups(|groups| {
            let mut all = groups.clone();
            let wanted = all
                .0
                .get_mut(&name("g1"))
                .and_then(|one| one.desired.get_mut("web"))
                .expect("the version");
            wanted.version = parsed("3.0");
            wanted.generation = 0;
            Ok::<_, Infallible>((all, ()))
        })
        .expect("staged");
    a.desired
        .commit_staged(staged)
        .expect_err("a generation of 0 is refused");
    assert_eq!((numbers(&db), version(&db, Part::Desired)), before);
}

/// Proves: a start gives a number to each row a release before wrote or changed, in the order of
/// that release's queue, above every number held (the sequence is raised to the highest first,
/// whatever it stood at): rows with no number and rows whose version is not the one their number
/// was taken for are numbered, the ungrouped ones first by service name, then each group's by its
/// last change, its name, the package's place in its list (not its name) and its name; a row
/// that kept its version keeps its number; the part's version moves on once; and a second start
/// changes nothing.
///
/// Catches: a row a release before changed that keeps a number from before its change, a number
/// below one held (an order that puts the new row ahead of older sets), a queue order that
/// differs from the one the release before had, and a start that moves the part's version when
/// it has nothing to number.
#[test]
fn set_seq_of_a_row_changed_by_the_release_before_is_restamped_in_queue_order() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    put(
        &a,
        vec![
            (
                "g1",
                group(
                    &["hostkeeper", "beta", "web", "api"],
                    300,
                    &[("web", "1.0")],
                ),
            ),
            ("g2", group(&["hostkeeper", "db"], 100, &[("db", "1.0")])),
        ],
    );
    a.desired.set("abc", desired("1.0", 1, 1)).expect("abc");
    a.desired.set("zed", desired("1.0", 1, 1)).expect("zed");
    a.store.stop();
    let numbered = numbers(&db);
    assert_eq!(
        numbered.values().filter_map(|(n, _)| *n).max(),
        Some(4),
        "four sets took four numbers"
    );

    // The release before changes two versions and inserts rows with no number, with the sequence
    // behind the numbers held.
    db.run(
        "UPDATE group_desired SET version = '9.0' WHERE host_group = 'g2' AND service = 'db'; \
         UPDATE desired SET version = '2.0' WHERE service = 'zed'; \
         INSERT INTO group_desired (host_group, service, version, generation) VALUES \
           ('g1', 'api', '1.0', 1), ('g1', 'beta', '1.0', 1); \
         INSERT INTO host_groups (name, generation, policy, by_name, at_unix) \
           VALUES ('g0', 1, '{}', 'op', 50); \
         INSERT INTO host_group_packages (host_group, service, position) \
           VALUES ('g0', 'hostkeeper', 0), ('g0', 'cache', 1); \
         INSERT INTO group_desired (host_group, service, version, generation) \
           VALUES ('g0', 'cache', '1.0', 1); \
         SELECT setval('desired_seq', 1)",
    )
    .expect("the release before's changes");
    let moved = version(&db, Part::Desired);

    let (store, loaded) = db.open("restarted").expect("the store opens");
    let after = numbers(&db);
    let one = |n: i64, v: &str| (Some(n), Some(v.to_owned()));
    assert_eq!(after["g1/web"], numbered["g1/web"], "an untouched row");
    assert_eq!(after["abc"], numbered["abc"], "an untouched row");
    assert_eq!(after["zed"], one(5, "2.0"), "an ungrouped row comes first");
    assert_eq!(after["g0/cache"], one(6, "1.0"), "the group changed first");
    assert_eq!(after["g2/db"], one(7, "9.0"), "a changed version");
    assert_eq!(after["g1/beta"], one(8, "1.0"), "listed before api");
    assert_eq!(after["g1/api"], one(9, "1.0"));
    assert_eq!(version(&db, Part::Desired), moved + 1);
    assert_eq!(
        loaded.desired.seqs[&(Some("g1".to_owned()), "api".to_owned())],
        9
    );
    assert_eq!(loaded.desired.seqs[&(None, "zed".to_owned())], 5);
    store.stop();

    let (store, loaded) = db.open("restarted again").expect("the store opens");
    assert_eq!(numbers(&db), after, "a second start numbers nothing");
    assert_eq!(version(&db, Part::Desired), moved + 1);
    assert_eq!(loaded.desired.seqs.len(), after.len());
    store.stop();
}

/// Proves (B8, both ways, for `0025`): the release before's runner (migrations `0001` to `0024`)
/// accepts a database this release migrated and numbered rows in, runs nothing, and takes the
/// coordinator's lock once this release has stopped; this release, started on a database the
/// release before migrated and wrote groups and services into, runs `0025` and numbers those rows
/// at start, the ungrouped service first, each with the version it was taken for.
///
/// Catches: a `0025` marked `refuse`, which would turn a rollback into a restore, and a start on
/// the release before's rows that leaves them with no number.
#[test]
fn the_release_before_runs_on_a_database_with_0025_and_this_one_on_its() {
    let db = TestDatabase::new();
    let a = instance(&db, "this release");
    put(
        &a,
        vec![("g1", group(&["hostkeeper", "web"], 300, &[("web", "1.0")]))],
    );
    a.store.stop();
    assert_eq!(seq(&db, "g1/web"), 1);
    let (ran, locked) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..24]).expect("the release before's set");
        let ran = migrate::apply(session, &before).expect("the release before is accepted");
        (ran, try_lock(session).expect("a try"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert!(
        locked,
        "the release before has the lock once this release stopped"
    );

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..24]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    old.run(
        "INSERT INTO host_groups (name, generation, policy, by_name, at_unix) \
           VALUES ('g1', 1, '{}', 'op', 300); \
         INSERT INTO host_group_packages (host_group, service, position) \
           VALUES ('g1', 'hostkeeper', 0), ('g1', 'web', 1); \
         INSERT INTO group_desired (host_group, service, version, generation) \
           VALUES ('g1', 'web', '1.0', 1); \
         INSERT INTO desired (service, version, max_updating, min_up) VALUES \
           ('abc', '1.0', '{\"frontend\":1,\"storage\":1,\"daemon\":2,\"mac-daemon\":2}', \
            '{\"storage\":0,\"frontend\":0}')",
    )
    .expect("the release before's rows");
    let (store, loaded) = old.open("this release").expect("this release migrates");
    let one = |n: i64| (Some(n), Some("1.0".to_owned()));
    assert_eq!(
        numbers(&old),
        BTreeMap::from([("abc".to_owned(), one(1)), ("g1/web".to_owned(), one(2))])
    );
    assert_eq!(loaded.desired.seqs.len(), 2);
    store.stop();
}

/// Proves: the rows that are not for the release before to read stay out of its way: a desired
/// state read from the rows holds the numbers in memory only, so the file of the desired state a
/// coordinator over files writes (`Held` encoded) holds none and an older build reads it.
///
/// Catches: a number written into the state file, which its strict reader refuses.
#[test]
fn the_numbers_are_not_written_to_the_state_file() {
    let db = TestDatabase::new();
    let a = instance(&db, "a");
    put(
        &a,
        vec![("g1", group(&["hostkeeper", "web"], 300, &[("web", "1.0")]))],
    );
    a.desired.refresh();
    let (_store, loaded) = db.open("b").expect("opens");
    assert_eq!(loaded.desired.seqs.len(), 1);
    let text = crate::desired::encode(&loaded.desired, None).expect("encoded");
    let text = String::from_utf8(text).expect("text");
    assert!(
        !text.contains("set_seq") && !text.contains("seqs"),
        "{text}"
    );
    crate::desired::parse_file(text.as_bytes(), None).expect("the file reads back");
}
