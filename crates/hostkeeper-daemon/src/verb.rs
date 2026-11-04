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
use hostkeeper_proto::hostkeeper::agent::v1::{Order, order};

use crate::checks;

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

/// The verbs that run a package's hook to a verdict through the step runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookedVerb {
    /// Refused below the running version.
    Update,
    /// A hooked update to any version, lower ones included (automatic rollback sends it).
    ForceRollback,
    /// The update path with no previous release, for a service with none installed.
    Install,
}

impl HookedVerb {
    /// The verb's name in the order.
    pub const fn name(self) -> &'static str {
        match self {
            HookedVerb::Update => "update",
            HookedVerb::ForceRollback => "force_rollback",
            HookedVerb::Install => "install",
        }
    }
}

/// The arguments of an order that names a package: `update`, `force_rollback`, `install`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hooked {
    pub service: ServiceName,
    pub version: Version,
    pub digest: PackageDigest,
    /// The coordinator's try of the order, from 1.
    pub order_try: NonZeroU64,
}

/// The arguments of an `uninstall`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unhooked {
    pub service: ServiceName,
    /// The coordinator's try of the order, from 1: reported with the uninstall's verdict.
    pub order_try: NonZeroU64,
}

/// The arguments of a `force`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forced {
    pub package: Hooked,
    /// Greater than the generation the service is held under: a force lifts a standing halt.
    pub halt_generation: u64,
}

/// One verb with its typed arguments. The executor matches it exhaustively; a verb a later
/// change builds fills its arm.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Local only: the admin socket's `ClearRecord`; no order carries it.
    ClearRecord(ServiceName),
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
    order_try: NonZeroU64,
) -> Result<Hooked, BadOrder> {
    Ok(Hooked {
        service: service(verb, service_text)?,
        version: checks::version(version).map_err(|_| bad(verb, "version", VERSION_RULE))?,
        digest: checks::digest(digest).map_err(|_| bad(verb, "package_digest", DIGEST_RULE))?,
        order_try,
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
        Ok(match verb {
            order::Verb::Update(a) => Verb::Update(hooked(
                "update",
                (&a.service, &a.version, &a.package_digest),
                order_try,
            )?),
            order::Verb::ForceRollback(a) => Verb::ForceRollback(hooked(
                "force_rollback",
                (&a.service, &a.version, &a.package_digest),
                order_try,
            )?),
            order::Verb::Install(a) => Verb::Install(hooked(
                "install",
                (&a.service, &a.version, &a.package_digest),
                order_try,
            )?),
            order::Verb::Force(a) => Verb::Force(Forced {
                package: hooked(
                    "force",
                    (&a.service, &a.version, &a.package_digest),
                    order_try,
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
        })
    }
}
