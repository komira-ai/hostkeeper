//! The verbs compiled into the daemon (design section 4.1), each with typed arguments, decoded
//! from the coordinator's `Order` by one `TryFrom`.
//!
//! Decoding is where an order's fields are checked against their grammars ([`crate::checks`]):
//! an order with no verb (or a verb this daemon does not know, which decodes as none), no try,
//! or a field that breaks its grammar is refused naming the field, before any lock is taken and
//! before anything is read or written.

use std::fmt;
use std::num::NonZeroU64;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, MaintainAction, Order, order};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_proto::maintenance::MAX_RUN_ID;

use crate::checks;
use crate::maintenance_hook::{MaintainParams, ParamsError};

/// A package digest as an order carries it: the SHA-256 of the package as 64 lower-case hex
/// digits (the units of `hostkeeper/agent/v1/agent.proto`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDigest(String);

impl PackageDigest {
    /// `text`, when it is 64 lower-case hex digits.
    pub fn new(text: &str) -> Option<PackageDigest> {
        let hex = |b: u8| b.is_ascii_digit() | (b'a'..=b'f').contains(&b);
        ((text.len() == 64) & text.bytes().all(hex)).then(|| PackageDigest(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The verbs that switch a service to a package under the service's lock: those that run its
/// hook to a verdict through the step runner, and `force`, which runs no hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookedVerb {
    /// Refused below the running version.
    Update,
    /// A hooked update to any version, lower ones included (automatic rollback sends it).
    ForceRollback,
    /// The hookless switch of design section 5.6, to any version, lower ones included.
    Force,
    /// The update path with no previous release, for a service with none installed.
    Install,
}

impl HookedVerb {
    /// The verb's name in the order.
    pub const fn name(self) -> &'static str {
        match self {
            HookedVerb::Update => "update",
            HookedVerb::ForceRollback => "force_rollback",
            HookedVerb::Force => "force",
            HookedVerb::Install => "install",
        }
    }
}

/// The arguments of an order that names a package: `update`, `force_rollback`, `install`.
/// (Not `Eq`: the entry holds a float of its manifest.)
#[derive(Clone, Debug, PartialEq)]
pub struct Hooked {
    pub service: ServiceName,
    pub version: Version,
    pub digest: PackageDigest,
    /// The coordinator's try of the order, from 1.
    pub order_try: NonZeroU64,
    /// The coordinator's id of the order; `None` when it carries none. A force keeps it in its
    /// record and reports it with its verdict; the update of the daemon itself keeps it in its
    /// wait for the restart ([`crate::selfupdate`]), for the next binary to report its verdict.
    pub order_id: Option<String>,
    /// The entry the coordinator picked, as the order's wire `target` carries it, undecoded: its
    /// checks are the executor's ([`crate::entry::check`]), not the grammar of decoding. `None`
    /// for an order without one, which the executor refuses (`order_invalid`): the daemon stages
    /// a version from the entry alone. The order's `index_digest` is not read.
    pub entry: Option<Target>,
}

/// The arguments of an `uninstall`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unhooked {
    pub service: ServiceName,
    /// The coordinator's try of the order, from 1: reported with the uninstall's verdict.
    pub order_try: NonZeroU64,
}

/// The arguments of a `force`.
#[derive(Clone, Debug, PartialEq)]
pub struct Forced {
    pub package: Hooked,
    /// Greater than the generation the service is held under: a force lifts a standing halt.
    pub halt_generation: u64,
}

/// One verb with its typed arguments. The executor matches it exhaustively; a verb a later
/// change builds fills its arm.
#[derive(Clone, Debug, PartialEq)]
pub enum Verb {
    Update(Hooked),
    ForceRollback(Hooked),
    Install(Hooked),
    Uninstall(Unhooked),
    Force(Forced),
    Abort(ServiceName),
    ClearHalt {
        service: ServiceName,
        halt_generation: u64,
    },
    /// The `clear_record` case of an order.
    ClearRecord(ServiceName),
    /// The `maintain` case of an order: a host's turn of a maintenance run.
    Maintain(Box<Maintained>),
}

/// The arguments of a `maintain` (docs/design/maintenance-updates.md, section 5.1): the order's
/// id and try, and its terms, checked as the maintenance hook reads them.
#[derive(Clone, Debug, PartialEq)]
pub struct Maintained {
    /// The coordinator's id of the order: 1 to [`MAX_RUN_ID`] bytes, the bound the report gives
    /// it back under.
    pub order_id: String,
    /// The coordinator's try of the order, from 1.
    pub order_try: NonZeroU64,
    /// The run: its id and kinds checked ([`MaintainParams::new`]), its action `RUN`.
    pub params: MaintainParams,
}

/// Why an order could not be decoded: the field, dotted (`update.version`), and its rule.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the order's {field} {rule}")]
pub struct BadOrder {
    pub field: String,
    pub rule: &'static str,
}

/// What a verb that is not set breaks.
pub const NO_VERB: &str = "is not set, or is a verb this daemon does not know";
/// What a try of 0 breaks.
pub const NO_TRY: &str = "is 0 or missing: a try counts from 1";
/// What a service name that breaks its rule breaks: the config crate's words.
pub const SERVICE_RULE: &str = hostkeeper_config::names::SERVICE_NAME_RULE;
/// What a version that breaks its rule breaks: the config crate's words.
pub const VERSION_RULE: &str = hostkeeper_config::names::VERSION_RULE;
/// What a malformed package digest breaks.
pub const DIGEST_RULE: &str = "is not a SHA-256 of 64 lower-case hex digits";
/// What the order id of a `maintain` breaks.
pub const ORDER_ID_RULE: &str = "is not 1 to 64 bytes";
/// What a `maintain` whose action is not `RUN` breaks.
pub const ACTION_RULE: &str =
    "is not RUN: this daemon runs a host's turn, and no retry or put back";
/// What the run id of a `maintain` breaks.
pub const RUN_ID_RULE: &str = "is not 1 to 64 bytes";
/// What the kinds of a `maintain` break.
pub const KINDS_RULE: &str = "are not 1 to 4 known kinds, each given once";

/// The arguments of `maintain`, from the order `order_id` try `order_try`.
fn maintained(
    maintain: &Maintain,
    (order_try, order_id): (NonZeroU64, &str),
) -> Result<Maintained, BadOrder> {
    if !(1..=MAX_RUN_ID).contains(&order_id.len()) {
        return Err(BadOrder {
            field: "order_id".to_owned(),
            rule: ORDER_ID_RULE,
        });
    }
    if maintain.action != i32::from(MaintainAction::Run) {
        return Err(bad("maintain", "action", ACTION_RULE));
    }
    let params = MaintainParams::new(maintain.clone()).map_err(|error| match error {
        ParamsError::RunId => bad("maintain", "run_id", RUN_ID_RULE),
        _ => bad("maintain", "kinds", KINDS_RULE),
    })?;
    Ok(Maintained {
        order_id: order_id.to_owned(),
        order_try,
        params,
    })
}

fn bad(verb: &str, field: &str, rule: &'static str) -> BadOrder {
    BadOrder {
        field: format!("{verb}.{field}"),
        rule,
    }
}

fn service(verb: &str, text: &str) -> Result<ServiceName, BadOrder> {
    checks::service_name(text).map_err(|_| bad(verb, "service", SERVICE_RULE))
}

fn hooked(
    verb: &str,
    (service_text, version, digest): (&str, &str, &str),
    (order_try, order_id): (NonZeroU64, &str),
    entry: Option<&Target>,
) -> Result<Hooked, BadOrder> {
    Ok(Hooked {
        service: service(verb, service_text)?,
        version: checks::version(version).map_err(|_| bad(verb, "version", VERSION_RULE))?,
        digest: checks::digest(digest).map_err(|_| bad(verb, "package_digest", DIGEST_RULE))?,
        order_try,
        order_id: Some(order_id.to_owned()).filter(|id| !id.is_empty()),
        entry: entry.cloned(),
    })
}

impl TryFrom<&Order> for Verb {
    type Error = BadOrder;

    fn try_from(order: &Order) -> Result<Verb, BadOrder> {
        let no_verb = || BadOrder {
            field: "verb".to_owned(),
            rule: NO_VERB,
        };
        let verb = order.verb.as_ref().ok_or_else(no_verb)?;
        let no_try = || BadOrder {
            field: "try".to_owned(),
            rule: NO_TRY,
        };
        let order_try = NonZeroU64::new(order.r#try).ok_or_else(no_try)?;
        let of = (order_try, order.order_id.as_str());
        Ok(match verb {
            order::Verb::Update(a) => Verb::Update(hooked(
                "update",
                (&a.service, &a.version, &a.package_digest),
                of,
                a.target.as_ref(),
            )?),
            order::Verb::ForceRollback(a) => Verb::ForceRollback(hooked(
                "force_rollback",
                (&a.service, &a.version, &a.package_digest),
                of,
                a.target.as_ref(),
            )?),
            order::Verb::Install(a) => Verb::Install(hooked(
                "install",
                (&a.service, &a.version, &a.package_digest),
                of,
                a.target.as_ref(),
            )?),
            order::Verb::Force(a) => Verb::Force(Forced {
                package: hooked(
                    "force",
                    (&a.service, &a.version, &a.package_digest),
                    of,
                    a.target.as_ref(),
                )?,
                halt_generation: a.halt_generation,
            }),
            order::Verb::Uninstall(a) => Verb::Uninstall(Unhooked {
                service: service("uninstall", &a.service)?,
                order_try,
            }),
            order::Verb::Abort(a) => Verb::Abort(service("abort", &a.service)?),
            order::Verb::ClearHalt(a) => Verb::ClearHalt {
                service: service("clear_halt", &a.service)?,
                halt_generation: a.halt_generation,
            },
            order::Verb::ClearRecord(a) => Verb::ClearRecord(service("clear_record", &a.service)?),
            order::Verb::Maintain(a) => Verb::Maintain(Box::new(maintained(a, of)?)),
        })
    }
}
