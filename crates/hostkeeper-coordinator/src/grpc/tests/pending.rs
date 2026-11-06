//! Every RPC of the command table is served: none answers `UNIMPLEMENTED`, and [`PENDING`]
//! lists nothing. A method added to a proto without a handler cannot pass unnoticed.

use hostkeeper_proto::ctl::{COMMANDS, Command};
use tonic::Code;

use super::support::{
    CONVERGE, HEARTBEAT, SET_DESIRED, Served, call, decode, grpc_request, served,
    served_with_enrollment, updater,
};
use crate::grpc::PENDING;
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

/// Proves: [`PENDING`] lists no method: every method of the operator API has a handler.
#[test]
fn no_method_is_pending() {
    assert_eq!(PENDING, &[] as &[&str]);
}

/// Proves: no RPC of the command table (the agent's service and the operator API) answers
/// `UNIMPLEMENTED` to an empty request, each called with the credentials it takes on the
/// listener that serves it, with nothing excused; and the table is not empty, so the check
/// called something.
#[tokio::test]
async fn every_rpc_answers_something_other_than_unimplemented() {
    let served = served_with_enrollment("grpc-pending-all");
    assert!(
        !COMMANDS.is_empty(),
        "the table lists the agent's and the operator's RPCs"
    );
    let found = unserved(&served, COMMANDS, &[]).await;
    assert_eq!(found, Vec::<&str>::new(), "RPCs with no handler");
}

/// Proves: the check above is not blind to an RPC with no handler: of `Hosts.List`, which is
/// served, and `Hosts.NoSuchMethod`, a path of the same served service that has no method, it
/// finds the second alone, and does not find it when it is excused.
#[tokio::test]
async fn the_check_finds_a_method_the_table_does_not_know() {
    let served = served("grpc-pending-unknown");
    let known = COMMANDS
        .iter()
        .find(|command| command.path == "/hostkeeper.operator.v1.Hosts/List")
        .copied()
        .expect("Hosts.List is in the table");
    let invented = Command {
        path: "/hostkeeper.operator.v1.Hosts/NoSuchMethod",
        ..known
    };
    let found = unserved(&served, &[known, invented], &[]).await;
    assert_eq!(found, vec!["/hostkeeper.operator.v1.Hosts/NoSuchMethod"]);
    let excused = unserved(&served, &[known, invented], &[invented.path]).await;
    assert_eq!(excused, Vec::<&str>::new());
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
