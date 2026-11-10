//! The tests of `proto_map`: each conversion between the loop's types and those of
//! `hostkeeper.agent.v1`, on fixed inputs.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Phase};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::{NOT_READ, NOT_RUN, heartbeat, reply};
use crate::beat::{AGENT_NAME, ANNOUNCED, Feature, Health, Heartbeat, Platform, ServiceFacts};
use crate::names::{OrderId, PackageDigest, Reason};
use crate::order::{
    Acknowledged, DeferredReport, Order, OrderRef, OrderResult, Outcome, Reply, Target, Verb,
};
use crate::ports::Unsendable;

fn name(text: &str) -> ServiceName {
    ServiceName::new(text).expect("a service name")
}

fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

fn digest() -> String {
    "0a".repeat(32)
}

fn facts(service: &str, at: &str) -> ServiceFacts {
    ServiceFacts {
        name: name(service),
        version: version(at),
        health: Health::Healthy,
        updating: None,
        last_update: None,
        deferred: None,
        health_detail: None,
        observed: None,
        record_try: None,
        installer: None,
        drift: None,
    }
}

fn beat(platform: Platform, services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform,
        agent_version: version("0.9.0"),
        features: ANNOUNCED.to_vec(),
        services,
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
    }
}

fn verdict(target: &str, outcome: Outcome) -> OrderResult {
    OrderResult {
        order: OrderRef {
            id: OrderId::new("o-9").expect("an order id"),
            order_try: 3,
        },
        target: version(target),
        outcome,
        phase: None,
        class: None,
        reason: None,
        forced: false,
        detail: None,
    }
}

fn sent(beat: &Heartbeat) -> pb::HeartbeatRequest {
    heartbeat(beat).expect("a request")
}

/// Catches the host's clock or certificate left out of the request, or put in another
/// field: they go out in `sent_unix`, `cert_not_after_unix` and `last_renew_error`, and the
/// two the enrollment loop did not give are left unset.
#[test]
fn the_host_clock_and_certificate_go_out_in_their_fields() {
    let mut full = beat(Platform::LinuxX86_64, Vec::new());
    full.sent_unix = 2_000_000_123;
    full.renewal = crate::Renewal {
        cert_not_after_unix: Some(2_000_600_000),
        last_renew_error: Some("renewal refused: host_excluded".to_owned()),
    };
    let request = sent(&full);
    assert_eq!(request.sent_unix, Some(2_000_000_123));
    assert_eq!(request.cert_not_after_unix, Some(2_000_600_000));
    assert_eq!(
        request.last_renew_error.as_deref(),
        Some("renewal refused: host_excluded")
    );
    let request = sent(&beat(Platform::LinuxX86_64, Vec::new()));
    assert_eq!(request.sent_unix, Some(0));
    assert_eq!(request.cert_not_after_unix, None);
    assert_eq!(request.last_renew_error, None);
}

/// Catches a trusted root that goes out in another field or with another reason, a root
/// that needs no attention sent as unknown (or unknown as one that needs none), and an age
/// sent for a root whose file is missing: each reason goes out as its own value with the age
/// the report gave, a report with no reason as `NONE`, and no report as no age and
/// `UNSPECIFIED`.
#[test]
fn the_trusted_root_goes_out_as_its_age_and_reason() {
    use crate::{RootReason, TrustedRootFacts};
    let cases = [
        (
            Some(TrustedRootFacts {
                age_secs: Some(91 * 86_400),
                reason: Some(RootReason::Stale),
            }),
            Some(91 * 86_400),
            pb::TrustedRootReason::Stale,
        ),
        (
            Some(TrustedRootFacts {
                age_secs: None,
                reason: Some(RootReason::Missing),
            }),
            None,
            pb::TrustedRootReason::Missing,
        ),
        (
            Some(TrustedRootFacts {
                age_secs: Some(5),
                reason: Some(RootReason::Unparseable),
            }),
            Some(5),
            pb::TrustedRootReason::Unparseable,
        ),
        (
            Some(TrustedRootFacts {
                age_secs: Some(0),
                reason: None,
            }),
            Some(0),
            pb::TrustedRootReason::None,
        ),
        (None, None, pb::TrustedRootReason::Unspecified),
    ];
    for (root, age, reason) in cases {
        let mut one = beat(Platform::LinuxX86_64, Vec::new());
        one.trusted_root = root;
        let request = sent(&one);
        assert_eq!(
            (request.trusted_root_age_secs, request.trusted_root_reason()),
            (age, reason),
            "{root:?}"
        );
    }
}

/// The order `o-1`, try 2, with the given verb.
fn order(verb: pb::order::Verb) -> pb::HeartbeatResponse {
    pb::HeartbeatResponse {
        order: Some(pb::Order {
            order_id: "o-1".to_owned(),
            r#try: 2,
            verb: Some(verb),
        }),
        ..pb::HeartbeatResponse::default()
    }
}

fn terms(service: &str, version: &str, digest: &str) -> pb::Update {
    pb::Update {
        target: None,
        service: service.to_owned(),
        version: version.to_owned(),
        package_digest: digest.to_owned(),
        index_digest: String::new(),
    }
}

/// Proves every fact the request has a field for goes out in it, and the ones it has none
/// for do not: the platform, the agent's name and version, the announced features by name,
/// and for a service its name, version, health, running update, verdict (target, outcome,
/// phase, class, reason, try, order id, forced, detail), deferral and the try its step record
/// holds (none for a service whose record holds none), but not the health detail or the
/// observer's detail.
#[test]
fn a_heartbeat_with_every_fact_maps_to_the_request_field_for_field() {
    let mut busy = facts("web", "1.0");
    busy.health = Health::Unhealthy;
    busy.updating = Some(version("2.1"));
    busy.health_detail = Some("probe refused".to_owned());
    busy.observed = Some(crate::beat::Observed {
        liveness: crate::beat::Liveness::Stopped,
        version: Some(version("1.0")),
    });
    busy.last_update = Some(OrderResult {
        phase: Some(Phase::GateRelease),
        class: Some(Class::Release),
        reason: Some(Reason::new("drain_bound").expect("a reason")),
        forced: true,
        detail: Some("the drain took too long".to_owned()),
        ..verdict("2.0", Outcome::Failed)
    });
    busy.deferred = Some(DeferredReport {
        target: version("2.2"),
        reason: Reason::new("busy").expect("a reason"),
        retry_after_secs: 90,
    });
    busy.record_try = Some(4);
    let request = sent(&beat(Platform::LinuxArm64, vec![busy, facts("api", "3.1")]));
    assert_eq!(
        request,
        pb::HeartbeatRequest {
            installers: Vec::new(),
            repositories: None,
            platform: pb::Platform::LinuxArm64 as i32,
            agent: Some(pb::AgentInfo {
                name: AGENT_NAME.to_owned(),
                version: "0.9.0".to_owned(),
            }),
            features: [
                "deferred-v1",
                "halt-v1",
                "heartbeat-v2",
                "forced-v1",
                "order-v1",
                "verdict-acks-v1",
                "grpc-v1",
                "install-v1",
                "abort-v1",
                "clear-record-v1",
                "target-v1"
            ]
            .map(str::to_owned)
            .to_vec(),
            services: vec![
                pb::ServiceBeat {
                    name: "web".to_owned(),
                    version: "1.0".to_owned(),
                    health: pb::Health::Unhealthy as i32,
                    last_update: Some(pb::UpdateResult {
                        target: "2.0".to_owned(),
                        outcome: pb::Outcome::Failed as i32,
                        phase: pb::FailurePhase::GateRelease as i32,
                        class: pb::FailureClass::Release as i32,
                        reason: "drain_bound".to_owned(),
                        order_try: Some(3),
                        order_id: Some("o-9".to_owned()),
                        forced: true,
                        detail: Some("the drain took too long".to_owned()),
                        ..pb::UpdateResult::default()
                    }),
                    updating: Some("2.1".to_owned()),
                    deferred: Some(pb::DeferredReport {
                        target: "2.2".to_owned(),
                        reason: "busy".to_owned(),
                        retry_after_secs: 90,
                    }),
                    record_try: Some(4),
                    ..pb::ServiceBeat::default()
                },
                pb::ServiceBeat {
                    name: "api".to_owned(),
                    version: "3.1".to_owned(),
                    health: pb::Health::Healthy as i32,
                    ..pb::ServiceBeat::default()
                },
            ],
            sent_unix: Some(0),
            cert_not_after_unix: None,
            last_renew_error: None,
            trusted_root_age_secs: None,
            trusted_root_reason: pb::TrustedRootReason::Unspecified as i32,
            facts_generation: None,
        }
    );
}

/// Proves a heartbeat of a host with no service still lists them: an empty list, with the
/// platform and agent.
#[test]
fn a_heartbeat_with_no_service_sends_an_empty_list() {
    let request = sent(&beat(Platform::DarwinArm64, Vec::new()));
    assert_eq!(request.services, Vec::<pb::ServiceBeat>::new());
    assert_eq!(request.platform, pb::Platform::DarwinArm64 as i32);
}

/// Proves each platform, health, phase, class and succeeded outcome has its own word, and a
/// failure that reports no phase, class or reason sends the unspecified ones and no reason.
#[test]
fn every_platform_health_phase_and_class_maps_to_its_own_word() {
    let platforms = [
        (Platform::LinuxX86_64, pb::Platform::LinuxX8664),
        (Platform::LinuxArm64, pb::Platform::LinuxArm64),
        (Platform::DarwinArm64, pb::Platform::DarwinArm64),
    ];
    for (platform, word) in platforms {
        assert_eq!(sent(&beat(platform, Vec::new())).platform, word as i32);
    }
    let healths = [
        (Health::Healthy, pb::Health::Healthy),
        (Health::Unhealthy, pb::Health::Unhealthy),
        (Health::Absent, pb::Health::Absent),
    ];
    for (health, word) in healths {
        let mut web = facts("web", "1.0");
        web.health = health;
        let request = sent(&beat(Platform::LinuxX86_64, vec![web]));
        assert_eq!(request.services[0].health, word as i32);
    }
    let phases = [
        (Phase::BeforeCommit, pb::FailurePhase::BeforeCommit),
        (Phase::GateRelease, pb::FailurePhase::GateRelease),
        (Phase::GateEnvironment, pb::FailurePhase::GateEnvironment),
        (Phase::AfterCommit, pb::FailurePhase::AfterCommit),
        (Phase::Halt, pb::FailurePhase::Halt),
    ];
    let classes = [
        (Class::Release, pb::FailureClass::Release),
        (Class::Environment, pb::FailureClass::Environment),
    ];
    let last = |result: OrderResult| {
        let mut web = facts("web", "1.0");
        web.last_update = Some(result);
        sent(&beat(Platform::LinuxX86_64, vec![web])).services[0]
            .last_update
            .clone()
            .expect("a verdict")
    };
    for (phase, word) in phases {
        let result = last(OrderResult {
            phase: Some(phase),
            ..verdict("2.0", Outcome::Failed)
        });
        assert_eq!(result.phase, word as i32);
    }
    for (class, word) in classes {
        let result = last(OrderResult {
            class: Some(class),
            ..verdict("2.0", Outcome::Failed)
        });
        assert_eq!(result.class, word as i32);
    }
    let bare = last(verdict("2.0", Outcome::Failed));
    assert_eq!(
        (bare.phase, bare.class, bare.reason.as_str(), bare.detail),
        (
            pb::FailurePhase::Unspecified as i32,
            pb::FailureClass::Unspecified as i32,
            "",
            None
        )
    );
    let done = last(verdict("2.0", Outcome::Succeeded));
    assert_eq!(done.outcome, pb::Outcome::Succeeded as i32);
    assert!(!done.forced);
}

/// Proves a deferral is reported by the service's `deferred` and not as a verdict: a
/// deferred outcome leaves `last_update` out.
#[test]
fn a_deferred_verdict_is_not_a_last_update() {
    let mut web = facts("web", "1.0");
    web.last_update = Some(verdict(
        "2.0",
        Outcome::Deferred {
            retry_after_secs: 30,
        },
    ));
    let request = sent(&beat(Platform::LinuxX86_64, vec![web]));
    assert_eq!(request.services[0].last_update, None);
}

/// Proves a name or a version outside the wire's grammar is refused by name wherever it is:
/// the service's name, the agent's version, a service's version, its running update, its
/// verdict's target and its deferral's target.
#[test]
fn a_fact_the_wire_has_no_word_for_is_refused_by_name() {
    let bad = Unsendable::Version("v2".to_owned());
    let mut running = facts("web", "1.0");
    running.updating = Some(version("v2"));
    let mut verdict_of = facts("web", "1.0");
    verdict_of.last_update = Some(verdict("v2", Outcome::Failed));
    let mut deferral = facts("web", "1.0");
    deferral.deferred = Some(DeferredReport {
        target: version("v2"),
        reason: Reason::new("busy").expect("a reason"),
        retry_after_secs: 1,
    });
    let mut agent = beat(Platform::LinuxX86_64, Vec::new());
    agent.agent_version = version("v2");
    for refused in [
        agent,
        beat(Platform::LinuxX86_64, vec![facts("web", "v2")]),
        beat(Platform::LinuxX86_64, vec![running]),
        beat(Platform::LinuxX86_64, vec![verdict_of]),
        beat(Platform::LinuxX86_64, vec![deferral]),
    ] {
        assert_eq!(heartbeat(&refused).err(), Some(bad.clone()));
    }
    assert_eq!(
        heartbeat(&beat(Platform::LinuxX86_64, vec![facts("Web_1", "1.0")])).err(),
        Some(Unsendable::ServiceName("Web_1".to_owned()))
    );
}

/// Proves a response with nothing in it is the loop's default reply, and nothing is dropped.
#[test]
fn an_empty_response_is_the_default_reply() {
    let mapped = reply(pb::HeartbeatResponse::default());
    assert_eq!(mapped.reply, Reply::default());
    assert_eq!(mapped.dropped, None);
}

/// Proves the features the daemon knows are kept in the coordinator's order and the ones it
/// does not are dropped, and that the halt generations of the services the daemon could run
/// come in the order of the names, a name outside the grammar naming none.
#[test]
fn a_response_keeps_the_known_features_and_the_halts_of_valid_names_in_order() {
    let mapped = reply(pb::HeartbeatResponse {
        features: ["halt-v1", "future-v9", "grpc-v1"]
            .map(str::to_owned)
            .to_vec(),
        halt_generations: [("web", 2), ("a/b", 9), ("api", 1), ("", 4)]
            .into_iter()
            .map(|(name, generation)| (name.to_owned(), generation))
            .collect(),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(mapped.reply.features, [Feature::HaltV1, Feature::GrpcV1]);
    assert_eq!(
        mapped.reply.halt_generations,
        [(name("api"), 1), (name("web"), 2)]
    );
    assert_eq!(mapped.reply.acknowledged, Vec::<Acknowledged>::new());
}

/// Proves an ack of a readable service and order id is the loop's, in the response's order,
/// and one whose service name or order id the daemon's grammars refuse is dropped.
#[test]
fn acks_are_kept_when_readable_and_dropped_otherwise() {
    let ack = |service: &str, order_id: &str, order_try: u64| pb::VerdictAck {
        service: service.to_owned(),
        order_id: order_id.to_owned(),
        order_try,
    };
    let mapped = reply(pb::HeartbeatResponse {
        acks: vec![
            ack("web", "o-1", 2),
            ack("a/b", "o-1", 1),
            ack("api", ".hidden", 1),
            ack("api", "force-3", 1),
        ],
        ..pb::HeartbeatResponse::default()
    });
    let expected = |service: &str, id: &str, order_try: u64| Acknowledged {
        service: name(service),
        order: OrderRef {
            id: OrderId::new(id).expect("an order id"),
            order_try,
        },
    };
    assert_eq!(
        mapped.reply.acknowledged,
        [expected("web", "o-1", 2), expected("api", "force-3", 1)]
    );
}

/// Proves an `update` order is the loop's update order with its id, try and terms, a `force`
/// order is the loop's forced one, an `install` its install with the same terms, and an
/// `uninstall`, an `abort` and a `clear_record` the loop's of the service they name.
#[test]
fn every_verb_the_loop_runs_is_its_order() {
    let target = Target {
        service: name("web"),
        version: version("2.0"),
        digest: PackageDigest::new(&digest()).expect("a digest"),
        index_digest: None,
        entry: None,
    };
    let updated = reply(order(pb::order::Verb::Update(terms(
        "web",
        "2.0",
        &digest(),
    ))));
    assert_eq!(
        updated.reply.order,
        Some(Order {
            id: OrderId::new("o-1").expect("an order id"),
            order_try: 2,
            verb: Verb::Update(target.clone()),
        })
    );
    assert_eq!(updated.dropped, None);
    let forced = reply(order(pb::order::Verb::Force(pb::Force {
        target: None,
        service: "web".to_owned(),
        version: "2.0".to_owned(),
        package_digest: digest(),
        index_digest: String::new(),
        halt_generation: 5,
    })));
    assert_eq!(
        forced.reply.order,
        Some(Order {
            id: OrderId::new("o-1").expect("an order id"),
            order_try: 2,
            verb: Verb::Force(target.clone()),
        })
    );
    assert_eq!(forced.dropped, None);
    let installed = reply(order(pb::order::Verb::Install(pb::Install {
        target: None,
        service: "web".to_owned(),
        version: "2.0".to_owned(),
        package_digest: digest(),
        index_digest: String::new(),
    })));
    let uninstalled = reply(order(pb::order::Verb::Uninstall(pb::Uninstall {
        service: "web".to_owned(),
    })));
    let aborted = reply(order(pb::order::Verb::Abort(pb::Abort {
        service: "web".to_owned(),
    })));
    let cleared = reply(order(pb::order::Verb::ClearRecord(pb::ClearRecord {
        service: "web".to_owned(),
    })));
    let verbs = [
        (installed, Verb::Install(target)),
        (uninstalled, Verb::Uninstall(name("web"))),
        (aborted, Verb::Abort(name("web"))),
        (cleared, Verb::ClearRecord(name("web"))),
    ];
    for (mapped, verb) in verbs {
        assert_eq!(
            mapped.reply.order,
            Some(Order {
                id: OrderId::new("o-1").expect("an order id"),
                order_try: 2,
                verb,
            })
        );
        assert_eq!(mapped.dropped, None);
    }
}

/// An index digest unlike the package digest, for the tests of the index digest.
fn index_digest() -> String {
    "cd".repeat(32)
}

/// Proves an `update`, a `force` and an `install` that carry an index digest give the loop
/// an order whose target holds it beside the package digest, and one that carries none (an
/// older coordinator) gives a target with no index digest.
#[test]
fn an_index_digest_of_an_order_is_kept_and_an_empty_one_is_none() {
    let wanted = PackageDigest::new(&index_digest()).expect("an index digest");
    let package = PackageDigest::new(&digest()).expect("a digest");
    let verbs = [
        reply(order(pb::order::Verb::Update(pb::Update {
            index_digest: index_digest(),
            ..terms("web", "2.0", &digest())
        }))),
        reply(order(pb::order::Verb::Force(pb::Force {
            target: None,
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            package_digest: digest(),
            halt_generation: 1,
            index_digest: index_digest(),
        }))),
        reply(order(pb::order::Verb::Install(pb::Install {
            target: None,
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            package_digest: digest(),
            index_digest: index_digest(),
        }))),
    ];
    for mapped in verbs {
        assert_eq!(mapped.dropped, None);
        let order = mapped.reply.order.expect("an order");
        let target = order.target().expect("a target");
        assert_eq!(target.index_digest.as_ref(), Some(&wanted));
        assert_eq!(target.digest, package);
    }
    let none = reply(order(pb::order::Verb::Update(terms(
        "web",
        "2.0",
        &digest(),
    ))));
    let order = none.reply.order.expect("an order");
    assert_eq!(order.target().expect("a target").index_digest, None);
}

/// Proves an index digest that is not 64 lower-case hex digits makes the order unreadable,
/// as a bad package digest does: not given to the loop, and dropped naming the verb.
#[test]
fn a_malformed_index_digest_drops_the_order() {
    for bad in [
        "not-a-digest",
        &index_digest().to_uppercase(),
        &"cd".repeat(31),
    ] {
        let verbs = [
            reply(order(pb::order::Verb::Update(pb::Update {
                index_digest: bad.to_owned(),
                ..terms("web", "2.0", &digest())
            }))),
            reply(order(pb::order::Verb::Install(pb::Install {
                target: None,
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: digest(),
                index_digest: bad.to_owned(),
            }))),
        ];
        for mapped in verbs {
            assert_eq!(mapped.reply.order, None, "{bad}");
            let dropped = mapped.dropped.expect("dropped");
            assert_eq!(dropped.why, NOT_READ);
        }
    }
}

/// Proves an order of a verb the daemon does not run, and one with no verb, is not given to
/// the loop and comes back with why and which.
#[test]
fn an_order_of_another_verb_or_none_is_dropped_with_why() {
    let rollback = reply(order(pb::order::Verb::ForceRollback(pb::ForceRollback {
        target: None,
        service: "web".to_owned(),
        version: "1.0".to_owned(),
        package_digest: digest(),
        index_digest: String::new(),
    })));
    let clear = reply(order(pb::order::Verb::ClearHalt(pb::ClearHalt {
        service: "web".to_owned(),
        halt_generation: 2,
    })));
    let mut none = order(pb::order::Verb::Abort(pb::Abort::default()));
    none.order.as_mut().expect("an order").verb = None;
    for mapped in [rollback, clear, reply(none)] {
        assert_eq!(mapped.reply.order, None);
        let dropped = mapped.dropped.expect("dropped");
        assert_eq!(dropped.what, "order \"o-1\"");
        assert_eq!(dropped.why, NOT_RUN);
    }
}

/// Proves an order of any verb the loop runs with a word the daemon's grammars refuse
/// (service, version, digest or order id) is not given to the loop and comes back naming the
/// verb.
#[test]
fn an_order_with_an_unreadable_word_is_dropped_with_why() {
    let cases = [
        terms("a/b", "2.0", &digest()),
        terms("web", "", &digest()),
        terms("web", "2.0", "not-a-digest"),
    ];
    for bad in cases {
        let mapped = reply(order(pb::order::Verb::Update(bad)));
        assert_eq!(mapped.reply.order, None);
        let dropped = mapped.dropped.expect("dropped");
        assert_eq!(dropped.what, "update order \"o-1\"");
        assert_eq!(dropped.why, NOT_READ);
    }
    let mut forced = order(pb::order::Verb::Force(pb::Force {
        target: None,
        service: "web".to_owned(),
        version: "2.0".to_owned(),
        package_digest: digest(),
        index_digest: String::new(),
        halt_generation: 1,
    }));
    forced.order.as_mut().expect("an order").order_id = ".hidden".to_owned();
    let mapped = reply(forced);
    assert_eq!(mapped.reply.order, None);
    let dropped = mapped.dropped.expect("dropped");
    assert_eq!(dropped.what, "force order \".hidden\"");
    assert_eq!(dropped.why, NOT_READ);
    let named = [
        (
            pb::order::Verb::Install(pb::Install {
                target: None,
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: "not-a-digest".to_owned(),
                index_digest: String::new(),
            }),
            "install order \"o-1\"",
        ),
        (
            pb::order::Verb::Uninstall(pb::Uninstall {
                service: "a/b".to_owned(),
            }),
            "uninstall order \"o-1\"",
        ),
        (
            pb::order::Verb::Abort(pb::Abort {
                service: String::new(),
            }),
            "abort order \"o-1\"",
        ),
        (
            pb::order::Verb::ClearRecord(pb::ClearRecord {
                service: "../web".to_owned(),
            }),
            "clear_record order \"o-1\"",
        ),
    ];
    for (verb, what) in named {
        let mapped = reply(order(verb));
        assert_eq!(mapped.reply.order, None, "{what}");
        let dropped = mapped.dropped.expect("dropped");
        assert_eq!(dropped.what, what);
        assert_eq!(dropped.why, NOT_READ);
    }
}

/// An entry with every field of the release message set, for the tests of the entry an order
/// carries.
fn entry() -> hostkeeper_proto::hostkeeper::release::v1::Target {
    use hostkeeper_proto::hostkeeper::release::v1 as release;
    release::Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: vec![release::Location {
            store: "lan".to_owned(),
            locator: "releases/web/2.0/web.tar".to_owned(),
        }],
        digest: digest(),
        bytes: 4096,
        attestation: Some(release::Blob {
            digest: index_digest(),
            bytes: 512,
            locations: vec![release::Location {
                store: "github".to_owned(),
                locator: "web/web.tar.sigstore.json".to_owned(),
            }],
        }),
        manifest: None,
        repository: None,
        package_version: None,
    }
}

/// Proves the `target` entry of an `update`, a `force` and an `install` reaches the loop's
/// order field for field, whatever the entry holds (the link judges nothing of it: an entry of
/// no installer and no locations is carried too), and that an order with no `target` carries
/// none.
///
/// Catches: an entry dropped, read from the wrong verb, altered on the way, or refused by the
/// link instead of left to the daemon.
#[test]
fn the_entry_of_an_update_a_force_and_an_install_is_carried_unchanged() {
    use hostkeeper_proto::hostkeeper::release::v1 as release;
    let carried = |mapped: super::Mapped| {
        assert_eq!(mapped.dropped, None);
        mapped
            .reply
            .order
            .expect("an order")
            .target()
            .expect("a target")
            .entry
            .clone()
    };
    for picked in [entry(), release::Target::default()] {
        let update = pb::order::Verb::Update(pb::Update {
            target: Some(picked.clone()),
            ..terms("web", "2.0", &digest())
        });
        let force = pb::order::Verb::Force(pb::Force {
            target: Some(picked.clone()),
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            package_digest: digest(),
            index_digest: String::new(),
            halt_generation: 0,
        });
        let install = pb::order::Verb::Install(pb::Install {
            target: Some(picked.clone()),
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            package_digest: digest(),
            index_digest: String::new(),
        });
        for verb in [update, force, install] {
            let entry = carried(reply(order(verb))).expect("an entry");
            assert_eq!(entry.as_target(), &picked);
            assert_eq!(entry.into_target(), picked);
        }
    }
    let bare = pb::order::Verb::Update(terms("web", "2.0", &digest()));
    assert_eq!(carried(reply(order(bare))), None);
}

/// Proves the installers of the heartbeat go out as `installers` of the request, in order, and
/// that a heartbeat with none sends none.
///
/// Catches: the installers left out of the request, or reordered.
#[test]
fn the_installers_go_out_in_the_request() {
    let mut full = beat(Platform::LinuxX86_64, Vec::new());
    full.installers = vec!["hostkeeper-archive".to_owned(), "tarball".to_owned()];
    assert_eq!(sent(&full).installers, ["hostkeeper-archive", "tarball"]);
    let none = beat(Platform::LinuxX86_64, Vec::new());
    assert_eq!(sent(&none).installers, Vec::<String>::new());
}

/// Proves a service's installer goes out as `installer` of its `ServiceBeat` on the gRPC route
/// when it has one and is left absent when it has none.
///
/// Catches: the installer dropped on the way to the message, or sent as an empty text for a
/// service that has none.
#[test]
fn a_services_installer_goes_out_as_installer_when_it_has_one() {
    let mut tarball = facts("web", "1.0");
    tarball.installer = Some("tarball".to_owned());
    let request = sent(&beat(
        Platform::LinuxX86_64,
        vec![tarball, facts("db", "1.0")],
    ));
    assert_eq!(request.services[0].installer.as_deref(), Some("tarball"));
    assert_eq!(request.services[1].installer, None);
}

/// Proves the frozen JSON route never carries a service's installer: its message has no field
/// for it, and no key of that name appears in the body.
///
/// Catches: the installer leaking into the JSON route's body.
#[test]
fn the_json_route_never_carries_a_services_installer() {
    let mut tarball = facts("web", "1.0");
    tarball.installer = Some("tarball".to_owned());
    let mapped = crate::wire_map::heartbeat(&beat(Platform::LinuxX86_64, vec![tarball]))
        .expect("a heartbeat");
    let body = serde_json::to_string(&mapped).expect("JSON");
    assert!(!body.contains("installer"), "{body}");
    assert!(!body.contains("tarball"), "{body}");
}

/// Proves the size measure counts a service's installer: the heartbeat that names one is longer
/// than the same heartbeat with none by the text, its quotes, less the `null` it replaces.
///
/// Catches: the installer left out of the bytes the bound measures.
#[test]
fn the_size_measure_counts_a_services_installer() {
    let plain = beat(Platform::LinuxX86_64, vec![facts("web", "1.0")]);
    let mut named = plain.clone();
    named.services[0].installer = Some("tarball".to_owned());
    assert_eq!(
        named.encoded_len() - plain.encoded_len(),
        "tarball".len() + 2 - "null".len()
    );
}

/// Proves a service's drift goes out as `drift` with its strings as they are, a drift with one
/// string absent keeps it absent (`found` absent: the package is not installed), a service with
/// no drift sends none, and a drift with a string outside the grammar of a package version (empty,
/// 129 bytes, a space) is sent as none while the heartbeat still goes out.
///
/// Catches: a drift left out of the beat, an absent string sent as an empty one, a bad version
/// sent to a coordinator that shows it, or a bad drift that refuses the beat.
#[test]
fn a_services_drift_goes_out_when_its_strings_are_package_versions() {
    use crate::Drift;
    let drifting = |found: Option<&str>, recorded: Option<&str>| {
        let mut one = facts("web", "1.0");
        one.drift = Some(Drift {
            found: found.map(str::to_owned),
            recorded: recorded.map(str::to_owned),
        });
        sent(&beat(Platform::LinuxX86_64, vec![one])).services[0]
            .drift
            .clone()
    };
    assert_eq!(
        drifting(Some("2:1.0~rc1-1"), Some("1.0-1")),
        Some(pb::DriftReport {
            found: Some("2:1.0~rc1-1".to_owned()),
            recorded: Some("1.0-1".to_owned()),
        })
    );
    assert_eq!(
        drifting(None, Some("1.0-1")),
        Some(pb::DriftReport {
            found: None,
            recorded: Some("1.0-1".to_owned()),
        })
    );
    let longest = "9".repeat(128);
    assert_eq!(
        drifting(Some(&longest), None),
        Some(pb::DriftReport {
            found: Some(longest.clone()),
            recorded: None,
        })
    );
    let too_long = "9".repeat(129);
    for (found, recorded) in [
        (Some(""), None),
        (Some(too_long.as_str()), None),
        (None, Some("1 0")),
        (Some("2.0"), Some("1/0")),
    ] {
        assert_eq!(drifting(found, recorded), None, "{found:?} {recorded:?}");
    }
    let plain = sent(&beat(Platform::LinuxX86_64, vec![facts("web", "1.0")]));
    assert_eq!(plain.services[0].drift, None);
}

/// Proves the repositories report goes out as `repositories` field for field, with its refusal
/// when it has one and without when it has none, and that a heartbeat with no report leaves the
/// field absent (not an empty report).
#[test]
fn the_repositories_report_goes_out_as_its_field() {
    use crate::repositories::{RepositoriesReport, RepositoryRefusal};
    let mut with = beat(Platform::LinuxX86_64, Vec::new());
    with.repositories = Some(RepositoriesReport {
        generation: 4,
        digest: "ab".repeat(32),
        refused: Some(RepositoryRefusal {
            generation: 5,
            digest: "cd".repeat(32),
            reason: "the keyring lacks the fingerprint".to_owned(),
        }),
    });
    assert_eq!(
        sent(&with).repositories,
        Some(pb::RepositoriesReport {
            generation: 4,
            digest: "ab".repeat(32),
            refused: Some(pb::RepositoryRefusal {
                generation: 5,
                digest: "cd".repeat(32),
                reason: "the keyring lacks the fingerprint".to_owned(),
            }),
        })
    );
    with.repositories = Some(RepositoriesReport::default());
    assert_eq!(
        sent(&with).repositories,
        Some(pb::RepositoriesReport::default())
    );
    with.repositories = None;
    assert_eq!(sent(&with).repositories, None);
}

/// Proves the set of a response comes in whole, as decoded, and a response with none has none.
#[test]
fn a_response_keeps_its_repository_set_as_decoded() {
    use crate::repositories::PushedSet;
    use hostkeeper_proto::hostkeeper::release::v1::Repository;
    let entries = vec![Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: "https://repo.example.com/debian".to_owned(),
        suites: vec!["stable".to_owned(), "testing".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec!["AB".repeat(20)],
        keyring: vec![0x99, 0, 1, 4],
        ..Default::default()
    }];
    let mapped = reply(pb::HeartbeatResponse {
        repositories: Some(pb::RepositorySet {
            generation: 9,
            digest: "ef".repeat(32),
            entries: entries.clone(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.repositories,
        Some(PushedSet {
            generation: 9,
            digest: "ef".repeat(32),
            entries,
        })
    );
    assert_eq!(mapped.dropped, None);
    assert_eq!(
        reply(pb::HeartbeatResponse::default()).reply.repositories,
        None
    );
}
