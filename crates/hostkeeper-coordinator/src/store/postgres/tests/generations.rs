//! Generations and write ids over the Postgres store (`docs/design/coordinator-storage.md`,
//! section 3.2), end to end through `Releases`: a coordinator over a database of its own, its
//! `SetDesired` and `Converge` called as CI's router or the operator's admits them, and the
//! columns of `desired` read beside it.

use std::collections::HashMap;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use tonic::{Code, Request};

use super::TestDatabase;
use super::coordinator::start_on_postgres;
use crate::grpc::Admitted;
use crate::grpc::operator::ReleasesService;
use crate::marks_support::WEB;
use crate::service::Coordinator;
use crate::store::Postgres;
use crate::testutil::{OPERATOR, Rig, ci_token};

/// A coordinator running `web` and `db` over a store of `database`, and its `Releases`.
struct Releasing {
    coordinator: Arc<Coordinator>,
    store: Postgres,
    _rig: Rig,
}

impl Releasing {
    fn start(test: &str, database: &TestDatabase) -> Releasing {
        let (store, loaded) = database.open("coordinator").expect("the store opens");
        let rig = Rig::new(test);
        let marker = rig.dir.path().join("halt.marker");
        let coordinator = Arc::new(start_on_postgres(&rig, &store, loaded, &marker));
        Releasing {
            coordinator,
            store,
            _rig: rig,
        }
    }

    fn releases(&self) -> ReleasesService {
        ReleasesService::new(Arc::clone(&self.coordinator))
    }

    /// `message` as the router admits it for the operator's token.
    async fn as_operator<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        request.extensions_mut().insert(Admitted::Operator {
            caller: self.coordinator.caller(Some(OPERATOR)).await,
        });
        request
    }

    /// `message` as the router admits it for a CI token nobody used.
    async fn as_ci<T>(&self, message: T) -> Request<T> {
        let verified = self
            .coordinator
            .ci(Some(&ci_token()))
            .await
            .expect("CI's token is accepted");
        let mut request = Request::new(message);
        request.extensions_mut().insert(Admitted::Ci(verified));
        request
    }

    /// The record `SetDesired` of `request` answers, as CI.
    async fn ci_sets(&self, request: proto::SetDesiredRequest) -> proto::WriteRecord {
        self.releases()
            .set_desired(self.as_ci(request).await)
            .await
            .expect("the set is accepted")
            .into_inner()
            .record
            .expect("a store that keeps records answers one")
    }

    /// The record `Converge` of `web` shows, as CI.
    async fn converged(&self) -> proto::WriteRecord {
        let request = proto::ConvergeRequest {
            service: WEB.to_owned(),
            host_group: String::new(),
        };
        self.releases()
            .converge(self.as_ci(request).await)
            .await
            .expect("the convergence")
            .into_inner()
            .convergence
            .and_then(|c| c.record)
            .expect("a store that keeps records shows one")
    }
}

/// A set of `version` of `web` whose storage minimum is `storage`.
fn set(version: &str, storage: u32) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: WEB.to_owned(),
        desired: Some(proto::DesiredState {
            version: version.to_owned(),
            max_updating: HashMap::new(),
            min_up: HashMap::from([("storage".to_owned(), storage)]),
            package_digest: None,
            release_index_digest: None,
        }),
        ..proto::SetDesiredRequest::default()
    }
}

/// [`set`] with the write id `id`.
fn set_with_id(version: &str, storage: u32, id: &str) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        write_id: Some(id.to_owned()),
        ..set(version, storage)
    }
}

/// [`set`] over the generation `generation`.
fn set_over(version: &str, generation: u64) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        if_generation: Some(generation),
        ..set(version, 1)
    }
}

/// The record of the generation `generation` and nothing else.
fn record(generation: u64) -> proto::WriteRecord {
    proto::WriteRecord {
        generation,
        ..proto::WriteRecord::default()
    }
}

/// The four columns of `web`'s row of `desired`, as text.
fn columns(database: &TestDatabase) -> Vec<Vec<Option<String>>> {
    database.rows(
        "SELECT generation, write_id, replaced_version, replaced_min_up::text FROM desired \
         WHERE service = 'web'",
    )
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

/// Proves: over the database every accepted set raises the generation by one, a set identical
/// to the state held included: two identical sets answer generations 1 and 2, each is a commit
/// of its own (the lease's counter moves on by one each), the row holds generation 2,
/// `Converge` shows it, and a start again on the database loads it.
///
/// Catches: an identical set skipped as unchanged over the database, so a writer could not tell
/// from the generation that its set landed; a generation kept in memory only.
#[tokio::test]
async fn an_accepted_write_raises_the_generation_even_for_an_identical_set() {
    let database = TestDatabase::new();
    let farm = Releasing::start("pg-generation-identical", &database);
    assert_eq!(farm.converged().await, record(0));
    let commits = || database.rows("SELECT commits FROM lease");
    let before = commits();
    assert_eq!(farm.ci_sets(set("1.0", 1)).await, record(1));
    assert_eq!(farm.ci_sets(set("1.0", 1)).await, record(2));
    let after: i64 = commits()[0][0]
        .clone()
        .expect("a count")
        .parse()
        .expect("a number");
    let was: i64 = before[0][0]
        .clone()
        .expect("a count")
        .parse()
        .expect("a number");
    assert_eq!(after - was, 2);
    assert_eq!(columns(&database), vec![vec![text("2"), None, None, None]]);
    assert_eq!(farm.converged().await, record(2));

    farm.store.stop();
    let (again, loaded) = database.open("again").expect("the store opens again");
    assert_eq!(loaded.desired.records["web"].generation, 2);
    again.stop();
}

/// Proves: a set as the operator whose `if_generation` is not the service's generation is
/// refused `ABORTED` `generation_conflict`, and nothing changes: not the desired state, not its
/// row, not the audit table (the operator's `set_desired` entry is not written), not the lease's
/// counter. The same set over the generation now is taken and raises it.
///
/// Catches: a precondition checked after the operator's audit entry is written, or after the
/// state is committed; one read as "at least".
#[tokio::test]
async fn a_stale_if_generation_is_refused_generation_conflict_and_changes_nothing() {
    let database = TestDatabase::new();
    let farm = Releasing::start("pg-generation-stale", &database);
    let first = farm
        .releases()
        .set_desired(farm.as_operator(set("1.0", 1)).await)
        .await
        .expect("the first set");
    assert_eq!(first.into_inner().record, Some(record(1)));
    let snapshot = || {
        (
            database.rows("SELECT version, generation FROM desired"),
            database.rows("SELECT seq, action FROM audit ORDER BY seq"),
            database.rows("SELECT commits FROM lease"),
        )
    };
    let before = snapshot();
    for stale in [0, 2] {
        let refused = farm
            .releases()
            .set_desired(farm.as_operator(set_over("2.0", stale)).await)
            .await
            .expect_err("a stale generation is refused");
        assert_eq!(
            (refused.code(), refused.message()),
            (Code::Aborted, "generation_conflict")
        );
    }
    assert_eq!(snapshot(), before);
    let desired = farm.coordinator.desired.get(WEB).expect("a state");
    assert_eq!(desired.version.as_str(), "1.0");

    let taken = farm
        .releases()
        .set_desired(farm.as_operator(set_over("2.0", 1)).await)
        .await
        .expect("the set over the generation now");
    assert_eq!(taken.into_inner().record.map(|r| r.generation), Some(2));
    farm.store.stop();
}

/// Proves: the record a set answers, `Converge` shows and a start again loads holds the last
/// write's id and the version and minimums the last change of version replaced: none before a
/// second version; after a change of version the version before with its minimums (the format's
/// `frontend` named, as the state was answered); a write of the same version without an id
/// clears the id and keeps what was replaced. The columns of `desired` hold the same.
///
/// Catches: a write id kept from an earlier write, a replaced state taken from the write itself
/// or lost at a restart, and a writer that cannot find its write in `Converge`.
#[tokio::test]
async fn the_write_id_and_the_replaced_state_come_back_in_the_record() {
    let database = TestDatabase::new();
    let farm = Releasing::start("pg-generation-record", &database);
    let first = farm.ci_sets(set_with_id("1.0", 2, "ci-1")).await;
    assert_eq!(
        first,
        proto::WriteRecord {
            generation: 1,
            replaced: None,
            write_id: text("ci-1"),
        }
    );
    let replaced = proto::Replaced {
        version: "1.0".to_owned(),
        min_up: HashMap::from([("storage".to_owned(), 2), ("frontend".to_owned(), 0)]),
    };
    let second = proto::WriteRecord {
        generation: 2,
        replaced: Some(replaced.clone()),
        write_id: text("ci-2"),
    };
    assert_eq!(farm.ci_sets(set_with_id("2.0", 1, "ci-2")).await, second);
    assert_eq!(farm.converged().await, second);
    assert_eq!(
        columns(&database),
        vec![vec![
            text("2"),
            text("ci-2"),
            text("1.0"),
            text(r#"{"storage": 2}"#)
        ]]
    );

    let third = proto::WriteRecord {
        generation: 3,
        replaced: Some(replaced),
        write_id: None,
    };
    assert_eq!(farm.ci_sets(set("2.0", 1)).await, third);
    farm.store.stop();

    let restarted = Releasing::start("pg-generation-record-again", &database);
    assert_eq!(restarted.converged().await, third);
    restarted.store.stop();
}
