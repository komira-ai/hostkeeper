//! The limits of the JSON surfaces on the gRPC ones: the service limit of a heartbeat is one
//! number on both transports and a list over it is handled alike, and a gRPC message over the
//! byte limit of its route is refused where one at the limit is read.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::wire::{MAX_HEARTBEAT_BYTES, MAX_SERVICES};
use prost::Message;
use serde_json::json;
use tonic::Code;

use super::heartbeat::{
    HEARTBEAT, Heard, Svc, beat, farm, farm_enrolled, grpc_call, heard_json, heard_proto,
    json_beat, json_text, proto_message, svc, updater,
};
use crate::enrollment::MAX_MESSAGE_BYTES;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enroll_request};
use crate::http::{MAX_OPERATOR_BYTES, Surface};
use crate::testutil::OPERATOR;

/// The services of a heartbeat that lists `count` of them: `kbf`, then `s01`, `s02`, ...
fn services(count: usize) -> Vec<Svc> {
    std::iter::once(svc("kbf", "1.0"))
        .chain((1..count).map(|n| svc(&format!("s{n:02}"), "1.0")))
        .collect()
}

/// Proves that the service limit of a heartbeat is one number on both transports: the wire's
/// `MAX_SERVICES` and the proto's `MAX_SERVICES_PER_HEARTBEAT` are equal, and are the limit
/// both enforce. The crates cannot share the constant (neither depends on the other, and a
/// shared one would make the wire depend on the protos), so this test is what holds them
/// together: a list of that many services is answered over JSON and gRPC, and one more is not.
#[tokio::test]
async fn the_wire_and_proto_service_limits_are_one_constant() {
    assert_eq!(MAX_SERVICES, hostkeeper_proto::MAX_SERVICES_PER_HEARTBEAT);
    let farm = farm("grpc-limit-constant").await;
    let who = updater("d1", "daemon");
    let at_the_limit = services(MAX_SERVICES);
    let (code, body) = json_beat(&farm.coordinator, &who, &json_text(&at_the_limit)).await;
    let over_json = heard_json(code, &body);
    let over_grpc =
        heard_proto(&beat(&farm.coordinator, &who, &proto_message(&at_the_limit)).await);
    assert_eq!(over_json, over_grpc);
    assert!(
        matches!(over_json, Heard::Told { .. }),
        "{MAX_SERVICES} services are answered"
    );

    let over = services(MAX_SERVICES + 1);
    let (code, body) = json_beat(&farm.coordinator, &who, &json_text(&over)).await;
    let reply = beat(&farm.coordinator, &who, &proto_message(&over)).await;
    assert_eq!(heard_json(code, &body), Heard::Refused);
    assert_eq!(heard_proto(&reply), Heard::Refused);
}

/// Proves that a service list over the limit is handled the same way on both transports: the
/// list is dropped and counted, not the heartbeat. A host that does not announce `order-v1`
/// (its `kbf` entry is the top level) is still recorded as a legacy-service host on each, with
/// the same facts, and each farm counts one dropped list; a host that announced `order-v1`
/// (no legacy top level to fall back to) is refused on each, and each counts one dropped list.
#[tokio::test]
async fn a_service_list_over_the_limit_is_refused_on_grpc_and_json_alike() {
    let over_json = farm("grpc-over-json").await;
    let over_grpc = farm("grpc-over-grpc").await;
    let who = updater("d1", "daemon");
    let others: Vec<Svc> = (1..=MAX_SERVICES + 1)
        .map(|n| svc(&format!("s{n:02}"), "1.0"))
        .collect();

    // No `order-v1`: kbf is the top level of the JSON heartbeat.
    let listed: Vec<_> = others
        .iter()
        .map(|one| json!({"name": one.name, "version": one.version, "health": "healthy"}))
        .collect();
    let body = json!({
        "platform": "linux-x86_64", "version": "1.0", "health": "healthy", "services": listed,
    })
    .to_string();
    let (code, _) = json_beat(&over_json.coordinator, &who, &body).await;
    assert_eq!(code, 200, "the heartbeat is answered");
    let mut message = super::heartbeat::legacy_beat("1.0", None);
    message.services.extend(proto_message(&others).services);
    assert_eq!(message.services.len(), MAX_SERVICES + 2);
    let reply = beat(&over_grpc.coordinator, &who, &message).await;
    assert_eq!(reply.token(), (Code::Ok, ""), "the heartbeat is answered");
    let (json_hosts, json_dropped) = over_json.heard_from();
    let (grpc_hosts, grpc_dropped) = over_grpc.heard_from();
    assert_eq!(json_hosts, [("d1".to_owned(), "daemon".to_owned())]);
    assert_eq!(grpc_hosts, json_hosts);
    assert_eq!((json_dropped, grpc_dropped), (1, 1));
    let facts = |farm: &super::heartbeat::Farm| {
        let status = farm.coordinator.status(Some(OPERATOR)).expect("status");
        serde_json::to_value(&status.hosts[0].beat).expect("JSON")
    };
    assert_eq!(facts(&over_grpc), facts(&over_json));
    assert_eq!(facts(&over_json)["version"], "1.0");
    assert!(facts(&over_json).get("services").is_none());

    // `order-v1`: the top level is the agent, and the dropped list leaves nothing to answer.
    let announced = services(MAX_SERVICES + 1);
    let (code, body) = json_beat(&over_json.coordinator, &who, &json_text(&announced)).await;
    let reply = beat(&over_grpc.coordinator, &who, &proto_message(&announced)).await;
    assert_eq!(heard_json(code, &body), Heard::Refused);
    assert_eq!(heard_proto(&reply), Heard::Refused);
    assert_eq!(over_json.heard_from().1, 2);
    assert_eq!(over_grpc.heard_from().1, 2);
}

/// `message` with a length-delimited field the message does not define, as long as it takes
/// to make the encoding `total` bytes.
fn padded(mut message: Vec<u8>, total: usize) -> Vec<u8> {
    fn varint(mut value: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let low = u8::try_from(value & 0x7F).expect("seven bits");
            value >>= 7;
            if value == 0 {
                bytes.push(low);
                return bytes;
            }
            bytes.push(low | 0x80);
        }
    }
    let need = total
        .checked_sub(message.len())
        .expect("the message is no longer than the total");
    if need == 0 {
        return message;
    }
    // One byte of tag (field 15, length-delimited), the length, and as many bytes as it says.
    let length = (0..need)
        .find(|n| 1 + varint(*n).len() + n == need)
        .expect("a padding of that size");
    message.push(0x7A);
    message.extend(varint(length));
    message.resize(total, 0);
    message
}

/// Proves the size of a message the same on every method: a message of the limit of its route
/// (and one byte under) is read, and one byte over is refused `OUT_OF_RANGE` with the limit in
/// the message, whatever the credential: a heartbeat at 16 KiB, `Enroll`, `Poll` and `Renew`
/// at 8 KiB, an operator's call at 4 KiB.
#[tokio::test]
async fn oversize_grpc_messages_are_refused_at_the_json_limits() {
    assert_eq!(MAX_HEARTBEAT_BYTES, 16 * 1024);
    assert_eq!(MAX_MESSAGE_BYTES, 8 * 1024);
    assert_eq!(MAX_OPERATOR_BYTES, 4 * 1024);
    let farm = farm("grpc-sizes").await;
    let who = updater("d1", "daemon");
    let beat_bytes = proto_message(&[svc("kbf", "2.0")]).encode_to_vec();
    let audit = operator::ListAuditRequest::default().encode_to_vec();
    for under in [MAX_HEARTBEAT_BYTES - 1, MAX_HEARTBEAT_BYTES] {
        let message = padded(beat_bytes.clone(), under);
        assert_eq!(message.len(), under);
        let reply = grpc_call(
            &farm.coordinator,
            Surface::Heartbeat,
            &who,
            HEARTBEAT,
            None,
            &message,
        )
        .await;
        assert_eq!(reply.token(), (Code::Ok, ""), "{under} bytes");
    }
    let over = padded(beat_bytes, MAX_HEARTBEAT_BYTES + 1);
    let reply = grpc_call(
        &farm.coordinator,
        Surface::Heartbeat,
        &who,
        HEARTBEAT,
        None,
        &over,
    )
    .await;
    assert_eq!(reply.status.code(), Code::OutOfRange);
    assert!(
        reply.status.message().contains("the limit is: 16384 bytes"),
        "{}",
        reply.status.message()
    );

    for under in [MAX_OPERATOR_BYTES - 1, MAX_OPERATOR_BYTES] {
        let reply = grpc_call(
            &farm.coordinator,
            Surface::Control,
            &None,
            "/hostkeeper.operator.v1.Audit/List",
            Some(OPERATOR),
            &padded(audit.clone(), under),
        )
        .await;
        assert_eq!(reply.token(), (Code::Ok, ""), "{under} bytes");
        assert!(
            reply
                .decode::<operator::ListAuditResponse>()
                .entries
                .is_empty()
        );
    }
    let reply = grpc_call(
        &farm.coordinator,
        Surface::Control,
        &None,
        "/hostkeeper.operator.v1.Audit/List",
        Some(OPERATOR),
        &padded(audit, MAX_OPERATOR_BYTES + 1),
    )
    .await;
    assert_eq!(reply.status.code(), Code::OutOfRange);
    assert!(
        reply.status.message().contains("the limit is: 4096 bytes"),
        "{}",
        reply.status.message()
    );
}

/// Proves the size of an enrollment message: `Enroll` at 8 KiB is read and answered, and
/// `Enroll`, `Poll` and `Renew` one byte over are refused `OUT_OF_RANGE`; at the limit `Poll`
/// and `Renew` are read (and refused for what they say, not for their size).
#[tokio::test]
async fn enrollment_messages_are_refused_over_eight_kib() {
    let enrollment = EnrollmentRig::new("grpc-sizes-enrollment");
    let farm = farm_enrolled("grpc-sizes-enrolled", &enrollment.enrollment).await;
    let key = HostKey::new();
    let enroll = enroll_request(&key, "machine-1").encode_to_vec();
    let poll = agent::PollRequest::default().encode_to_vec();
    let renew = agent::RenewRequest::default().encode_to_vec();
    let call = |path: &'static str, message: Vec<u8>| {
        let coordinator = farm.coordinator.clone();
        async move {
            grpc_call(
                &coordinator,
                Surface::Heartbeat,
                &None,
                path,
                None,
                &message,
            )
            .await
        }
    };
    let enroll_path = "/hostkeeper.agent.v1.Coordinator/Enroll";
    let poll_path = "/hostkeeper.agent.v1.Coordinator/Poll";
    let renew_path = "/hostkeeper.agent.v1.Coordinator/Renew";

    let at = call(enroll_path, padded(enroll.clone(), MAX_MESSAGE_BYTES)).await;
    assert_eq!(at.token(), (Code::Ok, ""));
    assert_eq!(
        at.decode::<agent::EnrollResponse>().request_id,
        key.key_hash().to_hex()
    );
    let at = call(poll_path, padded(poll.clone(), MAX_MESSAGE_BYTES)).await;
    assert_eq!(at.token(), (Code::NotFound, "unknown_request"));
    let at = call(renew_path, padded(renew.clone(), MAX_MESSAGE_BYTES)).await;
    assert_eq!(at.token(), (Code::PermissionDenied, "forbidden"));

    for (path, message) in [
        (enroll_path, enroll),
        (poll_path, poll),
        (renew_path, renew),
    ] {
        let over = call(path, padded(message, MAX_MESSAGE_BYTES + 1)).await;
        assert_eq!(over.status.code(), Code::OutOfRange, "{path}");
        assert!(
            over.status.message().contains("the limit is: 8192 bytes"),
            "{path}: {}",
            over.status.message()
        );
    }
}
