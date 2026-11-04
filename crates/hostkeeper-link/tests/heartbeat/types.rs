//! The types of the loop: what an order can hold, the words of a heartbeat, the errors, and the
//! flag.

use clap::Parser;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_link::testing::verdict;
use hostkeeper_link::{
    DeferredReport, Feature, Health, Heartbeat, LinkError, Liveness, LoopFlags, Observed, Order,
    OrderId, OrderRef, Outcome, Oversize, PackageDigest, Platform, Reason, ReplyFault,
    ServiceFacts, Target, Verb, WordError,
};

use crate::{digest, order_id, service, update, version};

/// The words no grammar of an order lets through: a path, a URL, a command line, and the empty
/// word.
const FREE: [&str; 6] = [
    "/a/b",
    "../up",
    "https://example.com/x",
    "run this now",
    "a;b",
    "",
];

/// Catches a field added to an order that can carry free text (a command, a path, a URL): the
/// destructuring below names every field and the type of each, so a new field fails to compile,
/// and each word type refuses what a command, a path or a URL needs.
#[test]
fn an_order_cannot_carry_a_command_path_or_url() {
    let order = update("o-1", 1, "x", "2.0");
    let Order {
        id,
        order_try,
        verb,
    } = order;
    let _: OrderId = id;
    let _: u64 = order_try;
    let target = match verb {
        Verb::Update(target) | Verb::Force(target) => target,
    };
    let Target {
        service: name,
        version: to,
        digest: package,
    } = target;
    let _: ServiceName = name;
    let _: Version = to;
    let _: PackageDigest = package;
    let OrderRef { id, order_try } = crate::reference("o-1", 1);
    let _: (OrderId, u64) = (id, order_try);

    for word in FREE {
        assert!(OrderId::new(word).is_err(), "{word:?}");
        assert!(PackageDigest::new(word).is_err(), "{word:?}");
        assert!(ServiceName::new(word).is_err(), "{word:?}");
        assert!(Version::new(word).is_err(), "{word:?}");
        assert!(Reason::new(word).is_err(), "{word:?}");
    }
    assert!(OrderId::new("a:b").is_err());
    assert!(OrderId::new(".").is_err());
    assert!(OrderId::new("..").is_err());
    assert!(OrderId::new(&"a".repeat(65)).is_err());
    assert_eq!(order_id(&"a".repeat(64)).as_str().len(), 64);
    assert_eq!(order_id("ord-1.2_x").as_str(), "ord-1.2_x");
    assert!(PackageDigest::new(&"0A".repeat(32)).is_err());
    assert!(PackageDigest::new(&"0a".repeat(31)).is_err());
    assert!(PackageDigest::new(&"0g".repeat(32)).is_err());
    assert_eq!(digest().as_str(), "0a".repeat(32));
    assert_eq!(
        Reason::new("drain_bound").expect("a reason").as_str(),
        "drain_bound"
    );
}

/// Catches a refusal that does not name the word refused or its grammar.
#[test]
fn each_word_refused_is_named_with_its_grammar() {
    assert_eq!(
        OrderId::new("a b").expect_err("a space").to_string(),
        "\"a b\" is not an order id: 1 to 64 bytes of A-Z, a-z, 0-9, `.`, `_` and `-`, not \
         beginning with `.`"
    );
    assert_eq!(
        Reason::new("Busy").expect_err("upper case"),
        WordError::Reason("Busy".to_owned())
    );
    assert_eq!(
        WordError::Reason("Busy".to_owned()).to_string(),
        "\"Busy\" is not a reason: 1 to 64 bytes of a-z, 0-9 and `_`"
    );
    assert_eq!(
        PackageDigest::new("abc").expect_err("short").to_string(),
        "\"abc\" is not a package digest: 64 lower-case hex digits"
    );
}

/// Catches a link error, a reply fault or an oversize that says something else than what
/// failed.
#[test]
fn each_link_error_says_what_failed() {
    let cases = [
        (
            LinkError::Transport(std::io::ErrorKind::TimedOut),
            "the coordinator could not be reached: timed out",
        ),
        (
            LinkError::Refused(Reason::new("forbidden").expect("a reason")),
            "the coordinator refused the heartbeat: forbidden",
        ),
        (
            LinkError::BadReply(ReplyFault::UnknownKey),
            "the coordinator's reply is refused: it holds a key the daemon does not know",
        ),
        (
            LinkError::BadReply(ReplyFault::OverBound),
            "the coordinator's reply is refused: it is longer than the reply bound",
        ),
        (
            LinkError::BadReply(ReplyFault::Malformed),
            "the coordinator's reply is refused: it is not a reply",
        ),
        (
            LinkError::TooLarge(Oversize::Services { count: 20 }),
            "the heartbeat is not sent: it lists 20 services, more than 16",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
    }
}

/// Catches a word of the heartbeat spelled other than the wire spells it, and a key of the
/// heartbeat's JSON renamed: the bound is measured on this text.
#[test]
fn the_heartbeat_json_has_the_wire_names() {
    let words = [
        (Platform::LinuxX86_64.as_str(), "linux-x86_64"),
        (Platform::LinuxArm64.as_str(), "linux-arm64"),
        (Platform::DarwinArm64.as_str(), "darwin-arm64"),
        (Health::Healthy.as_str(), "healthy"),
        (Health::Unhealthy.as_str(), "unhealthy"),
        (Health::Absent.as_str(), "absent"),
        (Liveness::Running.as_str(), "running"),
        (Liveness::Stopped.as_str(), "stopped"),
        (Liveness::Transitioning.as_str(), "transitioning"),
        (Liveness::Absent.as_str(), "absent"),
        (Liveness::Unobserved.as_str(), "unobserved"),
        (Liveness::Unmonitored.as_str(), "unmonitored"),
        (Outcome::Succeeded.as_str(), "succeeded"),
        (Outcome::Failed.as_str(), "failed"),
        (
            Outcome::Deferred {
                retry_after_secs: 1,
            }
            .as_str(),
            "deferred",
        ),
    ];
    for (word, wire) in words {
        assert_eq!(word, wire);
    }

    let mut result = verdict(&update("o-1", 3, "x", "2.0"), Outcome::Failed);
    result.phase = Some(hostkeeper_hook::Phase::GateRelease);
    result.class = Some(hostkeeper_hook::Class::Release);
    result.reason = Some(Reason::new("probe_failed").expect("a reason"));
    result.detail = Some("verify said no".to_owned());
    let beat = Heartbeat {
        platform: Platform::DarwinArm64,
        agent_version: version("0.9.0"),
        features: vec![Feature::DeferredV1],
        services: vec![ServiceFacts {
            name: service("x"),
            version: version("1.0"),
            health: Health::Absent,
            updating: Some(version("2.0")),
            last_update: Some(result),
            deferred: Some(DeferredReport {
                target: version("2.0"),
                reason: Reason::new("drain_bound").expect("a reason"),
                retry_after_secs: 5,
            }),
            health_detail: Some("refused".to_owned()),
            observed: Some(Observed {
                liveness: Liveness::Transitioning,
                version: None,
            }),
        }],
    };
    let expected = serde_json::json!({
        "platform": "darwin-arm64",
        "agent": { "name": "hostkeeper", "version": "0.9.0" },
        "agent_features": ["deferred-v1"],
        "services": [{
            "name": "x",
            "version": "1.0",
            "health": "absent",
            "updating": "2.0",
            "last_update": {
                "order_id": "o-1",
                "order_try": 3,
                "target": "2.0",
                "outcome": "failed",
                "retry_after_secs": null,
                "phase": "gate_release",
                "class": "release",
                "reason": "probe_failed",
                "forced": false,
                "detail": "verify said no",
            },
            "deferred": { "target": "2.0", "reason": "drain_bound", "retry_after_secs": 5 },
            "health_detail": "refused",
            "observed": { "liveness": "transitioning", "version": null },
        }],
    });
    assert_eq!(beat.to_json(), expected);
    assert_eq!(beat.encoded_len(), expected.to_string().len());
    let deferred = verdict(
        &update("o-1", 1, "x", "2.0"),
        Outcome::Deferred {
            retry_after_secs: 9,
        },
    );
    assert_eq!(deferred.to_json()["retry_after_secs"], 9);
}

#[derive(Debug, Parser)]
struct Daemon {
    #[command(flatten)]
    heartbeat: LoopFlags,
}

/// Catches a period other than 15 seconds by default, a flag spelled otherwise, and a period of
/// 0 (a loop that never waits) accepted.
#[test]
fn the_heartbeat_period_flag_defaults_to_15_seconds_and_refuses_0() {
    let parsed = Daemon::try_parse_from(["daemon"]).expect("no flag");
    assert_eq!(parsed.heartbeat.heartbeat_secs, 15);
    let parsed = Daemon::try_parse_from(["daemon", "--heartbeat-secs", "30"]).expect("30");
    assert_eq!(parsed.heartbeat, LoopFlags { heartbeat_secs: 30 });
    assert!(Daemon::try_parse_from(["daemon", "--heartbeat-secs", "0"]).is_err());
    assert!(Daemon::try_parse_from(["daemon", "--heartbeat-secs", "x"]).is_err());
}
