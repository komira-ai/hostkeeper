//! The optional facts a heartbeat carries since host updates (`docs/design/host-updates.md`,
//! sections 3.7 and 3.8), and the answer that advertises what a coordinator supports.
//!
//! Every type here is read strictly (a bad value is an error) and every field that uses one is
//! optional on the wire, so [`crate::wire::read_heartbeat`] can drop a field that is bad and
//! keep the host's legacy-service updates going. A coordinator that predates them ignores them.
//!
//! [`Class`], the reason token ([`Token`], at most [`MAX_TOKEN`] bytes of `[a-z0-9_]`) and the
//! two outcomes the hook contract names are the shapes of the hook contract's step report
//! (`class`, `reason`, `retry_after_secs`, `promotion_pending`); this module mirrors them and
//! does not depend on the crate that defines them: the hook contract has no product
//! dependency, and the wire's home is here.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::version::Version;
use crate::wire::{MAX_SERVICES, ServiceReply, is_service_name};

/// The most bytes of a [`Token`].
pub const MAX_TOKEN: usize = 64;

/// The most feature names an agent announces, and a reply names.
pub const MAX_FEATURES: usize = 8;

/// The most bytes of a feature name.
pub const MAX_FEATURE_NAME: usize = 32;

/// The most steps whose duration a beat reports.
pub const MAX_STEP_SECS: usize = 8;

/// The most bytes of a [`StepName`].
pub const MAX_STEP_NAME: usize = 16;

/// A machine-readable word: lowercase letters, digits and `_`, 1 to [`MAX_TOKEN`] bytes
/// (`drain_bound`, `cutover`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl Token {
    /// `text` as a token, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        is_word(text, MAX_TOKEN).then(|| Self(text.to_owned()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Token {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text).ok_or("not a token: [a-z0-9_], 1 to 64 bytes")
    }
}

impl From<Token> for String {
    fn from(token: Token) -> Self {
        token.0
    }
}

/// The most bytes of an [`OrderId`].
pub const MAX_ORDER_ID: usize = 64;

/// The id of the order a verdict ends, as the coordinator named it: 1 to [`MAX_ORDER_ID`] bytes
/// of ASCII letters, digits, `.`, `_` and `-`. The coordinator counts a verdict once per host,
/// order id and try ([`crate::wire::UpdateResult::order_try`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OrderId(String);

impl OrderId {
    /// `text` as an order id, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let ok = (1..=MAX_ORDER_ID).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        ok.then(|| Self(text.to_owned()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OrderId {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text).ok_or("not an order id: [A-Za-z0-9._-], 1 to 64 bytes")
    }
}

impl From<OrderId> for String {
    fn from(id: OrderId) -> Self {
        id.0
    }
}

/// Whether `text` is 1 to `max` bytes of lowercase letters, digits and `_`.
fn is_word(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The name of an update step (`prepare`, `cutover`): a [`Token`] of at most
/// [`MAX_STEP_NAME`] bytes, so a beat that reports every step stays small.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StepName(String);

impl StepName {
    /// `text` as a step name, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        is_word(text, MAX_STEP_NAME).then(|| Self(text.to_owned()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for StepName {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text).ok_or("not a step name: [a-z0-9_], 1 to 16 bytes")
    }
}

impl From<StepName> for String {
    fn from(name: StepName) -> Self {
        name.0
    }
}

/// Whom a failure is blamed on; it decides whether the fleet rolls back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// The new version is wrong.
    Release,
    /// Something remote or shared failed; the release may be fine.
    Environment,
}

/// Where in the update a failure happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Before the hook's commit point: nothing irreversible happened.
    BeforeCommit,
    /// `verify` failed before the commit; the hook's class is `release`.
    GateRelease,
    /// `verify` failed before the commit; the hook's class is `environment`.
    GateEnvironment,
    /// After the commit, any other failure.
    AfterCommit,
    /// A check whose failure no version can repair.
    Halt,
}

/// Whether the update in flight has passed the hook's commit point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    /// It can still be undone.
    Uncommitted,
    /// It cannot.
    Committed,
}

/// The step an update is in, and which try of it this is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRun {
    /// The step's name.
    pub name: StepName,
    /// The try, from 1.
    pub attempt: u32,
}

/// An update the host declined for a reason about its moment, not the release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredReport {
    /// The version it did not take.
    pub target: Version,
    /// Why.
    pub reason: Token,
    /// When the coordinator may order it again.
    pub retry_after_secs: u64,
}

/// A field that is read as `None` when its value is not one of `T`: for a fact a newer agent
/// may spell in a way this reader does not know.
pub(crate) fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(value).ok())
}

/// A field that is the default of `T` when it is absent or `null`.
pub(crate) fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Whether `name` is a feature name: lowercase letters, digits and `-`, 1 to
/// [`MAX_FEATURE_NAME`] bytes.
#[must_use]
pub fn is_feature_name(name: &str) -> bool {
    (1..=MAX_FEATURE_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A list of feature names: at most [`MAX_FEATURES`], each a name, none twice.
pub(crate) fn features_text<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    use serde::de::Error;
    let names = or_default::<_, Vec<String>>(d)?;
    check_features(&names).map_err(D::Error::custom)?;
    Ok(names)
}

fn check_features(names: &[String]) -> Result<(), &'static str> {
    if names.len() > MAX_FEATURES {
        return Err("too many features");
    }
    for (i, name) in names.iter().enumerate() {
        if !is_feature_name(name) {
            return Err("not a feature name");
        }
        if names[..i].contains(name) {
            return Err("a feature is listed twice");
        }
    }
    Ok(())
}

/// The clear generations of an answer: at most [`MAX_SERVICES`] and the legacy service, each
/// named as a service: the services of a full services file.
fn check_halts(halts: &BTreeMap<String, u64>) -> Result<(), &'static str> {
    if halts.len() > MAX_SERVICES + 1 {
        return Err("too many services in halts");
    }
    if !halts.keys().all(|name| is_service_name(name)) {
        return Err("a halts key is not a service name");
    }
    Ok(())
}

/// The acks of an answer: at most [`MAX_ACKS`], each of a service name, none twice.
fn check_acks(acks: &[VerdictAck]) -> Result<(), &'static str> {
    if acks.len() > MAX_ACKS {
        return Err("too many acks");
    }
    for (i, ack) in acks.iter().enumerate() {
        if !is_service_name(&ack.service) {
            return Err("an ack's service is not a service name");
        }
        if acks[..i].contains(ack) {
            return Err("an ack is listed twice");
        }
    }
    Ok(())
}

/// The step durations of a beat: at most [`MAX_STEP_SECS`] steps.
pub(crate) fn step_secs_text<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<StepName, u64>, D::Error> {
    use serde::de::Error;
    let secs = or_default::<_, BTreeMap<StepName, u64>>(d)?;
    if secs.len() > MAX_STEP_SECS {
        return Err(D::Error::custom("too many steps"));
    }
    Ok(secs)
}

/// Whether `flag` is false, for `skip_serializing_if`.
pub(crate) fn is_false(flag: &bool) -> bool {
    !*flag
}

/// The coordinator's answer to a heartbeat: the reply, and, to an agent that announced
/// `agent_features`, the features the coordinator supports.
///
/// Written flat: the reply's keys, then `features` when there is one, then `halts` and `acks`
/// when there are any. An agent that never announced features reads only
/// [`Reply`](crate::wire::Reply) or [`ServiceReply`], both of which refuse an unknown field,
/// so `features` is `None` and `halts` and `acks` empty for it and the bytes are those of the
/// reply alone. Equality with a reply compares the reply and ignores the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Answer {
    /// What to do.
    #[serde(flatten)]
    pub reply: ServiceReply,
    /// The supported features, only for an agent that announced `agent_features`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<String>>,
    /// How many times an operator has cleared a halt of each service, by service name; a
    /// service never cleared is not listed. Only for an agent that announced `halt-v1`: it
    /// lifts its hold on a service when the number is newer than the one it halted under, so
    /// a clear is not lost with a heartbeat and a coordinator restart does not forget it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub halts: BTreeMap<String, u64>,
    /// The verdicts of the heartbeat that the coordinator has stored: the agent sends none of
    /// them again. Only for an agent that announced [`VERDICT_ACKS_FEATURE`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acks: Vec<VerdictAck>,
}

impl Answer {
    /// `reply` with no features.
    #[must_use]
    pub fn of(reply: ServiceReply) -> Self {
        Self {
            reply,
            features: None,
            halts: BTreeMap::new(),
            acks: Vec::new(),
        }
    }
}

/// The feature an agent announces to be acknowledged its verdicts ([`Answer::acks`]); one that
/// does not is answered as before, and resends each verdict until it reports another.
pub const VERDICT_ACKS_FEATURE: &str = "verdict-acks-v1";

/// The most acks of an answer: one verdict of each service a heartbeat lists, at most
/// [`MAX_SERVICES`], and the legacy service's.
pub const MAX_ACKS: usize = MAX_SERVICES + 1;

/// A verdict the coordinator acknowledges: the service it is about, and the order id and try
/// it ends ([`crate::wire::UpdateResult::order_id`], [`crate::wire::UpdateResult::order_try`]).
/// The service is named because an order id is the coordinator's per service: two services
/// may each have a `force-1`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictAck {
    /// The service the verdict is about.
    pub service: String,
    /// The order the verdict ends.
    pub order_id: OrderId,
    /// The try of the order, from 1.
    pub order_try: u64,
}

impl PartialEq<ServiceReply> for Answer {
    fn eq(&self, other: &ServiceReply) -> bool {
        self.reply == *other
    }
}

impl PartialEq<crate::wire::Reply> for Answer {
    fn eq(&self, other: &crate::wire::Reply) -> bool {
        self.reply == *other
    }
}

/// An answer as it is read: [`ServiceReply`]'s fields, `features`, `halts` and `acks`, and no
/// other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnswer {
    reply: String,
    service: Option<String>,
    version: Option<Version>,
    features: Option<Vec<String>>,
    #[serde(default)]
    halts: BTreeMap<String, u64>,
    #[serde(default)]
    acks: Vec<VerdictAck>,
}

impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = RawAnswer::deserialize(d)?;
        if let Some(names) = &raw.features {
            check_features(names).map_err(D::Error::custom)?;
        }
        check_halts(&raw.halts).map_err(D::Error::custom)?;
        check_acks(&raw.acks).map_err(D::Error::custom)?;
        let reply = ServiceReply::from_parts(raw.reply, raw.service, raw.version)
            .map_err(D::Error::custom)?;
        Ok(Self {
            reply,
            features: raw.features,
            halts: raw.halts,
            acks: raw.acks,
        })
    }
}
