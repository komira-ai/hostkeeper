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
//! ([`Unsendable`]), never sent without it. The daemon's trusted root goes out as its age and
//! reason (`TRUSTED_ROOT_REASON_NONE` for a root that needs no attention); a heartbeat with no
//! report of the root leaves the age out and the reason unspecified, which the coordinator reads
//! as unknown. The JSON route has no field for it.
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

use crate::beat::{AGENT_NAME, Feature, Health, Heartbeat, Platform, RootReason, ServiceFacts};
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
        installers: Vec::new(),
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
        trusted_root_age_secs: beat.trusted_root.and_then(|root| root.age_secs),
        trusted_root_reason: beat
            .trusted_root
            .map_or(pb::TrustedRootReason::Unspecified, |root| {
                root.reason.map_or(pb::TrustedRootReason::None, root_reason)
            })
            .into(),
    })
}

/// A trusted root's reason.
fn root_reason(reason: RootReason) -> pb::TrustedRootReason {
    match reason {
        RootReason::Stale => pb::TrustedRootReason::Stale,
        RootReason::Missing => pb::TrustedRootReason::Missing,
        RootReason::Unparseable => pb::TrustedRootReason::Unparseable,
    }
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
#[path = "proto_map_tests.rs"]
mod tests;
