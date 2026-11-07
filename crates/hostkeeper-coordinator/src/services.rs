//! Services as data: what the coordinator updates and the roles each service runs on
//! (`docs/design/host-updates.md`).
//!
//! The rollout file (`--rollout-file`) is TOML with one `[[service]]` table for each service the
//! coordinator manages. A [`ServiceDef`] has a name, a version scheme, the roles it runs on (a
//! plain list of role names: the base of its assignment, [`crate::assignment`]) and the settings
//! of its rollout: its priority, the hosts it is selected for, how long an updated host holds its
//! slot, its update plan and canary ([`crate::stages`]), the automatic rollback and the backoff
//! of a deferral. The decision reads
//! these and branches on no service name.
//!
//! No service is built in: the services are the ones the file defines (an empty file defines
//! none). A role is any [`RoleName`]. Every table refuses a key it does not know, naming it, so
//! a key an earlier build read and this one does not (`after`, `require_min_up`, and the
//! `max_updating`, `min_up`, `never_last` and `after` of a role's table) is refused, never
//! ignored. `install_when_absent = true` is refused the same way: a host assigned the service
//! that does not run it is ordered `install` ([`crate::decide`]); `install_when_absent = false`,
//! what an earlier build did by default and what this one does for an agent that cannot take
//! an install, is read and changes nothing. Which services a CI token may set is not here: the
//! allow-list entry that admits the token says (`services = [...]`, default the coordinator's
//! legacy service).
//!
//! ```toml
//! [[service]]
//! name = "agent"
//! version_scheme = "semver"      # default semver, the only scheme there is
//! roles = ["storage", "daemon"]  # a host of any other role is ordered for the service only
//!                                # when an operator assigned it there
//! priority = 0                   # default 0; a host is offered the higher number first
//! bake_beats = 0                 # default 0, at most 100: healthy beats before a slot frees
//! canary = { count = 1 }         # optional: a first stage of this many hosts
//! stages = [1, 10, { percent = 50, advance = "manual" }, 100]
//!                                # the update plan: each stage the percent of the eligible
//!                                # hosts done once it is done, rising to 100; a stage is entered
//!                                # by itself ("auto", the default) or by an operator's Advance
//!                                # ("manual"); default [1, 10, 50, 100]
//! rollback = "manual"            # or "automatic": the coordinator sets the last good version
//!                                # when a release fails on enough hosts (default manual)
//! rollback_after_failed = { percent = 25, min_hosts = 2 }  # the larger of the two (default)
//! min_version = "0.0.18"         # optional: the first version with update hooks; setting the
//!                                # desired state to it forgets the last good version
//! defer_backoff = { initial_secs = 300, factor = 2, max_secs = 3600 }   # the defaults
//!
//! [service.selector]             # optional; absent: a host belongs if it announces the service,
//!                                # or is missing it while it could be ordered to install it
//! roles = ["daemon"]             # default: every role of the service
//! labels = { zone = "a" }        # operator-set labels the host must carry
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hostkeeper_wire::RoleName;
use hostkeeper_wire::secret::read_secret;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{MAX_SERVICE_NAME, MAX_SERVICES, is_service_name};
use serde::Deserialize;

use crate::bake::MAX_BAKE_BEATS;
use crate::canary::Canary;
use crate::defer::DeferBackoff;
use crate::desired::{FORMAT_ROLES, MAX_COUNT};
use crate::membership::{RawSelector, Selector};
use crate::reconcile::{FailThreshold, Rollback};
use crate::stages::{Plan, RawStage};

/// The most bytes of a rollout file.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// How the versions of a service are told apart and ordered. There is one scheme, and it is the
/// default; the key `version_scheme` of the rollout file names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VersionScheme {
    /// Semantic versions, as [`Version`] reads them.
    #[default]
    Semver,
}

impl VersionScheme {
    /// Whether the scheme can order `version`, which [`Version::parse`] has accepted already.
    ///
    /// `semver`: the version starts with its release numbers, one or more decimal numbers split
    /// by single dots, each fitting 64 bits and without a leading zero (`0` itself is one), and
    /// then ends or goes on with `-`, `+` or `~` (a pre-release, a build, a Debian revision):
    /// `1.0`, `0.1.10+g0123456789ab-8` and `1.2.3~rc1-1` are read; `1..2`, `1.`, `1.0a`,
    /// `01.2` and a number past 64 bits are not, since [`Version::release_numbers`] would order
    /// them by a guess.
    #[must_use]
    pub fn parses(self, version: &Version) -> bool {
        match self {
            Self::Semver => {
                let text = version.as_str();
                let end = text
                    .find(|c: char| !c.is_ascii_digit() && c != '.')
                    .unwrap_or(text.len());
                let (numbers, rest) = text.split_at(end);
                let number = |part: &str| {
                    !part.is_empty()
                        && (part == "0" || !part.starts_with('0'))
                        && part.parse::<u64>().is_ok()
                };
                numbers.split('.').all(number)
                    && rest
                        .chars()
                        .next()
                        .is_none_or(|c| matches!(c, '-' | '+' | '~'))
            }
        }
    }
}

/// A service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceDef {
    /// Its name.
    pub name: String,
    /// How its versions are ordered.
    pub version_scheme: VersionScheme,
    /// The roles it runs on: the base of its assignment, on which the operator's entries go
    /// ([`crate::assignment`]). A host of any other role is ordered for it only when an operator
    /// assigned it there.
    pub roles: BTreeSet<String>,
    /// Which service a host is offered first when more than one is behind: the higher
    /// number first, and services with the same number in the tie order of
    /// [`crate::service::Coordinator::heartbeat`]. It orders offers only: a service whose
    /// order cannot be given yet (its update plan, another service's live rollout) yields to the next one
    /// whatever its priority.
    pub priority: i32,
    /// Which hosts should run the service (`None`: the hosts that announce it).
    pub selector: Option<Selector>,
    /// How many healthy beats a host at the desired version must send before it frees the
    /// slot it held while updating (default 0: it frees it at once).
    pub bake_beats: u32,
    /// The hosts that go alone first, if the service has a canary: the first stage of its
    /// `plan`, kept for the rollback, which a canary's failure reaches alone.
    pub canary: Option<Canary>,
    /// Its update plan: the stages it rolls out by ([`crate::stages`]), the canary's first.
    pub plan: Plan,
    /// Whether the coordinator rolls the service back by itself.
    pub rollback: Rollback,
    /// How many hosts must fail before it does.
    pub rollback_after_failed: FailThreshold,
    /// The first version that has update hooks: no rollback goes below it.
    pub min_version: Option<Version>,
    /// How long a host that deferred an update is left alone ([`crate::defer`]).
    pub defer_backoff: DeferBackoff,
}

impl ServiceDef {
    /// A service named `name` that runs on the roles `roles`, with every other setting at its
    /// default.
    #[cfg(test)]
    pub(crate) fn new(name: &str, roles: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            version_scheme: VersionScheme::Semver,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            priority: 0,
            selector: None,
            bake_beats: 0,
            canary: None,
            plan: Plan::default(),
            rollback: Rollback::Manual,
            rollback_after_failed: FailThreshold::default(),
            min_version: None,
            defer_backoff: DeferBackoff::default(),
        }
    }

    /// The names of its roles: the desired-state format's four roles first, in their order
    /// (frontend, storage, daemon, mac-daemon), then any other by name. Every list of roles is
    /// in this order.
    #[must_use]
    pub fn role_names(&self) -> Vec<&str> {
        let legacy = FORMAT_ROLES
            .into_iter()
            .filter(|role| self.roles.contains(*role));
        let rest = self
            .roles
            .iter()
            .map(String::as_str)
            .filter(|role| !FORMAT_ROLES.contains(role));
        legacy.chain(rest).collect()
    }
}

/// Why a rollout file is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServicesError {
    /// The file cannot be read.
    #[error("read {0}")]
    Read(String),
    /// The text is not TOML of the shape.
    #[error("services: {0}")]
    Parse(String),
    /// A value is unusable.
    #[error("services: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    service: Vec<RawService>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    name: String,
    version_scheme: Option<String>,
    roles: Option<RawRoles>,
    #[serde(default)]
    priority: i32,
    selector: Option<RawSelector>,
    install_when_absent: Option<bool>,
    #[serde(default)]
    bake_beats: u32,
    canary: Option<RawCanary>,
    stages: Option<Vec<RawStage>>,
    #[serde(default)]
    rollback: Rollback,
    #[serde(default)]
    rollback_after_failed: FailThreshold,
    min_version: Option<Version>,
    #[serde(default)]
    defer_backoff: DeferBackoff,
}

/// The `roles` key: a list of role names. Anything else, the `[service.roles.<name>]` tables of
/// an earlier build included, is kept as it is to be refused with a text that says what to write.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawRoles {
    Names(Vec<String>),
    Other(toml::Value),
}

/// The count form of the canary. The roles and labels form is refused as an unknown field
/// until it is implemented.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCanary {
    count: u32,
}

fn invalid<T>(why: String) -> Result<T, ServicesError> {
    Err(ServicesError::Invalid(why))
}

/// The one place that says which role names a rollout file may use: any [`RoleName`].
fn role_of(name: &str) -> Result<String, ServicesError> {
    name.parse::<RoleName>().map_or_else(
        |_| invalid(format!("{name} is not a role name")),
        |role| Ok(role.name().to_owned()),
    )
}

/// The roles of `service` from its `roles` key: each a role name, none listed twice. No key is
/// no role (the caller refuses a service with none).
fn role_set(service: &str, roles: Option<RawRoles>) -> Result<BTreeSet<String>, ServicesError> {
    let names = match roles {
        None => Vec::new(),
        Some(RawRoles::Names(names)) => names,
        Some(RawRoles::Other(value)) => {
            return invalid(format!(
                "{service}: roles must be a list of role names, roles = [\"daemon\"], not a value \
                 of type {}; the [service.roles.<name>] tables, with their max_updating, min_up, \
                 never_last and after keys, are no longer read",
                value.type_str()
            ));
        }
    };
    let mut roles = BTreeSet::new();
    for name in names {
        if !roles.insert(role_of(&name)?) {
            return invalid(format!("{service}: roles lists {name} twice"));
        }
    }
    Ok(roles)
}

fn service_def(
    raw: RawService,
    defined: &BTreeMap<String, ServiceDef>,
) -> Result<ServiceDef, ServicesError> {
    let RawService {
        name,
        version_scheme,
        roles,
        priority,
        selector,
        install_when_absent,
        bake_beats,
        canary,
        stages,
        rollback,
        rollback_after_failed,
        min_version,
        defer_backoff,
    } = raw;
    if !is_service_name(&name) {
        return invalid(format!(
            "service name {name:?} is not 1 to {MAX_SERVICE_NAME} bytes of lowercase letters, digits and -"
        ));
    }
    if defined.contains_key(&name) {
        return invalid(format!("{name} is defined twice"));
    }
    if install_when_absent == Some(true) {
        return invalid(format!(
            "{name}: install_when_absent = true is no longer read: a host the service is \
             assigned to that does not run it is ordered to install it; take the key out"
        ));
    }
    defer_backoff
        .check()
        .or_else(|why| invalid(format!("{name}: defer_backoff: {why}")))?;
    let version_scheme = match version_scheme.as_deref() {
        None | Some("semver") => VersionScheme::Semver,
        Some(other) => {
            return invalid(format!(
                "{name}: version_scheme is {other:?}, and \"semver\" is the only scheme"
            ));
        }
    };
    let roles = role_set(&name, roles)?;
    if roles.is_empty() {
        return invalid(format!("{name} has no roles"));
    }
    if bake_beats > MAX_BAKE_BEATS {
        return invalid(format!("{name}: bake_beats is over {MAX_BAKE_BEATS}"));
    }
    if canary
        .as_ref()
        .is_some_and(|c| c.count == 0 || c.count > MAX_COUNT)
    {
        return invalid(format!("{name}: canary count is not 1 to {MAX_COUNT}"));
    }
    let canary = canary.map(|c| Canary { count: c.count });
    let plan = Plan::read(stages, canary).or_else(|why| invalid(format!("{name}: {why}")))?;
    if !(1..=100).contains(&rollback_after_failed.percent) {
        return invalid(format!(
            "{name}: rollback_after_failed percent is not 1 to 100"
        ));
    }
    if !(1..=MAX_COUNT).contains(&rollback_after_failed.min_hosts) {
        return invalid(format!(
            "{name}: rollback_after_failed min_hosts is not 1 to {MAX_COUNT}"
        ));
    }
    let selector = selector
        .map(|raw| Selector::new(raw, &roles))
        .transpose()
        .or_else(|why| invalid(format!("{name}: selector {why}")))?;
    Ok(ServiceDef {
        name,
        version_scheme,
        roles,
        priority,
        selector,
        bake_beats,
        canary,
        plan,
        rollback,
        rollback_after_failed,
        min_version,
        defer_backoff,
    })
}

/// Every service the coordinator manages, and the ones CI's token may set.
///
/// The default is no service: the services are the ones a rollout file defines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Services {
    defs: BTreeMap<String, ServiceDef>,
}

impl Services {
    /// These services and `def`, for a test that needs a role the loader refuses.
    #[cfg(test)]
    pub(crate) fn with(mut self, def: ServiceDef) -> Self {
        self.defs.insert(def.name.clone(), def);
        self
    }

    /// The services of the file text `text`, exactly the ones it defines.
    ///
    /// # Errors
    /// [`ServicesError`]: the text is not TOML of the shape, a key is unknown (a key an
    /// earlier build read included), a name is bad or repeated, a service has no roles or lists
    /// one twice, there are more than [`MAX_SERVICES`] services, a role name, a count or the
    /// `version_scheme` is unusable, or a selector or the backoff is refused.
    pub fn parse(text: &str) -> Result<Self, ServicesError> {
        let file: RawFile =
            toml::from_str(text).map_err(|e| ServicesError::Parse(e.to_string()))?;
        if file.service.len() > MAX_SERVICES {
            return invalid(format!("more than {MAX_SERVICES} services"));
        }
        let mut services = Self::default();
        for raw in file.service {
            let def = service_def(raw, &services.defs)?;
            services.defs.insert(def.name.clone(), def);
        }
        Ok(services)
    }

    /// The services in the rollout file at `path`. The file is read under the rule of the operator's
    /// token file: a regular file, not a symlink, owned by this process's user, mode `0600`
    /// or `0400`. Whoever can write it controls the rollout limits (a replaced definition can
    /// drop every gate), so it is held to the same rule as a secret.
    ///
    /// # Errors
    /// [`ServicesError`]: the file fails the rule, cannot be read, is over [`MAX_FILE_BYTES`],
    /// or its text is refused by [`Self::parse`].
    pub fn from_file(path: &Path) -> Result<Self, ServicesError> {
        let shown = path.display().to_string();
        let bytes = read_secret(path, &shown, MAX_FILE_BYTES)
            .map_err(|e| ServicesError::Read(e.to_string()))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(ServicesError::Read(format!(
                "{shown}: it is over {MAX_FILE_BYTES} bytes"
            )));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| ServicesError::Read(format!("{shown}: it is not UTF-8")))?;
        Self::parse(&text)
    }

    /// The services `defs` and nothing else.
    #[cfg(test)]
    pub(crate) fn from_defs(defs: Vec<ServiceDef>) -> Self {
        Self {
            defs: defs
                .into_iter()
                .map(|def| (def.name.clone(), def))
                .collect(),
        }
    }

    /// Every service the coordinator manages, by name.
    pub fn defs(&self) -> impl Iterator<Item = &ServiceDef> {
        self.defs.values()
    }

    /// The names of every service the coordinator manages, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defs.keys().map(String::as_str)
    }

    /// The service `name`, if the coordinator manages it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ServiceDef> {
        self.defs.get(name)
    }
}
