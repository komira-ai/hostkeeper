//! Which host groups an operator's call about one package reaches (`docs/design/hostgroups.md`,
//! sections 2.1, 4 and 7.2), once the coordinator holds a group.
//!
//! A call that names a group reaches that group, which must list the package; a call that names
//! none reaches each group that lists it, in name order. A call that needs exactly one group
//! ([`Coordinator::reach_one`]) refuses a package more than one group lists when it names none
//! (`ambiguous_host_group`, [`Unique`]). A package no group lists, or a group that does not exist
//! or does not list it, is `not_in_host_group`; a name that is no group name is an invalid
//! request. Each refusal is a value of the refusal enum of the call that made it.
//!
//! A package is known to the coordinator when the rollout file defines it or any group lists it
//! ([`Coordinator::package_def`]): a group's package need not be in the file, whose definition
//! gives a package only what it has alone (its version scheme, its repository, the floor of its
//! minimum version).

use std::borrow::Cow;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::group_scope::package_def;
use crate::holds::HoldScope;
use crate::host_groups::{GroupName, HostGroup, HostGroups};
use crate::ops::OpsState;
use crate::service::Coordinator;
use crate::services::ServiceDef;

/// Why a call reaches no group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missed {
    /// The name given is no group name.
    BadName,
    /// No group has the name, or the group does not list the package, or no group lists it.
    NotListed,
}

/// Why a call that needs exactly one group reaches none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unique {
    /// The call reaches no group.
    Missed(Missed),
    /// It named none, for a package more than one group lists.
    Ambiguous,
}

impl From<Missed> for proto::ReleasesRefusal {
    fn from(missed: Missed) -> Self {
        match missed {
            Missed::BadName => Self::InvalidRequest,
            Missed::NotListed => Self::NotInHostGroup,
        }
    }
}

impl From<Missed> for proto::RolloutsRefusal {
    fn from(missed: Missed) -> Self {
        match missed {
            Missed::BadName => Self::InvalidRequest,
            Missed::NotListed => Self::NotInHostGroup,
        }
    }
}

impl From<Unique> for proto::RolloutsRefusal {
    fn from(unique: Unique) -> Self {
        match unique {
            Unique::Missed(missed) => missed.into(),
            Unique::Ambiguous => Self::AmbiguousHostGroup,
        }
    }
}

/// A group a call reaches: its name, what it holds, and the definition of the package in it.
pub(crate) struct Reached {
    /// The group's name.
    pub(crate) name: GroupName,
    /// The group.
    pub(crate) listed: HostGroup,
    /// The package as the group runs it.
    pub(crate) def: ServiceDef,
}

/// Whether `group` lists `package`.
pub(crate) fn lists(group: &HostGroup, package: &str) -> bool {
    group.packages.iter().any(|listed| listed == package)
}

/// The names of the groups of `groups` that list `package`, in name order.
pub(crate) fn listing(groups: &HostGroups, package: &str) -> Vec<GroupName> {
    groups
        .0
        .iter()
        .filter(|(_, group)| lists(group, package))
        .map(|(name, _)| name.clone())
        .collect()
}

impl Coordinator {
    /// The definition of `package` as the operator's calls read it: the rollout file's, or, for a
    /// package a host group lists that the file does not define, one with every setting at its
    /// default. None for a package neither names.
    pub(crate) fn package_def(&self, package: &str) -> Option<Cow<'_, ServiceDef>> {
        if let Some(def) = self.services.get(package) {
            return Some(Cow::Borrowed(def));
        }
        let listed = self
            .desired
            .host_groups()
            .0
            .values()
            .any(|group| lists(group, package));
        listed.then(|| Cow::Owned(ServiceDef::new(package, &[])))
    }

    /// Whether `scope` is a role scope that names no host group, in a coordinator that holds one:
    /// a role scope there names a group ([`crate::holds::HoldScope::Role`]).
    pub(crate) fn names_no_group(&self, scope: &HoldScope) -> bool {
        let HoldScope::Role(role) = scope else {
            return false;
        };
        self.desired.in_group_mode()
            && role
                .parse::<GroupName>()
                .ok()
                .and_then(|name| self.desired.host_group(&name))
                .is_none()
    }

    /// The groups the call about `package` that names `named` (empty: none) reaches, in name
    /// order, each with the package's definition in it, over the groups `groups` and the
    /// operations state `state`.
    ///
    /// # Errors
    /// [`Missed::BadName`], [`Missed::NotListed`].
    pub(crate) fn reach(
        &self,
        groups: &HostGroups,
        state: &OpsState,
        package: &str,
        named: &str,
    ) -> Result<Vec<Reached>, Missed> {
        let only = match named {
            "" => None,
            name => Some(name.parse::<GroupName>().map_err(|_| Missed::BadName)?),
        };
        let reached: Vec<Reached> = groups
            .0
            .iter()
            .filter(|(name, _)| only.as_ref().is_none_or(|only| only == *name))
            .filter_map(|(name, listed)| {
                let def = package_def(name, listed, &self.services, state, package)?;
                Some(Reached {
                    name: name.clone(),
                    listed: listed.clone(),
                    def,
                })
            })
            .collect();
        if reached.is_empty() {
            return Err(Missed::NotListed);
        }
        Ok(reached)
    }

    /// [`Self::reach`] for a call that acts on one group: the one named, or, when none is, the
    /// only group that lists the package.
    ///
    /// # Errors
    /// [`Unique::Missed`], and [`Unique::Ambiguous`] for more than one group.
    pub(crate) fn reach_one(
        &self,
        groups: &HostGroups,
        state: &OpsState,
        package: &str,
        named: &str,
    ) -> Result<Reached, Unique> {
        let reached = self
            .reach(groups, state, package, named)
            .map_err(Unique::Missed)?;
        let [one] = <[Reached; 1]>::try_from(reached).map_err(|_| Unique::Ambiguous)?;
        Ok(one)
    }
}
