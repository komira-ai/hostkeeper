//! The fakes of the maintenance ports ([`FakeMaintenance`], [`FakeMaintain`]), beside the other
//! fakes of [`crate::testing`].

use std::sync::Mutex;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::lock;
use crate::order::MaintainOrder;
use crate::ports::{MaintainSink, MaintenanceSource};

/// The host's maintenance facts, as a test sets them: it holds the report a test gives it and
/// counts how often it was asked.
#[derive(Default)]
pub struct FakeMaintenance {
    report: Mutex<pb::MaintenanceReport>,
    asked: Mutex<usize>,
}

impl FakeMaintenance {
    /// Sets the report the next asks give.
    pub fn set(&self, report: pb::MaintenanceReport) {
        *lock(&self.report) = report;
    }

    /// How many times the loop asked for the report.
    pub fn asked(&self) -> usize {
        *lock(&self.asked)
    }
}

impl MaintenanceSource for FakeMaintenance {
    fn report(&self) -> pb::MaintenanceReport {
        *lock(&self.asked) += 1;
        lock(&self.report).clone()
    }
}

/// The daemon's runner of `maintain` orders, as a test sees it: it records every order it is given.
#[derive(Default)]
pub struct FakeMaintain {
    given: Mutex<Vec<MaintainOrder>>,
}

impl FakeMaintain {
    /// The orders the loop gave, oldest first.
    pub fn given(&self) -> Vec<MaintainOrder> {
        lock(&self.given).clone()
    }
}

impl MaintainSink for FakeMaintain {
    fn maintain(&self, order: &MaintainOrder) {
        lock(&self.given).push(order.clone());
    }
}
