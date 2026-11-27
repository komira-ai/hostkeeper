//! The hardware health's fake ([`FakeHostHealth`]), beside the other fakes of
//! [`crate::testing`].

use std::sync::Mutex;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::lock;
use crate::host_health::HostHealthSource;

/// The host's hardware health, as a test sets it: it gives the health a test sets, counts how
/// often it was asked, and records every list of records the loop said were written.
#[derive(Default)]
pub struct FakeHostHealth {
    health: Mutex<pb::HostHealth>,
    asked: Mutex<usize>,
    written: Mutex<Vec<Vec<String>>>,
}

impl FakeHostHealth {
    /// Sets the health the next asks give.
    pub fn set(&self, health: pb::HostHealth) {
        *lock(&self.health) = health;
    }

    /// How many times the loop asked for the health.
    pub fn asked(&self) -> usize {
        *lock(&self.asked)
    }

    /// Each list of `event_id`s the loop said were written, oldest first.
    pub fn written_ids(&self) -> Vec<Vec<String>> {
        lock(&self.written).clone()
    }
}

impl HostHealthSource for FakeHostHealth {
    fn health(&self) -> pb::HostHealth {
        *lock(&self.asked) += 1;
        lock(&self.health).clone()
    }

    fn written(&self, event_ids: &[String]) {
        lock(&self.written).push(event_ids.to_vec());
    }
}
