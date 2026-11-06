//! The loop's [`Heartbeat`] as the heartbeat of the frozen JSON route of `hostkeeper-wire`
//! (`POST /v1/heartbeat`), and that route's answer as the loop's [`Reply`]: every conversion
//! between the two families of types, in one place, for [`crate::JsonMtlsLink`].
//!
//! **Out.** The heartbeat's top level is the legacy service's on that wire; the daemon has none,
//! so the top level says the daemon itself (its version, `healthy`, no update) and every
//! installed service is an entry of `services`, which is sent even when empty. The daemon names
//! itself in `agent` and announces its features in `agent_features`. Of a service, the name,
//! version, health, running update, verdict and deferral go out; the observer's detail and the
//! health detail have no field on this wire and do not. Of a verdict, the target, outcome,
//! phase, class, reason, order id and try go out; the forced flag and detail have no field. A
//! deferred verdict has no outcome on this wire: the service's `deferred` report says it (as
//! kbf's agent does), so its `last_update` is left out. A service name or a version outside the
//! wire's grammars cannot be sent, and the heartbeat is refused by name ([`Unsendable`]), never
//! sent without it.
//!
//! **In.** The advertised features the daemon knows, and the halt clear generations of the
//! services the daemon could run (a name outside its grammar names none). The `acks` are the
//! verdicts the coordinator stored, by service, order id and try, each kept when the daemon's
//! grammars read its service name and order id and dropped otherwise (as a halt of a name outside
//! the grammar is). An update is the loop's update order when the answer carries its terms
//! (the order id, try and package digest a coordinator sends to an agent that announced
//! `order-v1`) and the daemon's grammars read every word of it. Any other update (one with no
//! service, the legacy service's; one with no terms, from a coordinator that sends none; one
//! with a word the daemon cannot read) is not run, because nothing runs from a package whose
//! digest the order did not name: it is returned beside the reply ([`Mapped::update`]), with
//! why, for the link to log.

use std::collections::BTreeMap;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Class, Phase};
use hostkeeper_wire::Platform as WirePlatform;
use hostkeeper_wire::version::Version as WireVersion;
use hostkeeper_wire::wire::{
    self, Agent, AgentName, Answer, OrderId as WireOrderId, PackageDigest as WirePackageDigest,
    ServiceBeat, ServiceReply, UpdateResult, is_service_name,
};
use hostkeeper_wire::wire_report::{
    Class as WireClass, DeferredReport as WireDeferred, Phase as WirePhase, Token,
};

use crate::beat::{AGENT_NAME, Feature, Health, Heartbeat, Platform, ServiceFacts};
use crate::names::{OrderId, PackageDigest};
use crate::order::{
    Acknowledged, DeferredReport, Order, OrderRef, OrderResult, Outcome, Reply, Target, Verb,
};
use crate::ports::Unsendable;

/// The heartbeat `beat` on the JSON route.
pub(crate) fn heartbeat(beat: &Heartbeat) -> Result<wire::Heartbeat, Unsendable> {
    let agent_version = version(&beat.agent_version)?;
    let services = beat
        .services
        .iter()
        .map(service)
        .collect::<Result<Vec<ServiceBeat>, Unsendable>>()?;
    Ok(wire::Heartbeat {
        platform: platform(beat.platform),
        version: agent_version.clone(),
        health: wire::Health::Healthy,
        last_update: None,
        updating: None,
        services: Some(services),
        agent_features: beat
            .features
            .iter()
            .map(|f| f.as_str().to_owned())
            .collect(),
        step: None,
        commit: None,
        step_secs: BTreeMap::new(),
        promotion_pending: false,
        deferred: None,
        // `AGENT_NAME` is a name of the wire's grammar (`a_heartbeat_names_its_agent`).
        agent: AgentName::parse(AGENT_NAME).map(|name| Agent {
            name,
            version: agent_version,
        }),
        sent_unix: Some(beat.sent_unix),
        cert_not_after_unix: beat.renewal.cert_not_after_unix,
        last_renew_error: beat.renewal.last_renew_error.clone(),
    })
}

/// One installed service.
fn service(facts: &ServiceFacts) -> Result<ServiceBeat, Unsendable> {
    let name = facts.name.as_str();
    if !is_service_name(name) {
        return Err(Unsendable::ServiceName(name.to_owned()));
    }
    let last_update = match &facts.last_update {
        Some(result) => verdict(result)?,
        None => None,
    };
    let updating = facts.updating.as_ref().map(version).transpose()?;
    let deferred = match &facts.deferred {
        Some(report) => deferral(report)?,
        None => None,
    };
    Ok(ServiceBeat {
        name: name.to_owned(),
        version: version(&facts.version)?,
        health: health(facts.health),
        last_update,
        updating,
        step: None,
        commit: None,
        step_secs: BTreeMap::new(),
        promotion_pending: false,
        deferred,
    })
}

/// A verdict as `last_update`: none for a deferral, which `deferred` says.
fn verdict(result: &OrderResult) -> Result<Option<UpdateResult>, Unsendable> {
    let outcome = match result.outcome {
        Outcome::Succeeded => wire::Outcome::Succeeded,
        Outcome::Failed => wire::Outcome::Failed,
        Outcome::Deferred { .. } => return Ok(None),
    };
    Ok(Some(UpdateResult {
        target: version(&result.target)?,
        outcome,
        phase: result.phase.map(phase),
        class: result.class.map(class),
        // A reason is a token of the hook contract, the grammar of the wire's token.
        reason: result
            .reason
            .as_ref()
            .and_then(|r| Token::parse(r.as_str())),
        // A loop's order id is one of the wire's, whose grammar is wider.
        order_id: WireOrderId::parse(result.order.id.as_str()),
        order_try: Some(result.order.order_try),
    }))
}

/// A deferral while its retry time is ahead.
fn deferral(report: &DeferredReport) -> Result<Option<WireDeferred>, Unsendable> {
    let target = version(&report.target)?;
    Ok(
        Token::parse(report.reason.as_str()).map(|reason| WireDeferred {
            target,
            reason,
            retry_after_secs: report.retry_after_secs,
        }),
    )
}

/// A version in the wire's grammar, which is narrower than the daemon's.
fn version(version: &Version) -> Result<WireVersion, Unsendable> {
    WireVersion::parse(version.as_str())
        .map_err(|_| Unsendable::Version(version.as_str().to_owned()))
}

fn platform(platform: Platform) -> WirePlatform {
    match platform {
        Platform::LinuxX86_64 => WirePlatform::LinuxX86_64,
        Platform::LinuxArm64 => WirePlatform::LinuxArm64,
        Platform::DarwinArm64 => WirePlatform::DarwinArm64,
    }
}

fn health(health: Health) -> wire::Health {
    match health {
        Health::Healthy => wire::Health::Healthy,
        Health::Unhealthy => wire::Health::Unhealthy,
        Health::Absent => wire::Health::Absent,
    }
}

fn phase(phase: Phase) -> WirePhase {
    match phase {
        Phase::BeforeCommit => WirePhase::BeforeCommit,
        Phase::GateRelease => WirePhase::GateRelease,
        Phase::GateEnvironment => WirePhase::GateEnvironment,
        Phase::AfterCommit => WirePhase::AfterCommit,
        Phase::Halt => WirePhase::Halt,
    }
}

fn class(class: Class) -> WireClass {
    match class {
        Class::Release => WireClass::Release,
        Class::Environment => WireClass::Environment,
    }
}

/// An update the JSON route asked for that the loop is not given, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Update {
    /// The service; none for the coordinator's legacy service.
    pub service: Option<String>,
    pub version: WireVersion,
    pub why: &'static str,
}

/// The loop's reply of an answer, and the update of the answer the loop is not given.
pub(crate) struct Mapped {
    pub reply: Reply,
    pub update: Option<Update>,
}

/// The answer of the JSON route as the loop's reply.
pub(crate) fn reply(answer: Answer) -> Mapped {
    let Answer {
        reply,
        features,
        halts,
        order_id,
        order_try,
        package_digest,
        acks,
    } = answer;
    let features = features
        .unwrap_or_default()
        .iter()
        .filter_map(|name| Feature::parse(name))
        .collect();
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
        .filter_map(|ack| acknowledged(&ack.service, ack.order_id.as_str(), ack.order_try))
        .collect();
    let (order, update) = match reply {
        ServiceReply::Nothing => (None, None),
        ServiceReply::Update { service, version } => {
            let terms = order_id.zip(order_try).zip(package_digest);
            match (service.as_deref(), terms) {
                (None, _) => (
                    None,
                    Some(Update {
                        service,
                        version,
                        why: NO_SERVICE,
                    }),
                ),
                (Some(_), None) => (
                    None,
                    Some(Update {
                        service,
                        version,
                        why: NO_TERMS,
                    }),
                ),
                (Some(name), Some(((id, order_try), digest))) => {
                    match order(name, &version, &id, order_try, &digest) {
                        Some(order) => (Some(order), None),
                        None => (
                            None,
                            Some(Update {
                                service,
                                version,
                                why: NOT_READ,
                            }),
                        ),
                    }
                }
            }
        }
    };
    Mapped {
        reply: Reply {
            order,
            features,
            halt_generations,
            acknowledged,
        },
        update,
    }
}

/// The acknowledgement of the verdict of `service` for the order `id` and try, when the daemon's
/// grammars read both words; shared with the protobuf route.
pub(crate) fn acknowledged(service: &str, id: &str, order_try: u64) -> Option<Acknowledged> {
    Some(Acknowledged {
        service: ServiceName::new(service).ok()?,
        order: OrderRef {
            id: OrderId::new(id).ok()?,
            order_try,
        },
    })
}

/// Why an update names no service: it is the coordinator's legacy service.
pub(crate) const NO_SERVICE: &str = "it names no service";
/// Why an update without its terms is not run: nothing runs without the order's digest.
pub(crate) const NO_TERMS: &str = "it carries no order id, try or package digest";
/// Why an update whose words the daemon's grammars refuse is not run.
pub(crate) const NOT_READ: &str =
    "its service, version, order id or digest is not one the daemon reads";

/// The loop's update order of the wire's words, when the daemon's grammars read every one: an
/// order id, a service name and a version are narrower there than on the wire.
fn order(
    service: &str,
    version: &WireVersion,
    id: &WireOrderId,
    order_try: u64,
    digest: &WirePackageDigest,
) -> Option<Order> {
    let target = Target {
        service: ServiceName::new(service).ok()?,
        version: Version::new(version.as_str()).ok()?,
        digest: PackageDigest::new(digest.as_str()).ok()?,
        index_digest: None,
    };
    Some(Order {
        id: OrderId::new(id.as_str()).ok()?,
        order_try,
        verb: Verb::Update(target),
    })
}
