//! `Repositories` of the operator API ([`crate::operator_repositories`]) as the coordinator
//! answers it: what `Put`, `Get`, `List` and `Remove` return and audit, each refusal with the
//! registry and the audit as they were, and a restart that keeps the registry. The calls are the
//! handlers' own; the adapters' agreement over gRPC and the gateway is in `grpc/tests`.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::RepositoriesRefusal as Refusal;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry::keyring_sha256;

use crate::marks_support::{operator, rig_on, start_on};
use crate::operator::Caller;
use crate::repositories_support::{fingerprint, keyring, komira, repository};
use crate::store::{Fake, audit_failure, store_failure};
use crate::testutil::Rig;

const REASON: &str = "the first repository";

fn put_of(repository: Repository) -> proto::PutRepositoryRequest {
    proto::PutRepositoryRequest {
        repository: Some(repository),
        reason: REASON.to_owned(),
    }
}

fn get_of(name: &str) -> proto::GetRepositoryRequest {
    proto::GetRepositoryRequest {
        name: name.to_owned(),
    }
}

fn remove_of(name: &str) -> proto::RemoveRepositoryRequest {
    proto::RemoveRepositoryRequest {
        name: name.to_owned(),
        reason: "retired".to_owned(),
    }
}

fn put(rig: &Rig, repository: Repository) -> Result<proto::PutRepositoryResponse, Refusal> {
    rig.coordinator
        .repositories_put(&operator(), put_of(repository))
}

fn list(rig: &Rig) -> proto::ListRepositoriesResponse {
    rig.coordinator
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed")
}

/// The newest audit entry of the ring: its action and reason.
fn last_entry(rig: &Rig) -> (String, String, Option<String>) {
    let state = rig.coordinator.ops().state();
    let entry = state.audit.last().expect("an entry");
    (
        entry.action.clone(),
        entry.reason.clone().unwrap_or_default(),
        entry.service.clone(),
    )
}

/// Proves: `Put` adds `komira` at generation 1 and answers it with its keyring and the audit
/// entry `repository_put` whose reason is the operator's, then the name, URI, suites and
/// fingerprints; `Get` answers it with its keyring, `List` without it but with its SHA-256, the
/// registry's generation and the services of the rollout file that name it; a second repository
/// raises the generation to 2; `Remove` of it answers generation 3 and the entry
/// `repository_remove` with its name, and `List` shows the one left.
///
/// Catches: a registry that is not changed by a put, a keyring in the list, a generation that
/// does not move on a remove, a change with no audit entry or one that does not say what changed.
#[test]
fn put_get_list_and_remove_round_trip_and_are_audited() {
    let rig = rig_on("repositories-round-trip", &Fake::new(), "");
    // A fresh registry answers the built-in `komira` at generation 0, and lacks nothing.
    let fresh = list(&rig);
    assert_eq!((fresh.generation, fresh.unregistered.len()), (0, 0));
    let [builtin] = fresh.repositories.as_slice() else {
        panic!("one repository");
    };
    assert!(builtin.builtin && builtin.generation == 0);
    assert_eq!(builtin.services, ["db", "web"]);
    assert_eq!(
        builtin.platforms,
        ["linux-x86_64", "linux-arm64", "macos-arm64"]
    );

    let done = put(&rig, komira(1)).expect("put");
    let held = done.repository.expect("the repository");
    assert_eq!(held.repository, Some(komira(1)));
    assert_eq!(held.generation, 1);
    assert_eq!(held.keyring_sha256, keyring_sha256(&komira(1).keyring));
    assert_eq!(held.services, ["db", "web"]);
    let entry = done.entry.expect("the entry");
    assert_eq!(entry.action, "repository_put");
    assert_eq!(
        entry.reason.as_deref(),
        Some(
            format!(
                "{REASON}: komira https://packages.example.com/apt suites testing,stable fingerprints {}",
                fingerprint(&komira(1).keyring)
            )
            .as_str()
        )
    );
    assert_eq!(last_entry(&rig).0, "repository_put");

    let got = rig
        .coordinator
        .repositories_get(&operator(), &get_of("komira"))
        .expect("got");
    assert_eq!(got.repository, Some(held.clone()));

    let listed = list(&rig);
    assert_eq!(listed.generation, 1);
    assert!(listed.unregistered.is_empty());
    let mut bare = held.clone();
    bare.repository
        .as_mut()
        .expect("a repository")
        .keyring
        .clear();
    assert_eq!(listed.repositories, [bare]);

    let spare = repository("spare", "https://spare.example.com/", &["x"], 2);
    assert_eq!(
        put(&rig, spare)
            .expect("put")
            .repository
            .expect("held")
            .generation,
        2
    );
    let removed = rig
        .coordinator
        .repositories_remove(&operator(), remove_of("spare"))
        .expect("removed");
    assert_eq!(removed.generation, 3);
    assert_eq!(
        removed.entry.expect("the entry").action,
        "repository_remove"
    );
    assert_eq!(last_entry(&rig).1, "retired: spare");
    let listed = list(&rig);
    assert_eq!(listed.generation, 3);
    assert_eq!(listed.repositories.len(), 1);
}

/// Proves: a `Put` of a name the registry holds with another keyring and fingerprints replaces it
/// (a key rotation): `Get` answers the new keyring and the generation of the put, the other
/// repositories keep theirs, and the generation went up by one.
///
/// Catches: a rotation that keeps the old key, or that moves another repository's generation.
#[test]
fn a_put_of_a_name_the_registry_holds_rotates_its_key() {
    let rig = rig_on("repositories-rotation", &Fake::new(), "");
    put(&rig, komira(1)).expect("put");
    put(
        &rig,
        repository("spare", "https://spare.example.com/", &["x"], 2),
    )
    .expect("put");
    put(&rig, komira(3)).expect("rotated");
    let got = |name: &str| {
        rig.coordinator
            .repositories_get(&operator(), &get_of(name))
            .expect("got")
            .repository
            .expect("held")
    };
    let rotated = got("komira");
    assert_eq!(rotated.repository, Some(komira(3)));
    assert_eq!(rotated.generation, 3);
    assert_eq!(got("spare").generation, 2);
    assert_eq!(list(&rig).generation, 3);
}

/// Proves: `List` names the repositories the rollout file's services name that the registry lacks,
/// each once and in order, and none once they are put.
///
/// Catches: an unregistered repository that is not shown, which is why a host is sent none.
#[test]
fn list_names_the_repositories_services_name_that_the_registry_lacks() {
    let rig = rig_on(
        "repositories-unregistered",
        &Fake::new(),
        "repository = \"other\"",
    );
    // `komira` is built in, so only the repository the registry does not hold is lacking.
    assert_eq!(list(&rig).unregistered, ["other"]);
    put(
        &rig,
        repository("other", "https://o.example.com/", &["x"], 2),
    )
    .expect("put");
    assert!(list(&rig).unregistered.is_empty());
}

/// A way to break a repository.
type Break = Box<dyn Fn(&mut Repository)>;

fn state_of(fake: &Fake) -> (crate::repositories::Registry, usize) {
    (fake.desired().repositories, fake.audit().len())
}

/// Proves: every refusal of `Repositories` is the value that names its cause, and leaves the
/// registry and the audit as they were: `unauthorized` and `forbidden` for every method;
/// `invalid_request` for a reason that is empty or holds a control character, no repository, a
/// kind, a URI, a suite, a component, a fingerprint or a keyring of the wrong size, and a name that
/// is no repository name; `keyring_invalid` for bytes that are no keyring, a fingerprint the
/// keyring lacks, and a key none of whose fingerprints is listed; `unknown_repository` for `Get`
/// and `Remove` of a name not held; `in_use` for `Remove` of the repository a service names;
/// `builtin` for `Remove` of the built-in `komira`;
/// `conflict` for a channel that is another repository's; `capacity` for a seventeenth.
///
/// Catches: a refusal that changes the registry or writes an audit entry, a keyring fault
/// answered as another value, a removal of a repository a service names.
#[test]
fn every_refusal_changes_nothing_and_says_its_cause() {
    let fake = Fake::new();
    let rig = rig_on("repositories-refusals", &fake, "repository = \"spare\"");
    put(
        &rig,
        repository("spare", "https://spare.example.com/", &["x"], 2),
    )
    .expect("put");
    put(&rig, repository("a", "https://a.example.com/", &["b-c"], 5)).expect("put");
    let before = state_of(&fake);
    let outcome = |result: Result<(), Refusal>| {
        let seen = result.expect_err("refused");
        assert_eq!(state_of(&fake), before, "{seen:?} changed something");
        seen
    };
    let put_as = |caller: &Caller, repository: Repository| {
        rig.coordinator
            .repositories_put(caller, put_of(repository))
            .map(drop)
    };
    let ci = Caller::Ci;
    let anonymous = Caller::Anonymous;
    for (caller, refusal) in [
        (&ci, Refusal::Forbidden),
        (&anonymous, Refusal::Unauthorized),
    ] {
        assert_eq!(outcome(put_as(caller, komira(1))), refusal);
        assert_eq!(
            outcome(
                rig.coordinator
                    .repositories_get(caller, &get_of("komira"))
                    .map(drop)
            ),
            refusal
        );
        assert_eq!(
            outcome(
                rig.coordinator
                    .repositories_list(caller, &proto::ListRepositoriesRequest {})
                    .map(drop)
            ),
            refusal
        );
        assert_eq!(
            outcome(
                rig.coordinator
                    .repositories_remove(caller, remove_of("spare"))
                    .map(drop)
            ),
            refusal
        );
    }

    let me = operator();
    let with_reason = |reason: &str| proto::PutRepositoryRequest {
        reason: reason.to_owned(),
        ..put_of(komira(1))
    };
    for reason in ["", "  ", "line\nbreak", &"x".repeat(257)] {
        assert_eq!(
            outcome(
                rig.coordinator
                    .repositories_put(&me, with_reason(reason))
                    .map(drop)
            ),
            Refusal::InvalidRequest,
            "reason {reason:?}"
        );
    }
    let no_repository = proto::PutRepositoryRequest {
        repository: None,
        reason: REASON.to_owned(),
    };
    assert_eq!(
        outcome(
            rig.coordinator
                .repositories_put(&me, no_repository)
                .map(drop)
        ),
        Refusal::InvalidRequest
    );
    let broken: [Break; 8] = [
        Box::new(|r| r.kind = "rpm".to_owned()),
        Box::new(|r| r.uri = "http://packages.example.com/".to_owned()),
        Box::new(|r| r.suites = vec!["Stable".to_owned()]),
        Box::new(|r| r.components.clear()),
        Box::new(|r| r.fingerprints = vec!["abc".to_owned()]),
        Box::new(|r| r.name = "Komira".to_owned()),
        Box::new(|r| r.keyring.clear()),
        Box::new(|r| r.keyring = vec![0; 64 * 1024 + 1]),
    ];
    for change in broken {
        let mut bad = komira(1);
        change(&mut bad);
        assert_eq!(outcome(put_as(&me, bad)), Refusal::InvalidRequest);
    }
    let mut not_a_keyring = komira(1);
    not_a_keyring.keyring = vec![1, 2, 3];
    assert_eq!(outcome(put_as(&me, not_a_keyring)), Refusal::KeyringInvalid);
    let mut wrong_print = komira(1);
    wrong_print.fingerprints = vec![fingerprint(&keyring(9))];
    assert_eq!(outcome(put_as(&me, wrong_print)), Refusal::KeyringInvalid);
    let mut unlisted = komira(1);
    unlisted.keyring.extend(keyring(8));
    assert_eq!(outcome(put_as(&me, unlisted)), Refusal::KeyringInvalid);

    for name in ["Bad Name", ""] {
        assert_eq!(
            outcome(
                rig.coordinator
                    .repositories_get(&me, &get_of(name))
                    .map(drop)
            ),
            Refusal::InvalidRequest
        );
        assert_eq!(
            outcome(
                rig.coordinator
                    .repositories_remove(&me, remove_of(name))
                    .map(drop)
            ),
            Refusal::InvalidRequest
        );
    }
    let no_reason = proto::RemoveRepositoryRequest {
        reason: String::new(),
        ..remove_of("spare")
    };
    assert_eq!(
        outcome(
            rig.coordinator
                .repositories_remove(&me, no_reason)
                .map(drop)
        ),
        Refusal::InvalidRequest
    );
    assert_eq!(
        outcome(
            rig.coordinator
                .repositories_get(&me, &get_of("absent"))
                .map(drop)
        ),
        Refusal::UnknownRepository
    );
    assert_eq!(
        outcome(
            rig.coordinator
                .repositories_remove(&me, remove_of("absent"))
                .map(drop)
        ),
        Refusal::UnknownRepository
    );
    assert_eq!(
        outcome(
            rig.coordinator
                .repositories_remove(&me, remove_of("spare"))
                .map(drop)
        ),
        Refusal::InUse
    );
    assert_eq!(
        outcome(
            rig.coordinator
                .repositories_remove(&me, remove_of("komira"))
                .map(drop)
        ),
        Refusal::Builtin
    );
    // `a-b` + `c` and `a` + `b-c` are one channel, `a-b-c`.
    let clashing = repository("a-b", "https://c.example.com/", &["c"], 6);
    assert_eq!(outcome(put_as(&me, clashing)), Refusal::Conflict);
}

/// Proves: a new name at 16 repositories is `capacity` and a replacement at 16 is taken, with the
/// registry unchanged by the refusal.
///
/// Catches: a cap that is not held, or one that counts a replacement.
#[test]
fn a_seventeenth_repository_is_capacity_and_a_replacement_is_not() {
    let fake = Fake::new();
    let rig = rig_on("repositories-capacity", &fake, "");
    for n in 0..16u8 {
        let name = format!("repo-{n}");
        put(
            &rig,
            repository(&name, "https://r.example.com/", &[&format!("s{n}")], n),
        )
        .expect("put");
    }
    let before = state_of(&fake);
    let new = repository("repo-new", "https://r.example.com/", &["new"], 1);
    assert_eq!(put(&rig, new).expect_err("refused"), Refusal::Capacity);
    assert_eq!(state_of(&fake), before);
    let again = repository("repo-3", "https://r.example.com/", &["s3"], 77);
    assert!(put(&rig, again).is_ok());
}

/// Proves: a commit that fails is `store_failed` and one whose audit line cannot be kept is
/// `audit_unavailable`, for `Put` and for `Remove`, and each leaves the registry in memory and in
/// the store as it was; once the store takes commits again the same call lands.
///
/// Catches: a registry that stands in memory after a commit that failed.
#[test]
fn a_commit_that_fails_changes_nothing_and_a_resend_lands() {
    let fake = Fake::new();
    let rig = rig_on("repositories-commit-fails", &fake, "");
    put(
        &rig,
        repository("spare", "https://spare.example.com/", &["x"], 2),
    )
    .expect("put");
    let before = state_of(&fake);
    for (failure, refusal) in [
        (store_failure as fn() -> _, Refusal::StoreFailed),
        (audit_failure, Refusal::AuditUnavailable),
    ] {
        fake.refuse(move |_| Some(failure()));
        assert_eq!(put(&rig, komira(1)).expect_err("refused"), refusal);
        assert_eq!(
            rig.coordinator
                .repositories_remove(&operator(), remove_of("spare"))
                .expect_err("refused"),
            refusal
        );
        assert_eq!(state_of(&fake), before);
        assert_eq!(list(&rig).generation, 1);
    }
    fake.heal();
    assert!(put(&rig, komira(1)).is_ok());
    assert_eq!(list(&rig).generation, 2);
}

/// Proves: a coordinator started again on what the store holds has the registry, its generation
/// and each repository's generation, and its next `Put` is the generation after; on the files of
/// the state file too (`<state-file>.repositories`).
///
/// Catches: a registry that is kept in memory only.
#[test]
fn a_restart_keeps_the_registry_on_the_fake_and_on_the_files() {
    let fake = Fake::new();
    let rig = rig_on("repositories-restart-fake", &fake, "");
    put(&rig, komira(1)).expect("put");
    put(
        &rig,
        repository("spare", "https://spare.example.com/", &["x"], 2),
    )
    .expect("put");
    let again = start_on(&fake, &rig.clock, "");
    let listed = again
        .repositories_list(&operator(), &proto::ListRepositoriesRequest {})
        .expect("listed");
    assert_eq!(listed.generation, 2);
    assert_eq!(
        listed
            .repositories
            .iter()
            .map(|r| r.generation)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let next = again
        .repositories_put(&operator(), put_of(komira(3)))
        .expect("put");
    assert_eq!(next.repository.expect("held").generation, 3);

    let mut files = crate::marks_support::rig("repositories-restart-files", "");
    put(&files, komira(1)).expect("put");
    files.coordinator = crate::marks_support::start(&files.dir, &files.clock, "");
    let kept = rig_get(&files, "komira");
    assert_eq!(kept.repository, Some(komira(1)));
    assert!(files.dir.path().join("desired.json.repositories").exists());
    assert_eq!(list(&files).generation, 1);
}

fn rig_get(rig: &Rig, name: &str) -> proto::RegisteredRepository {
    rig.coordinator
        .repositories_get(&operator(), &get_of(name))
        .expect("got")
        .repository
        .expect("held")
}
