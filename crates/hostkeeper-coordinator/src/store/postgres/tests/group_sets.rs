//! Sets of one package in two host groups over the Postgres store, end to end through
//! `Releases.SetDesired` as a release's CI sets it: one set for each group, the second made on the
//! same coordinator right after the first commits, with no request in between that reads the
//! parts' versions again ([`crate::service::Coordinator::refresh_caches`]). That is what a second
//! set that was already waiting for the fleet's lock when the first committed does.
//!
//! A group set's commit takes the number its version is set under in the database
//! ([`crate::store::postgres::queue_seq`]), which memory does not hold, so the coordinator that
//! made it forgets its copy's version. The next set is staged on memory and its edit then finds
//! the desired state changed under it, and the set is decided again on the rows read afresh.
//!
//! A commit of another coordinator between a set's decision and its edit's locks is the same
//! answer from the store. It is made here by a second store on the database, run by the ops store's
//! hook at the start of the set's change ([`crate::ops::OpsStore::interleave`]). The set is decided
//! again on the rows the edit read, up to three times, and then refused `state_changed`.
//!
//! Nothing sleeps and no time is measured: the order of the sets and of the rival's commits is the
//! order of the calls.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use tonic::{Code, Request};

use super::TestDatabase;
use super::coordinator::start_on_postgres;
use super::edits::{Instance, instance};
use super::releases::targets;
use crate::grpc::Admitted;
use crate::grpc::operator::ReleasesService;
use crate::host_groups::{GroupName, HostGroup, HostGroups, Policy};
use crate::ops::AuditEntry;
use crate::service::Coordinator;
use crate::store::Postgres;
use crate::testutil::{Log, Rig, ci_token, version};

/// A coordinator over a database of its own holding the groups `linux` and `mac`, each listing
/// `web`, and its `Releases`.
struct Grouped {
    coordinator: Arc<Coordinator>,
    store: Postgres,
    database: TestDatabase,
    _rig: Rig,
}

fn name(text: &str) -> GroupName {
    text.parse().expect("a group name")
}

/// A group listing `hostkeeper` and `web`, with no desired version yet.
fn listing_web() -> HostGroup {
    HostGroup {
        packages: vec!["hostkeeper".to_owned(), "web".to_owned()],
        policy: Policy::default(),
        generation: 1,
        by: "alice".to_owned(),
        at_unix: 100,
        desired: BTreeMap::new(),
    }
}

impl Grouped {
    fn start(test: &str) -> Grouped {
        let database = TestDatabase::new();
        let (store, loaded) = database.open("coordinator").expect("the store opens");
        let rig = Rig::new(test);
        let marker = rig.dir.path().join("halt.marker");
        let coordinator = Arc::new(start_on_postgres(&rig, &store, loaded, &marker));
        let (staged, ()) = coordinator
            .desired
            .stage_host_groups(|_| {
                let groups = [("linux", listing_web()), ("mac", listing_web())]
                    .into_iter()
                    .map(|(group, held)| (name(group), held))
                    .collect();
                Ok::<_, Infallible>((HostGroups(groups), ()))
            })
            .expect("staged");
        coordinator
            .ops()
            .decide_changes_with(staged, |_| vec![AuditEntry::new("test", "seed")])
            .expect("the groups are held");
        Grouped {
            coordinator,
            store,
            database,
            _rig: rig,
        }
    }

    /// `SetDesired` of `web` at `version` with [`targets`] in the group `group`, as CI's router
    /// admits it for a token nobody used.
    async fn ci_sets(
        &self,
        group: &str,
        version: &str,
    ) -> Result<proto::SetDesiredResponse, tonic::Status> {
        let verified = self
            .coordinator
            .ci(Some(&ci_token()))
            .await
            .expect("CI's token is accepted");
        let set = proto::SetDesiredRequest {
            service: "web".to_owned(),
            desired: Some(proto::DesiredState {
                version: version.to_owned(),
                ..proto::DesiredState::default()
            }),
            targets: targets(),
            host_group: group.to_owned(),
            ..proto::SetDesiredRequest::default()
        };
        let mut request = Request::new(set);
        request.extensions_mut().insert(Admitted::Ci(verified));
        ReleasesService::new(Arc::clone(&self.coordinator))
            .set_desired(request)
            .await
            .map(tonic::Response::into_inner)
    }

    /// The version and generation each group's row of `group_desired` holds for `web`, by group.
    fn rows(&self) -> Vec<Vec<Option<String>>> {
        self.database.rows(
            "SELECT host_group, version, generation::text, set_seq IS NOT NULL::bool::text \
             FROM group_desired WHERE service = 'web' ORDER BY host_group",
        )
    }
}

/// Arms `on` so that each of its next `times` storage-first changes begins only after `rival`, a
/// second coordinator on the database, committed a change of the group `linux` (its generation
/// raised, by `bob`), audited `host_group_put`: a commit between a set's decision and its edit.
fn rival_commits(on: &Arc<Coordinator>, rival: &Arc<Instance>, times: u32) {
    rival_commits_raising(on, rival, times, None);
}

/// [`rival_commits`], each rival change also raising the Mac group's minimum version of `web` to
/// `floor`, when one is given, in the same commit.
fn rival_commits_raising(
    on: &Arc<Coordinator>,
    rival: &Arc<Instance>,
    times: u32,
    floor: Option<&'static str>,
) {
    let (next, rival) = (Arc::clone(on), Arc::clone(rival));
    on.ops().interleave(move |_| {
        let (staged, ()) = rival
            .desired
            .stage_host_groups(|held| {
                let mut groups = held.clone();
                let linux = groups.0.get_mut(&name("linux")).expect("the group");
                linux.generation += 1;
                linux.by = "bob".to_owned();
                Ok::<_, Infallible>((groups, ()))
            })
            .expect("staged");
        rival
            .ops
            .decide_changes_with(staged, |state| {
                if let Some(floor) = floor {
                    let marks = state.host_groups.0.entry(name("mac")).or_default();
                    marks.entry("web".to_owned()).or_default().min_version = Some(version(floor));
                }
                let mut entry = AuditEntry::new("bob", "host_group_put");
                entry.host_group = Some("linux".to_owned());
                vec![entry]
            })
            .expect("the rival's change");
        if times > 1 {
            rival_commits_raising(&next, &rival, times - 1, floor);
        }
    });
}

/// The group `linux` as its row holds it: its generation and who made its last change.
fn linux_row(database: &TestDatabase) -> Vec<Vec<Option<String>>> {
    database.rows("SELECT generation::text, by_name FROM host_groups WHERE name = 'linux'")
}

/// Who made each audited change, its action and the group it names, in order.
fn audited(database: &TestDatabase) -> Vec<Vec<Option<String>>> {
    database.rows(
        "SELECT by_name, action, host_group FROM audit \
         WHERE action IN ('set_desired', 'host_group_put') ORDER BY seq",
    )
}

/// The lines `log` holds from the set in host groups, without their times.
fn set_lines(log: &Log) -> Vec<String> {
    log.text()
        .lines()
        .filter(|line| line.contains("hostkeeper_coordinator::service::group_calls"))
        .map(|line| line.split_once(' ').expect("a time").1.to_owned())
        .collect()
}

/// The line a set decided again logs at `attempt`.
fn decided_again(attempt: u32) -> String {
    format!(
        " INFO hostkeeper_coordinator::service::group_calls: a set in host groups was staged on a \
         desired state older than the store's: it is decided again on the state as stored \
         service=web attempt={attempt}"
    )
}

fn cells(cells: &[&str]) -> Vec<Option<String>> {
    cells.iter().map(|cell| Some((*cell).to_owned())).collect()
}

/// Proves: the release's two sets of one version, the Mac group's and then the Linux group's, made
/// on one coordinator one right after the other, both land: each answer is the version at
/// generation 1, each group's row holds the version at generation 1 with its number taken, memory
/// holds the version in both groups, and the audit holds one `set_desired` by `ci` for each group,
/// in the order of the sets. A third set (2.0 in the Mac group) right after them lands too, at
/// generation 2.
///
/// Catches: the second set refused `store_failed` because the first set's commit left the
/// coordinator's copy of the desired state older than the rows (the refusal a release's second
/// set-desired job got while the first job's set landed in the same second).
#[tokio::test]
async fn two_group_sets_one_right_after_the_other_both_land() {
    let grouped = Grouped::start("group-sets-pg-both-land");

    let mac = grouped.ci_sets("mac", "1.5").await.expect("the Mac set");
    let linux = grouped
        .ci_sets("linux", "1.5")
        .await
        .expect("the Linux set");
    let again = grouped.ci_sets("mac", "2.0").await.expect("the third set");

    for (answer, generation, at) in [(&mac, 1, "1.5"), (&linux, 1, "1.5"), (&again, 2, "2.0")] {
        assert_eq!(
            answer.desired.as_ref().map(|d| d.version.as_str()),
            Some(at)
        );
        assert_eq!(
            answer.record.as_ref().map(|r| r.generation),
            Some(generation)
        );
    }
    assert_eq!(
        grouped.rows(),
        vec![
            cells(&["linux", "1.5", "1", "true"]),
            cells(&["mac", "2.0", "2", "true"]),
        ]
    );
    let desired = &grouped.coordinator.desired;
    assert_eq!(
        desired
            .group_desired(&name("linux"), "web")
            .map(|d| d.version),
        Some(version("1.5"))
    );
    assert_eq!(
        desired
            .group_desired(&name("mac"), "web")
            .map(|d| d.version),
        Some(version("2.0"))
    );
    let sets: Vec<(String, Option<String>)> = grouped
        .database
        .rows("SELECT by_name, host_group FROM audit WHERE action = 'set_desired' ORDER BY seq")
        .into_iter()
        .map(|row| (row[0].clone().expect("a name"), row[1].clone()))
        .collect();
    assert_eq!(
        sets,
        vec![
            ("ci".to_owned(), Some("mac".to_owned())),
            ("ci".to_owned(), Some("linux".to_owned())),
            ("ci".to_owned(), Some("mac".to_owned())),
        ]
    );
    grouped.store.stop();
}

/// Proves: a set of the Mac group whose edit finds that a second coordinator committed a change of
/// the group `linux` after the set was decided is decided again on the rows the edit read, and
/// lands: the answer is the version at generation 1, the row of `group_desired` holds it, the rival's
/// change stands (the group `linux` at generation 2 by `bob`), the audit holds the rival's
/// `host_group_put` and then CI's `set_desired`, and the set logged once that it was decided again.
///
/// Catches: a set refused `store_failed` for a commit another coordinator made between its decision
/// and its commit, or one that writes back the copy it was decided on over the rival's change.
#[tokio::test]
async fn a_set_staged_before_another_coordinators_commit_is_decided_again_and_lands() {
    let grouped = Grouped::start("group-sets-pg-rival-once");
    let rival = Arc::new(instance(&grouped.database, "rival"));
    rival_commits(&grouped.coordinator, &rival, 1);
    let log = Log::default();
    let guard = log.install();

    let answer = grouped.ci_sets("mac", "1.5").await.expect("the set");

    drop(guard);
    assert_eq!(answer.record.map(|r| r.generation), Some(1));
    assert_eq!(grouped.rows(), vec![cells(&["mac", "1.5", "1", "true"])]);
    assert_eq!(linux_row(&grouped.database), vec![cells(&["2", "bob"])]);
    assert_eq!(
        audited(&grouped.database),
        vec![
            cells(&["bob", "host_group_put", "linux"]),
            cells(&["ci", "set_desired", "mac"]),
        ]
    );
    assert_eq!(set_lines(&log)[..1], [decided_again(1)]);
    rival.store.stop();
    grouped.store.stop();
}

/// Proves: a set whose edit finds the desired state changed by another coordinator's commit each
/// of the three times it is decided is refused `ABORTED` `state_changed`, and changes nothing: no
/// row of `group_desired`, no `set_desired` in the audit, memory without the version, while the
/// three rival changes stand (the group `linux` at generation 4). The set logs that it was decided
/// again after the first and the second attempt and, at the third, the refusal with its cause, each
/// line by exact equality.
///
/// Catches: a set decided again without end, a refusal that does not tell a store that changed
/// under the set from one that did not take the write (`store_failed`), and a refusal whose log does
/// not name the write and the cause.
#[tokio::test]
async fn a_set_that_finds_the_state_changed_three_times_is_refused_state_changed() {
    let grouped = Grouped::start("group-sets-pg-rival-thrice");
    let rival = Arc::new(instance(&grouped.database, "rival"));
    rival_commits(&grouped.coordinator, &rival, 3);
    let log = Log::default();
    let guard = log.install();

    let refused = grouped
        .ci_sets("mac", "1.5")
        .await
        .expect_err("the set is refused");

    drop(guard);
    assert_eq!(
        (refused.code(), refused.message()),
        (Code::Aborted, "state_changed")
    );
    assert_eq!(grouped.rows(), Vec::<Vec<Option<String>>>::new());
    assert_eq!(linux_row(&grouped.database), vec![cells(&["4", "bob"])]);
    assert_eq!(
        audited(&grouped.database),
        vec![cells(&["bob", "host_group_put", "linux"]); 3]
    );
    assert_eq!(
        grouped
            .coordinator
            .desired
            .group_desired(&name("mac"), "web"),
        None
    );
    assert_eq!(
        set_lines(&log),
        vec![
            decided_again(1),
            decided_again(2),
            "ERROR hostkeeper_coordinator::service::group_calls: a set in host groups was refused \
             at its commit of the groups' desired versions, the release and the audit entries: \
             nothing changed service=web host_group=mac attempt=3 why=the desired state changed \
             in the store after this change was staged on it: nothing changed, and the change is \
             to be decided again on the state as stored"
                .to_owned(),
        ]
    );
    rival.store.stop();
    grouped.store.stop();
}

/// Proves: a set decided again is decided on the state the edit read, refusals and all: when the
/// rival's commit also raised the Mac group's minimum version of `web` to 2.0, the set of 1.5 in
/// the Mac group, decided before it on no floor, is refused `FAILED_PRECONDITION`
/// `below_min_version` at its second decision, and changes nothing (no row of `group_desired`, no
/// `set_desired` in the audit), while the rival's change stands.
///
/// Catches: a set decided again on the copy it was first decided on, which would set a version
/// below the minimum another coordinator raised meanwhile.
#[tokio::test]
async fn a_set_decided_again_is_refused_by_what_the_rival_commit_changed() {
    let grouped = Grouped::start("group-sets-pg-rival-floor");
    let rival = Arc::new(instance(&grouped.database, "rival"));
    rival_commits_raising(&grouped.coordinator, &rival, 1, Some("2.0"));

    let refused = grouped
        .ci_sets("mac", "1.5")
        .await
        .expect_err("the set is refused");

    assert_eq!(
        (refused.code(), refused.message()),
        (Code::FailedPrecondition, "below_min_version")
    );
    assert_eq!(grouped.rows(), Vec::<Vec<Option<String>>>::new());
    assert_eq!(linux_row(&grouped.database), vec![cells(&["2", "bob"])]);
    assert_eq!(
        audited(&grouped.database),
        vec![cells(&["bob", "host_group_put", "linux"])]
    );
    rival.store.stop();
    grouped.store.stop();
}
