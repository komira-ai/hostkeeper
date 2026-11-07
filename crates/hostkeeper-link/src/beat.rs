//! The heartbeat: what the daemon says of the host and of every service installed on it, the
//! features it announces, and the bound a heartbeat keeps (design 11, items 3 and 10).
//!
//! **The bound.** A heartbeat lists at most [`MAX_SERVICES_PER_HEARTBEAT`] services, the one
//! limit of `hostkeeper-proto`, and is at most [`MAX_HEARTBEAT_BYTES`] as JSON with the wire's
//! names ([`Heartbeat::encoded_len`]). [`fit`] keeps it: a heartbeat with more services is
//! refused, never cut short; one with too many bytes loses its optional facts, a group at a
//! time in the order of [`DROP_ORDER`] (the health detail, then the deferral detail, then the
//! observer's detail), and is refused when that is not enough. The service, its version, its
//! health, the update running and the last verdict are never dropped, nor are the host's clock
//! and certificate facts ([`Heartbeat::sent_unix`], [`Heartbeat::renewal`]), whose one text is
//! at most [`hostkeeper_wire::wire::MAX_RENEW_ERROR`] bytes.

use hostkeeper_config::names::{ServiceName, Version};
pub use hostkeeper_proto::MAX_SERVICES_PER_HEARTBEAT;
use serde_json::{Map, Value, json};

use crate::order::{DeferredReport, OrderResult};

/// The most bytes of a heartbeat: its JSON on the JSON route, its encoded message on the gRPC one.
pub const MAX_HEARTBEAT_BYTES: usize = 16 * 1024;

/// The name the daemon gives itself in `agent`.
pub const AGENT_NAME: &str = "hostkeeper";

/// A feature of the coordinator's side that the daemon can use, by its wire name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// `deferred-v1`: report a deferral, and decline its target until the retry time.
    DeferredV1,
    /// `halt-v1`: report a halt as one, and keep the clear generations of the reply.
    HaltV1,
    /// `heartbeat-v2`: the heartbeat lists every service installed. The daemon always does, so
    /// nothing it does depends on the coordinator advertising it.
    HeartbeatV2,
    /// `forced-v1`: run the forced update.
    ForcedV1,
    /// `order-v1`: the JSON route's update answer carries the order's id, try and package
    /// digest, and the heartbeat's top level is the daemon itself, never a legacy service.
    OrderV1,
    /// `verdict-acks-v1`: the coordinator names the verdicts it has stored, by service, order
    /// id and try, and reads a service entry with no verdict as still reporting the last one
    /// it acknowledged.
    VerdictAcksV1,
    /// `grpc-v1`: the coordinator serves the heartbeat as gRPC over HTTP/2 on its heartbeat
    /// listener; the daemon uses it for the next heartbeat.
    GrpcV1,
    /// `install-v1`: the daemon runs the `install` and `uninstall` orders of the gRPC route
    /// ([`crate::Verb::Install`], [`crate::Verb::Uninstall`]). It says what the loop takes, not
    /// what the coordinator serves: the loop runs these orders whatever the reply advertises.
    InstallV1,
    /// `abort-v1`: the daemon runs the `abort` order of the gRPC route ([`crate::Verb::Abort`]),
    /// beside the order it stops. Like `install-v1`, it says what the loop takes.
    AbortV1,
}

/// The features every heartbeat announces, in this order. Each names something the loop does:
/// `install-v1` and `abort-v1` are here because [`crate::Heartbeater`] hands those orders to the
/// executor.
pub const ANNOUNCED: [Feature; 9] = [
    Feature::DeferredV1,
    Feature::HaltV1,
    Feature::HeartbeatV2,
    Feature::ForcedV1,
    Feature::OrderV1,
    Feature::VerdictAcksV1,
    Feature::GrpcV1,
    Feature::InstallV1,
    Feature::AbortV1,
];

impl Feature {
    /// Its wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Feature::DeferredV1 => "deferred-v1",
            Feature::HaltV1 => "halt-v1",
            Feature::HeartbeatV2 => "heartbeat-v2",
            Feature::ForcedV1 => "forced-v1",
            Feature::OrderV1 => hostkeeper_wire::wire::ORDER_FEATURE,
            Feature::VerdictAcksV1 => "verdict-acks-v1",
            Feature::GrpcV1 => "grpc-v1",
            Feature::InstallV1 => "install-v1",
            Feature::AbortV1 => "abort-v1",
        }
    }

    /// The feature of a wire name; none for a name the daemon does not know.
    pub fn parse(name: &str) -> Option<Feature> {
        ANNOUNCED
            .into_iter()
            .find(|feature| feature.as_str() == name)
    }
}

/// The features the coordinator's last reply advertised: none until a reply came, and none
/// from a coordinator that advertises nothing, which is then served the old shapes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Features(Vec<Feature>);

impl Features {
    pub fn of(advertised: &[Feature]) -> Features {
        Features(advertised.to_vec())
    }

    pub fn has(&self, feature: Feature) -> bool {
        self.0.contains(&feature)
    }
}

/// The platforms releases are built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    LinuxX86_64,
    LinuxArm64,
    DarwinArm64,
}

impl Platform {
    /// Its wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::DarwinArm64 => "darwin-arm64",
        }
    }
}

/// Whether a service serves what it should.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// It does not, or the observer cannot tell.
    Unhealthy,
    /// It is installed and has never run.
    Absent,
}

impl Health {
    /// Its wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
            Health::Absent => "absent",
        }
    }
}

/// The state of a service's units, as the service manager reports it (design 5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    Running,
    Stopped,
    Transitioning,
    Absent,
    /// The observation failed; never read as absent or stopped.
    Unobserved,
    /// The package names no units to run.
    Unmonitored,
}

impl Liveness {
    /// Its word in a heartbeat.
    pub const fn as_str(self) -> &'static str {
        match self {
            Liveness::Running => "running",
            Liveness::Stopped => "stopped",
            Liveness::Transitioning => "transitioning",
            Liveness::Absent => "absent",
            Liveness::Unobserved => "unobserved",
            Liveness::Unmonitored => "unmonitored",
        }
    }
}

/// What the observer saw beyond the health: the optional observer detail of a heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub liveness: Liveness,
    /// The version the observer confirmed running; none when it could not tell.
    pub version: Option<Version>,
}

impl Observed {
    /// The detail as JSON.
    pub fn to_json(&self) -> Value {
        json!({
            "liveness": self.liveness.as_str(),
            "version": self.version.as_ref().map(Version::as_str),
        })
    }
}

/// What the heartbeat says of one installed service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceFacts {
    pub name: ServiceName,
    /// The version the host runs, from the step record.
    pub version: Version,
    pub health: Health,
    /// The target of the update running now; none when none runs.
    pub updating: Option<Version>,
    /// The verdict not yet acknowledged.
    pub last_update: Option<OrderResult>,
    /// Optional: the deferral, while its retry time is ahead and the coordinator advertised
    /// `deferred-v1`.
    pub deferred: Option<DeferredReport>,
    /// Optional: why the health is what it is.
    pub health_detail: Option<String>,
    /// Optional: the observer's detail.
    pub observed: Option<Observed>,
    /// The try the service's step record holds, sent in every heartbeat; none when the service
    /// has no step record, or its record names no try.
    pub record_try: Option<u64>,
}

impl ServiceFacts {
    /// The facts as JSON, with the wire's names.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name.as_str(),
            "version": self.version.as_str(),
            "health": self.health.as_str(),
            "updating": self.updating.as_ref().map(Version::as_str),
            "last_update": self.last_update.as_ref().map(OrderResult::to_json),
            "deferred": self.deferred.as_ref().map(DeferredReport::to_json),
            "health_detail": self.health_detail,
            "observed": self.observed.as_ref().map(Observed::to_json),
            "record_try": self.record_try,
        })
    }
}

/// What the enrollment loop last said of the host's certificate, which every heartbeat reports
/// ([`crate::RenewalSource`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Renewal {
    /// The last second the host's current certificate is valid, in seconds since the epoch;
    /// none while the host holds no certificate.
    pub cert_not_after_unix: Option<u64>,
    /// Why the last step of enrollment or renewal failed, at most
    /// [`hostkeeper_wire::wire::MAX_RENEW_ERROR`] bytes; none once a step succeeds.
    pub last_renew_error: Option<String>,
}

/// One heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heartbeat {
    pub platform: Platform,
    /// The daemon's own version; its name is [`AGENT_NAME`].
    pub agent_version: Version,
    /// The features announced: [`ANNOUNCED`].
    pub features: Vec<Feature>,
    /// Every service installed on the host, one entry each.
    pub services: Vec<ServiceFacts>,
    /// The host's unix clock when the heartbeat was made, which the coordinator holds against
    /// its own.
    pub sent_unix: u64,
    /// The host's certificate, as the enrollment loop last said it.
    pub renewal: Renewal,
}

impl Heartbeat {
    /// The heartbeat as JSON, with the wire's names.
    pub fn to_json(&self) -> Value {
        let features: Vec<&str> = self.features.iter().map(|f| f.as_str()).collect();
        let services: Vec<Value> = self.services.iter().map(ServiceFacts::to_json).collect();
        let mut beat = Map::new();
        beat.insert("platform".to_owned(), self.platform.as_str().into());
        beat.insert(
            "agent".to_owned(),
            json!({ "name": AGENT_NAME, "version": self.agent_version.as_str() }),
        );
        beat.insert("agent_features".to_owned(), features.into());
        beat.insert("services".to_owned(), services.into());
        beat.insert("sent_unix".to_owned(), self.sent_unix.into());
        if let Some(not_after) = self.renewal.cert_not_after_unix {
            beat.insert("cert_not_after_unix".to_owned(), not_after.into());
        }
        if let Some(error) = &self.renewal.last_renew_error {
            beat.insert("last_renew_error".to_owned(), error.clone().into());
        }
        Value::Object(beat)
    }

    /// Its size: the bytes of [`Heartbeat::to_json`].
    pub fn encoded_len(&self) -> usize {
        self.to_json().to_string().len()
    }

    fn drop_fact(&mut self, fact: OptionalFact) {
        for service in &mut self.services {
            match fact {
                OptionalFact::HealthDetail => service.health_detail = None,
                OptionalFact::DeferralDetail => service.deferred = None,
                OptionalFact::ObserverDetail => service.observed = None,
            }
        }
    }
}

/// A group of optional facts a heartbeat loses, every service's at once, to fit its bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionalFact {
    /// [`ServiceFacts::health_detail`].
    HealthDetail,
    /// [`ServiceFacts::deferred`].
    DeferralDetail,
    /// [`ServiceFacts::observed`].
    ObserverDetail,
}

/// The order in which [`fit`] drops the optional facts.
pub const DROP_ORDER: [OptionalFact; 3] = [
    OptionalFact::HealthDetail,
    OptionalFact::DeferralDetail,
    OptionalFact::ObserverDetail,
];

/// Why a heartbeat is not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Oversize {
    #[error("it lists {count} services, more than {MAX_SERVICES_PER_HEARTBEAT}")]
    Services { count: usize },
    #[error("it is {len} bytes with every optional fact dropped, more than {MAX_HEARTBEAT_BYTES}")]
    Bytes { len: usize },
}

/// A heartbeat within its bound, and the groups of optional facts it lost to get there, in the
/// order they were dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fitted {
    pub beat: Heartbeat,
    pub dropped: Vec<OptionalFact>,
}

/// `beat` within its bound: as it is, or without its optional facts, dropped a group at a time
/// in the order of [`DROP_ORDER`] until it fits.
///
/// # Errors
/// More services than [`MAX_SERVICES_PER_HEARTBEAT`], or more than [`MAX_HEARTBEAT_BYTES`] with
/// every optional fact dropped.
pub fn fit(mut beat: Heartbeat) -> Result<Fitted, Oversize> {
    let count = beat.services.len();
    if count > MAX_SERVICES_PER_HEARTBEAT {
        return Err(Oversize::Services { count });
    }
    let mut dropped = Vec::new();
    for fact in DROP_ORDER {
        if beat.encoded_len() <= MAX_HEARTBEAT_BYTES {
            return Ok(Fitted { beat, dropped });
        }
        beat.drop_fact(fact);
        dropped.push(fact);
    }
    let len = beat.encoded_len();
    if len <= MAX_HEARTBEAT_BYTES {
        Ok(Fitted { beat, dropped })
    } else {
        Err(Oversize::Bytes { len })
    }
}
