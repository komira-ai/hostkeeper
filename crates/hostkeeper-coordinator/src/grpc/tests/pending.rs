//! Every RPC of the command table is served, or listed in [`PENDING`] and answered
//! `UNIMPLEMENTED` with no message: a method added to a proto without a handler cannot pass
//! unnoticed.

use hostkeeper_proto::ctl::{COMMANDS, Command};
use tonic::Code;

use super::support::{
    CONVERGE, HEARTBEAT, SET_DESIRED, Served, call, decode, grpc_request, served,
    served_with_enrollment, updater,
};
use crate::grpc::PENDING;
use crate::grpc::gateway::is_method;
use crate::http::{Peer, Surface};
use crate::testutil::{OPERATOR, ci_token};

/// Where `path` is served and with what credentials a call of it is admitted: the heartbeat
/// listener and, for `Heartbeat`, an updater's certificate; for `Enroll`, `Poll` and `Renew`
/// nothing; the private listener with CI's token for `SetDesired` and `Converge`, and with the
/// operator token for every other method (it is admitted at those two as well).
fn credentials(path: &str) -> (Surface, Peer, Option<String>) {
    if path == HEARTBEAT {
        (Surface::Heartbeat, updater("host-1", "storage"), None)
    } else if path.starts_with("/hostkeeper.agent.") {
        (Surface::Heartbeat, None, None)
    } else if path == SET_DESIRED || path == CONVERGE {
        (Surface::Control, None, Some(ci_token()))
    } else {
        (Surface::Control, None, Some(OPERATOR.to_owned()))
    }
}

/// The path of each of `commands` that answers `UNIMPLEMENTED` to an empty request, with the
/// credentials it takes, and is not listed in `pending`.
async fn unserved(served: &Served, commands: &[Command], pending: &[&str]) -> Vec<&'static str> {
    let mut found = Vec::new();
    for command in commands {
        let (surface, peer, bearer) = credentials(command.path);
        let request = grpc_request(command.path, (), bearer.as_deref());
        let (code, _, _) = call(&served.coordinator, surface, &peer, request).await;
        if code == Code::Unimplemented && !pending.contains(&command.path) {
            found.push(command.path);
        }
    }
    found
}

/// The paths of [`PENDING`], sorted.
fn pending_sorted() -> Vec<&'static str> {
    let mut paths = PENDING.to_vec();
    paths.sort_unstable();
    paths
}

/// Proves: no RPC of the command table (the agent's service and the operator API) answers
/// `UNIMPLEMENTED` to an empty request unless [`PENDING`] lists it, each called with the
/// credentials it takes on the listener that serves it.
#[tokio::test]
async fn every_rpc_is_served_or_listed_pending() {
    let served = served_with_enrollment("grpc-pending-all");
    assert!(
        COMMANDS.len() > PENDING.len(),
        "the table lists the agent's and the operator's RPCs"
    );
    let found = unserved(&served, COMMANDS, PENDING).await;
    assert_eq!(
        found,
        Vec::<&str>::new(),
        "RPCs with no handler and no listing"
    );
}

/// Proves: the check above sees a method with no handler: with nothing listed pending it finds
/// exactly the paths of [`PENDING`], so a listing cannot be missing from the table without the
/// test failing, and no served method is mistaken for an unserved one.
#[tokio::test]
async fn the_check_finds_exactly_the_pending_methods_when_none_is_listed() {
    let served = served("grpc-pending-none-listed");
    let mut found = unserved(&served, COMMANDS, &[]).await;
    found.sort_unstable();
    assert_eq!(found, pending_sorted());
}

/// Proves: the check sees a path of a served service that has no method: it is found, and the
/// ones that are served are not.
#[tokio::test]
async fn the_check_finds_a_method_the_table_does_not_know() {
    let served = served("grpc-pending-unknown");
    let known = COMMANDS
        .iter()
        .find(|command| command.path == "/hostkeeper.operator.v1.Hosts/Abort")
        .copied()
        .expect("Hosts.Abort is in the table");
    let invented = Command {
        path: "/hostkeeper.operator.v1.Hosts/NoSuchMethod",
        ..known
    };
    let found = unserved(&served, &[known, invented], PENDING).await;
    assert_eq!(found, vec!["/hostkeeper.operator.v1.Hosts/NoSuchMethod"]);
}

/// Proves: every method [`PENDING`] lists answers `UNIMPLEMENTED` with an empty message and no
/// response message, to a caller admitted for it.
#[tokio::test]
async fn pending_methods_answer_unimplemented_with_no_message() {
    let served = served("grpc-pending-answers");
    assert_ne!(PENDING.len(), 0, "some methods are pending");
    for path in PENDING {
        let (surface, peer, bearer) = credentials(path);
        let request = grpc_request(path, (), bearer.as_deref());
        let answer = call(&served.coordinator, surface, &peer, request).await;
        assert_eq!(answer, (Code::Unimplemented, String::new(), None), "{path}");
    }
}

/// Proves: every path in [`PENDING`] is a method of the command table and of the descriptors
/// the gateway reads, listed once, so a rename in a proto leaves no stale entry.
#[test]
fn every_pending_path_is_a_method_of_the_descriptors() {
    for path in PENDING {
        assert!(
            COMMANDS.iter().any(|command| command.path == *path),
            "{path} is in the command table"
        );
        assert!(is_method(path).is_some(), "{path} is in the descriptors");
    }
    let mut once = pending_sorted();
    once.dedup();
    assert_eq!(once.len(), PENDING.len(), "no path is listed twice");
}

/// Proves: a served method answers with its message, so `UNIMPLEMENTED` above is not just what
/// an empty request gets everywhere: `Releases.Status` with the operator token answers `OK`
/// with the status of a coordinator in its quiet start.
#[tokio::test]
async fn a_served_method_answers_its_message() {
    use hostkeeper_proto::hostkeeper::operator::v1::StatusResponse;

    let served = served("grpc-pending-served");
    let request = grpc_request(
        "/hostkeeper.operator.v1.Releases/Status",
        (),
        Some(OPERATOR),
    );
    let (code, message, body) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let status: StatusResponse = decode(&body.expect("a message"));
    assert!(
        status.quiet,
        "a coordinator just started is in its quiet start"
    );
}
