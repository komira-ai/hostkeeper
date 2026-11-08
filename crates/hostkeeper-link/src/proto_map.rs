//! The loop's [`Heartbeat`] as the `HeartbeatRequest` of `hostkeeper.agent.v1`, and the
//! `HeartbeatResponse` as the loop's [`Reply`]: every conversion between the two families of
//! types, in one place, for [`crate::GrpcMtlsLink`]. It keeps the semantics of `wire_map`, which
//! does the same for the JSON route.
//!
//! **Out.** The request names the platform and the agent (`hostkeeper` and its version),
//! announces the features, and lists every installed service, which is sent even when there is
//! none. Of a service, the name, version, health, running update, verdict and deferral go out;
//! the observer's detail and the health detail have no field and do not. Of a verdict, the
//! target, outcome, phase, class, reason, order id and try go out, and so do the forced flag and
//! the detail, which this message has fields for. A deferred verdict has no outcome here: the
//! service's `deferred` report says it, so its `last_update` is left out. A service name or a
//! version outside the wire's grammars cannot be sent, and the heartbeat is refused by name
//! ([`Unsendable`]), never sent without it.
//!
//! **In.** The advertised features the daemon knows, and the halt clear generations of the
//! services the daemon could run (a name outside its grammar names none), in the order of the
//! service names. The `acks` are the verdicts the coordinator stored, each kept when the
//! daemon's grammars read its service name and order id and dropped otherwise. An order is the loop's
//! when its verb is `update`, `force`, `install`, `uninstall`, `abort` or `clear_record` and the
//! daemon's grammars read every word of it. Any other order (a verb the daemon does not run, no verb, or a word the
//! daemon cannot read) is not run: it is returned beside the reply ([`Mapped::dropped`]), with
//! why, for the link to log.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Phase};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_wire::version::Version as WireVersion;
use hostkeeper_wire::wire::is_service_name;

use crate::beat::{AGENT_NAME, Feature, Health, Heartbeat, Platform, ServiceFacts};
use crate::names::{OrderId, PackageDigest};
use crate::order::{DeferredReport, Order, OrderResult, Outcome, Reply, Target, Verb};
use crate::ports::Unsendable;
use crate::wire_map::acknowledged;

/// Why an order whose verb the daemon does not run, or that has none, is not run.
pub(crate) const NOT_RUN: &str =
    "the daemon runs only update, force, install, uninstall, abort and clear_record orders";
/// Why an order whose words the daemon's grammars refuse is not run.
pub(crate) const NOT_READ: &str =
    "its service, version, order id or digest is not one the daemon reads";

/// The heartbeat `beat` as the protobuf request.
pub(crate) fn heartbeat(beat: &Heartbeat) -> Result<pb::HeartbeatRequest, Unsendable> {
    let agent_version = version(&beat.agent_version)?;
    let services = beat
        .services
        .iter()
        .map(service)
        .collect::<Result<Vec<pb::ServiceBeat>, Unsendable>>()?;
    Ok(pb::HeartbeatRequest {
        platform: platform(beat.platform).into(),
        agent: Some(pb::AgentInfo {
            name: AGENT_NAME.to_owned(),
            version: agent_version,
        }),
        features: beat
            .features
            .iter()
            .map(|f| f.as_str().to_owned())
            .collect(),
        services,
        sent_unix: Some(beat.sent_unix),
        cert_not_after_unix: beat.renewal.cert_not_after_unix,
        last_renew_error: beat.renewal.last_renew_error.clone(),
    })
}

/// One installed service.
fn service(facts: &ServiceFacts) -> Result<pb::ServiceBeat, Unsendable> {
    let name = facts.name.as_str();
    if !is_service_name(name) {
        return Err(Unsendable::ServiceName(name.to_owned()));
    }
    let last_update = match &facts.last_update {
        Some(result) => verdict(result)?,
        None => None,
    };
    let deferred = facts.deferred.as_ref().map(deferral).transpose()?;
    Ok(pb::ServiceBeat {
        name: name.to_owned(),
        version: version(&facts.version)?,
        health: health(facts.health).into(),
        last_update,
        updating: facts.updating.as_ref().map(version).transpose()?,
        deferred,
        record_try: facts.record_try,
        ..pb::ServiceBeat::default()
    })
}

/// A verdict as `last_update`: none for a deferral, which `deferred` says.
fn verdict(result: &OrderResult) -> Result<Option<pb::UpdateResult>, Unsendable> {
    let outcome = match result.outcome {
        Outcome::Succeeded => pb::Outcome::Succeeded,
        Outcome::Failed => pb::Outcome::Failed,
        Outcome::Deferred { .. } => return Ok(None),
    };
    Ok(Some(pb::UpdateResult {
        target: version(&result.target)?,
        outcome: outcome.into(),
        phase: result
            .phase
            .map_or(pb::FailurePhase::Unspecified, phase)
            .into(),
        class: result
            .class
            .map_or(pb::FailureClass::Unspecified, class)
            .into(),
        reason: result
            .reason
            .as_ref()
            .map(|reason| reason.as_str().to_owned())
            .unwrap_or_default(),
        order_try: Some(result.order.order_try),
        order_id: Some(result.order.id.as_str().to_owned()),
        forced: result.forced,
        detail: result.detail.clone(),
        ..pb::UpdateResult::default()
    }))
}

/// A deferral while its retry time is ahead.
fn deferral(report: &DeferredReport) -> Result<pb::DeferredReport, Unsendable> {
    Ok(pb::DeferredReport {
        target: version(&report.target)?,
        reason: report.reason.as_str().to_owned(),
        retry_after_secs: report.retry_after_secs,
    })
}

/// A version in the wire's grammar, which is narrower than the daemon's.
fn version(version: &Version) -> Result<String, Unsendable> {
    WireVersion::parse(version.as_str())
        .map(|_| version.as_str().to_owned())
        .map_err(|_| Unsendable::Version(version.as_str().to_owned()))
}

fn platform(platform: Platform) -> pb::Platform {
    match platform {
        Platform::LinuxX86_64 => pb::Platform::LinuxX8664,
        Platform::LinuxArm64 => pb::Platform::LinuxArm64,
        Platform::DarwinArm64 => pb::Platform::DarwinArm64,
    }
}

fn health(health: Health) -> pb::Health {
    match health {
        Health::Healthy => pb::Health::Healthy,
        Health::Unhealthy => pb::Health::Unhealthy,
        Health::Absent => pb::Health::Absent,
    }
}

fn phase(phase: Phase) -> pb::FailurePhase {
    match phase {
        Phase::BeforeCommit => pb::FailurePhase::BeforeCommit,
        Phase::GateRelease => pb::FailurePhase::GateRelease,
        Phase::GateEnvironment => pb::FailurePhase::GateEnvironment,
        Phase::AfterCommit => pb::FailurePhase::AfterCommit,
        Phase::Halt => pb::FailurePhase::Halt,
    }
}

fn class(class: Class) -> pb::FailureClass {
    match class {
        Class::Release => pb::FailureClass::Release,
        Class::Environment => pb::FailureClass::Environment,
    }
}

/// An order of the response that the loop is not given, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dropped {
    /// What it is, for the log: the verb and the order id.
    pub what: String,
    pub why: &'static str,
}

/// The loop's reply of a response, and the order of the response the loop is not given.
pub(crate) struct Mapped {
    pub reply: Reply,
    pub dropped: Option<Dropped>,
}

/// The response as the loop's reply.
pub(crate) fn reply(response: pb::HeartbeatResponse) -> Mapped {
    let pb::HeartbeatResponse {
        order,
        features,
        halt_generations,
        acks,
    } = response;
    let features = features
        .iter()
        .filter_map(|name| Feature::parse(name))
        .collect();
    let mut halts: Vec<(String, u64)> = halt_generations.into_iter().collect();
    halts.sort();
    let halt_generations = halts
        .into_iter()
        .filter_map(|(name, generation)| {
            ServiceName::new(&name)
                .ok()
                .map(|service| (service, generation))
        })
        .collect();
    let acknowledged = acks
        .iter()
        .filter_map(|ack| acknowledged(&ack.service, &ack.order_id, ack.order_try))
        .collect();
    let (order, dropped) = match order {
        Some(order) => order_of(order),
        None => (None, None),
    };
    Mapped {
        reply: Reply {
            order,
            features,
            halt_generations,
            acknowledged,
        },
        dropped,
    }
}

/// The words of a protobuf order that names a package: service, version, package digest and
/// index digest (empty: none).
struct TargetTerms {
    service: String,
    version: String,
    digest: String,
    index_digest: String,
}

/// What a protobuf verb names: a version and package, or the service alone.
enum Terms {
    Target(fn(Target) -> Verb, TargetTerms),
    Service(fn(ServiceName) -> Verb, String),
}

/// The loop's order of the protobuf order, or why it is not run.
fn order_of(order: pb::Order) -> (Option<Order>, Option<Dropped>) {
    let pb::Order {
        order_id,
        r#try: order_try,
        verb,
    } = order;
    let (name, terms) = match verb {
        Some(pb::order::Verb::Update(a)) => (
            "update",
            Terms::Target(
                Verb::Update,
                TargetTerms {
                    service: a.service,
                    version: a.version,
                    digest: a.package_digest,
                    index_digest: a.index_digest,
                },
            ),
        ),
        Some(pb::order::Verb::Force(a)) => (
            "force",
            Terms::Target(
                Verb::Force,
                TargetTerms {
                    service: a.service,
                    version: a.version,
                    digest: a.package_digest,
                    index_digest: a.index_digest,
                },
            ),
        ),
        Some(pb::order::Verb::Install(a)) => (
            "install",
            Terms::Target(
                Verb::Install,
                TargetTerms {
                    service: a.service,
                    version: a.version,
                    digest: a.package_digest,
                    index_digest: a.index_digest,
                },
            ),
        ),
        Some(pb::order::Verb::Uninstall(a)) => {
            ("uninstall", Terms::Service(Verb::Uninstall, a.service))
        }
        Some(pb::order::Verb::Abort(a)) => ("abort", Terms::Service(Verb::Abort, a.service)),
        Some(pb::order::Verb::ClearRecord(a)) => {
            ("clear_record", Terms::Service(Verb::ClearRecord, a.service))
        }
        _ => {
            let dropped = Dropped {
                what: format!("order {order_id:?}"),
                why: NOT_RUN,
            };
            return (None, Some(dropped));
        }
    };
    let verb = match terms {
        Terms::Target(verb, terms) => target(&terms).map(verb),
        Terms::Service(verb, service) => ServiceName::new(&service).ok().map(verb),
    };
    match (verb, OrderId::new(&order_id)) {
        (Some(verb), Ok(id)) => {
            let order = Order {
                id,
                order_try,
                verb,
            };
            (Some(order), None)
        }
        _ => {
            let dropped = Dropped {
                what: format!("{name} order {order_id:?}"),
                why: NOT_READ,
            };
            (None, Some(dropped))
        }
    }
}

/// What an order acts on, when the daemon's grammars read the service, version and digests. An
/// empty index digest is none; a non-empty one the grammar refuses makes the order unreadable.
fn target(terms: &TargetTerms) -> Option<Target> {
    let index_digest = if terms.index_digest.is_empty() {
        None
    } else {
        Some(PackageDigest::new(&terms.index_digest).ok()?)
    };
    Some(Target {
        service: ServiceName::new(&terms.service).ok()?,
        version: Version::new(&terms.version).ok()?,
        digest: PackageDigest::new(&terms.digest).ok()?,
        index_digest,
    })
}

#[cfg(test)]
mod tests {
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
                    "clear-record-v1"
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
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: digest(),
                halt_generation: 1,
                index_digest: index_digest(),
            }))),
            reply(order(pb::order::Verb::Install(pb::Install {
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
}
