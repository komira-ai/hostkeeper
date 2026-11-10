//! Every RPC of the command table is served but those [`PENDING`] lists, the five of
//! `HostGroups`, which answer `UNIMPLEMENTED`. A method added to a proto without a handler
//! cannot pass unnoticed.

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

/// The gRPC path of each method of `HostGroups`.
const HOST_GROUPS: [&str; 5] = [
    "/hostkeeper.operator.v1.HostGroups/List",
    "/hostkeeper.operator.v1.HostGroups/Get",
    "/hostkeeper.operator.v1.HostGroups/Put",
    "/hostkeeper.operator.v1.HostGroups/Remove",
    "/hostkeeper.operator.v1.HostGroups/Move",
];

/// Proves: [`PENDING`] lists exactly the five methods of `HostGroups`, and each is a method of
/// the compiled descriptor set (the command table lists it), so a path misspelt here or a method
/// the proto drops cannot hide behind the excuse.
#[test]
fn only_the_host_groups_methods_are_pending_and_each_is_in_the_descriptor_set() {
    assert_eq!(PENDING, &HOST_GROUPS[..]);
    for path in PENDING {
        assert!(
            COMMANDS.iter().any(|command| command.path == *path),
            "{path} is a method of the descriptor set"
        );
    }
}

/// Proves: no RPC of the command table (the agent's service and the operator API) answers
/// `UNIMPLEMENTED` to an empty request, each called with the credentials it takes on the
/// listener that serves it, but those [`PENDING`] lists; and the table is not empty, so the
/// check called something.
#[tokio::test]
async fn every_rpc_answers_something_other_than_unimplemented() {
    let served = served_with_enrollment("grpc-pending-all");
    assert!(
        !COMMANDS.is_empty(),
        "the table lists the agent's and the operator's RPCs"
    );
    let found = unserved(&served, COMMANDS, PENDING).await;
    assert_eq!(found, Vec::<&str>::new(), "RPCs with no handler");
}

/// Proves: each method [`PENDING`] lists answers `UNIMPLEMENTED` with an empty message and no
/// response message on the private listener, with the operator token and with no token at all
/// (the refusal comes before any caller check and changes nothing), and the heartbeat listener
/// does not serve it either.
#[tokio::test]
async fn each_pending_method_answers_unimplemented_with_nothing_else() {
    let served = served("grpc-pending-host-groups");
    for path in PENDING {
        for (surface, bearer) in [
            (Surface::Control, Some(OPERATOR)),
            (Surface::Control, None),
            (Surface::Heartbeat, Some(OPERATOR)),
            (Surface::Heartbeat, None),
        ] {
            let request = grpc_request(path, (), bearer);
            let (code, message, body) = call(&served.coordinator, surface, &None, request).await;
            assert_eq!(
                (code, message.as_str(), body),
                (Code::Unimplemented, "", None),
                "{path} on {surface:?} with {bearer:?}"
            );
        }
    }
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
