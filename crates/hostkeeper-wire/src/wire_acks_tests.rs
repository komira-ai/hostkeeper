//! The acks of an answer (`acks`): written only when there is one, read by the agent that
//! announced `verdict-acks-v1`, and refused by the readers that predate it.

use serde::Deserialize;

use crate::wire::{
    Answer, MAX_ACKS, OrderId, Reply, ServiceReply, VERDICT_ACKS_FEATURE, VerdictAck,
};

/// The answer reader of an agent that predates `features`, `halts` and `acks`: strict, as
/// shipped.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code, reason = "the fields exist to be read by the derive")]
struct FrozenReply {
    reply: String,
    service: Option<String>,
    version: Option<String>,
}

fn ack(service: &str, order: &str, order_try: u64) -> VerdictAck {
    VerdictAck {
        service: service.to_owned(),
        order_id: OrderId::parse(order).expect("an order id"),
        order_try,
    }
}

fn answer(text: &str) -> Result<Answer, serde_json::Error> {
    serde_json::from_str(text)
}

fn featured() -> Answer {
    Answer {
        features: Some(vec![VERDICT_ACKS_FEATURE.to_owned()]),
        ..Answer::of(ServiceReply::Nothing)
    }
}

/// Catches: `acks` written when it is empty, which would change the bytes every agent reads
/// today: an answer that acknowledges nothing is the reply alone, or the reply and `features`,
/// as it was before the field existed.
#[test]
fn an_answer_without_acks_is_written_as_it_was() {
    assert_eq!(
        serde_json::to_string(&Answer::of(ServiceReply::Nothing)).expect("json"),
        r#"{"reply":"nothing"}"#
    );
    assert_eq!(
        serde_json::to_string(&featured()).expect("json"),
        r#"{"reply":"nothing","features":["verdict-acks-v1"]}"#
    );
}

/// Catches: the acks lost, misplaced or reordered on the way out: they follow `features` and
/// `halts`, in the order the coordinator gave them, each with its service, order id and try,
/// and are read back as written.
#[test]
fn acks_are_written_last_and_read_back() {
    let sent = Answer {
        reply: ServiceReply::Update {
            service: Some("web".to_owned()),
            version: crate::version::Version::parse("2.0").expect("a version"),
        },
        halts: [("web".to_owned(), 3)].into_iter().collect(),
        acks: vec![ack("web", "update-web-1.0", 2), ack("db", "force-1", 1)],
        ..featured()
    };
    let text = serde_json::to_string(&sent).expect("json");
    assert_eq!(
        text,
        concat!(
            r#"{"reply":"update","service":"web","version":"2.0","#,
            r#""features":["verdict-acks-v1"],"halts":{"web":3},"#,
            r#""acks":[{"service":"web","order_id":"update-web-1.0","order_try":2},"#,
            r#"{"service":"db","order_id":"force-1","order_try":1}]}"#
        )
    );
    assert_eq!(answer(&text).expect("read"), sent);
}

/// Catches: a new agent refusing the answer of a coordinator that predates `acks`: an answer
/// without the key reads with none.
#[test]
fn an_answer_without_acks_reads_with_none() {
    for text in [
        r#"{"reply":"nothing"}"#,
        r#"{"reply":"nothing","features":["verdict-acks-v1"]}"#,
        r#"{"reply":"nothing","features":["halt-v1"],"halts":{"kbf":1}}"#,
    ] {
        assert!(answer(text).expect("read").acks.is_empty(), "{text}");
    }
}

/// Catches: an ack the agent could misapply taken into the answer: one whose service is not a
/// service's name, whose order id is not one, whose try is not a number, with a key it does
/// not know or without one it needs, listed twice, or more than one per service a heartbeat
/// lists. Each refuses the whole answer, with the reason.
#[test]
fn a_bad_ack_is_refused() {
    let one = |body: &str| format!(r#"{{"reply":"nothing","acks":[{body}]}}"#);
    let good = r#"{"service":"web","order_id":"o-1","order_try":1}"#;
    assert_eq!(
        answer(&one(good)).expect("read").acks,
        [ack("web", "o-1", 1)]
    );
    for (body, why) in [
        (
            r#"{"service":"Web!","order_id":"o-1","order_try":1}"#,
            "an ack's service is not a service name",
        ),
        (
            r#"{"service":"web","order_id":"o 1","order_try":1}"#,
            "not an order id",
        ),
        (
            r#"{"service":"web","order_id":"o-1","order_try":"1"}"#,
            "invalid type",
        ),
        (
            r#"{"service":"web","order_id":"o-1","order_try":-1}"#,
            "invalid value",
        ),
        (
            r#"{"service":"web","order_id":"o-1","order_try":1,"x":1}"#,
            "unknown field",
        ),
        (r#"{"service":"web","order_id":"o-1"}"#, "missing field"),
        (&format!("{good},{good}"), "an ack is listed twice"),
    ] {
        let refused = answer(&one(body)).expect_err(body).to_string();
        assert!(refused.contains(why), "{body}: {refused}");
    }
    let most: Vec<String> = (0..MAX_ACKS)
        .map(|n| format!(r#"{{"service":"s{n}","order_id":"o-1","order_try":1}}"#))
        .collect();
    assert_eq!(
        answer(&one(&most.join(","))).expect("read").acks.len(),
        MAX_ACKS
    );
    let over = format!(
        r#"{},{{"service":"over","order_id":"o-1","order_try":1}}"#,
        most.join(",")
    );
    let refused = answer(&one(&over)).expect_err("too many").to_string();
    assert!(refused.contains("too many acks"), "{refused}");
}

/// Catches: `acks` sent to an agent that did not announce it: every reader that predates it
/// refuses the answer, so the coordinator must leave the key out for such an agent.
#[test]
fn the_readers_that_predate_acks_refuse_it() {
    let text = r#"{"reply":"nothing","acks":[{"service":"web","order_id":"o-1","order_try":1}]}"#;
    assert!(serde_json::from_str::<FrozenReply>(text).is_err());
    assert!(serde_json::from_str::<ServiceReply>(text).is_err());
    assert!(serde_json::from_str::<Reply>(text).is_err());
}

/// Catches: acks taken for part of the reply: an answer with acks equals the reply it
/// carries.
#[test]
fn acks_do_not_change_the_reply() {
    let acked = Answer {
        acks: vec![ack("web", "o-1", 1)],
        ..featured()
    };
    assert_eq!(acked, ServiceReply::Nothing);
    assert_eq!(acked, Reply::Nothing);
}
