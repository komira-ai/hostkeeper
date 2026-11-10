//! A package's repository in the database: `Services.SetRepository` through a coordinator over the
//! store, and the state a store opened again loads.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ServicesRefusal as Refusal;

use super::coordinator::Farm;
use crate::marks_support::{WEB, operator};
use crate::repositories_support::repository;

/// `Services.SetRepository` of `service` to `repository`, as the operator.
fn set(farm: &Farm, service: &str, repository: &str) -> Result<(), Refusal> {
    farm.rig
        .coordinator
        .set_repository(
            &operator(),
            proto::SetRepositoryRequest {
                service: service.to_owned(),
                repository: repository.to_owned(),
                reason: "moved".to_owned(),
            },
        )
        .map(drop)
}

/// Proves: a repository set through the coordinator is in the database row of the service, a store
/// opened again loads it as the service's marks.
///
/// Catches: a repository the database does not keep, so that a restart sends the service back to
/// the default.
#[test]
fn a_repository_set_survives_the_store_being_opened_again() {
    let farm = Farm::plain("pg-set-repository");
    farm.rig
        .coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(repository(
                    "acme",
                    "https://acme.example.com/apt",
                    &["testing"],
                    3,
                )),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");

    set(&farm, WEB, "acme").expect("set");
    assert_eq!(
        farm.database
            .rows("SELECT repository FROM services WHERE service = 'web'"),
        vec![vec![Some("acme".to_owned())]]
    );
    farm.store.stop();
    let (store, loaded) = farm.database.open("again").expect("opened again");
    assert_eq!(loaded.ops.services[WEB].repository.as_deref(), Some("acme"));
    store.stop();
}
