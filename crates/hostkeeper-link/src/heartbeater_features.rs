//! The features a heartbeat announces, and the reports and answers that only a feature the
//! coordinator advertised lets through: the maintenance report (`maintenance-v1`) and the host's
//! hardware health (`host-health-v1`). A child of [`super`], so that the loop's file keeps its
//! bound of lines.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::Heartbeater;
use crate::beat::{
    ANNOUNCED, FACTS, Feature, HOST_HEALTH, MAINTENANCE, OPERATOR_KEYS, PREREQUISITES,
    PRIVATE_REPOSITORY, REPOSITORIES, SSH,
};

impl Heartbeater {
    /// The features every heartbeat announces: [`ANNOUNCED`], `repositories-v1` when the host
    /// has a repository configuration to manage, `facts-v1` when it has a facts file, and
    /// `private-repository-v1` when it has a repository configuration or is told it reads
    /// private repositories, `operator-keys-v1` when it keeps operator keys, and
    /// `prerequisites-v1` when it keeps host prerequisites, and `ssh-v1` when it sets sshd up and
    /// the coordinator's last reply advertised `ssh-v1`, and `maintenance-v1` when it reads the
    /// host's maintenance and the last reply advertised `maintenance-v1`, and `host-health-v1`
    /// when it reads the host's hardware health and the last reply advertised `host-health-v1`: a
    /// coordinator of a release before `ssh-v1` reads at most 16 names and drops a longer list
    /// whole, and a daemon with every port names 17 to it (18 or 19 to one that advertises
    /// `maintenance-v1` and `host-health-v1`). A daemon that reads the host's maintenance but has
    /// no runner of `maintain` orders announces no `maintenance-v1`.
    pub(super) fn announced(&self) -> Vec<Feature> {
        let mut features = ANNOUNCED.to_vec();
        if self.ports.repositories.is_some() {
            features.push(REPOSITORIES);
        }
        if self.ports.facts.is_some() {
            features.push(FACTS);
        }
        if self.ports.repositories.is_some() || self.config.private_repository {
            features.push(PRIVATE_REPOSITORY);
        }
        if self.ports.operator_keys.is_some() {
            features.push(OPERATOR_KEYS);
        }
        if self.ports.prerequisites.is_some() {
            features.push(PREREQUISITES);
        }
        if self.ports.ssh.is_some() && self.advertised.has(Feature::SshV1) {
            features.push(SSH);
        }
        if self.maintains() {
            features.push(MAINTENANCE);
        }
        if self.reads_health() {
            features.push(HOST_HEALTH);
        }
        features
    }

    /// Whether the heartbeat announces `maintenance-v1` and sends the maintenance report: the
    /// host reads its maintenance and runs `maintain` orders, and the last reply advertised it.
    pub(super) fn maintains(&self) -> bool {
        self.ports.maintenance.is_some()
            && self.ports.maintain.is_some()
            && self.advertised.has(Feature::MaintenanceV1)
    }

    /// Whether the heartbeat announces `host-health-v1` and sends the host's hardware health: the
    /// host reads its health, and the last reply advertised it.
    fn reads_health(&self) -> bool {
        self.ports.health.is_some() && self.advertised.has(Feature::HostHealthV1)
    }

    /// The health the heartbeat carries: what the host's source gives, when the heartbeat
    /// announces `host-health-v1`; none otherwise.
    pub(super) fn health(&self) -> Option<pb::HostHealth> {
        self.ports
            .health
            .as_ref()
            .filter(|_| self.reads_health())
            .map(|source| source.health())
    }

    /// Tells the host's source which records a reply says the coordinator wrote. Only a
    /// coordinator that wrote them names them, so they are passed on whatever the reply
    /// advertises: a record written is one the host no longer holds to send.
    pub(super) fn health_written(&self, written: &[String]) {
        if let Some(source) = &self.ports.health
            && !written.is_empty()
        {
            source.written(written);
        }
    }
}
