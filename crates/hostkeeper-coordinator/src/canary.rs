//! The canary of a service: `canary = { count = n }` in the rollout file.
//!
//! A canary is the first stage of the service's update plan, of `n` hosts, entered by itself
//! ([`crate::stages::Plan::read`]): `canary = { count = 3 }` on eight hosts with the default plan
//! gives the stages 3, 4 and 8, as `stages = [30, 50, 100]` does. What a canary adds to a first
//! stage is in the rollback: a release failure on a host of the service while fewer than `n`
//! hosts have baked at the desired version rolls back on its own, and raises an alert
//! ([`crate::reconcile`]). The roles and labels form of the canary is not implemented: the
//! rollout file refuses it.

/// The hosts of a service that go alone first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canary {
    /// How many hosts.
    pub count: u32,
}
