//! The rig of the `maintain` tests: services whose release names `drain` and `resume`, the
//! `maintain` order, and readers of what the turn launched and of its run record. Every step runs
//! through the rig's `ScriptedLauncher`: the maintenance hook's steps as additive steps of the
//! service [`HOOK`] (one script of exit codes for all three), each service's `drain` and
//! `resume` as additive steps of that service.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::sync::Arc;

use hostkeeper_daemon::maintenance_hook::{Kind as HookKind, RunRecord, StoreRecord};
use hostkeeper_daemon::testing::{FakeClock, manifest};
use hostkeeper_proto::hostkeeper::agent::v1::{
    LinuxScope, Maintain, MaintainAction, MaintenanceKind, Order, order,
};
use hostkeeper_steps::BootId;
use hostkeeper_steps::maintenance::{MaintenanceLoaded, MaintenanceRecord, MaintenanceStore};

use crate::support::{Rig, manifest_with};

/// The run of every order here.
pub const RUN: &str = "run-1";
/// The order of every turn here.
pub const ORDER: &str = "order-1";
/// The service the maintenance hook's steps are launched as.
pub const HOOK: &str = "hostkeeper";
/// The `[hook]` lines of a release that names `drain` and `resume`.
pub const DRAINS: &str = "drain = \"drain\"\nresume = \"resume\"";

/// `service` runs `version` from a sealed package whose manifest names `drain` and `resume`.
pub fn draining(rig: &Rig, service: &str, version: &str) {
    rig.add_with(service, version, &manifest_with(service, version, DRAINS));
    rig.store.link(service, "current", version);
}

/// `service` runs `version` from a sealed package whose manifest names neither.
pub fn plain(rig: &Rig, service: &str, version: &str) {
    rig.add_with(service, version, &manifest(service, version));
    rig.store.link(service, "current", version);
}

/// The terms of a run of `kinds` (the Linux scope `security`), with the drain deadline
/// `deadline`.
pub fn terms(kinds: &[MaintenanceKind], deadline: Option<u64>) -> Maintain {
    Maintain {
        run_id: RUN.to_owned(),
        action: MaintainAction::Run.into(),
        kinds: kinds.iter().map(|kind| i32::from(*kind)).collect(),
        linux_scope: LinuxScope::Security.into(),
        drain_deadline_secs: deadline,
        ..Maintain::default()
    }
}

/// The `maintain` order `order_id`, try `order_try`, of `maintain`.
pub fn order_of(order_id: &str, order_try: u64, maintain: Maintain) -> Order {
    Order {
        order_id: order_id.to_owned(),
        r#try: order_try,
        verb: Some(order::Verb::Maintain(maintain)),
    }
}

/// The order [`ORDER`], try 1, of a run of `kinds` with no drain deadline.
pub fn maintain(kinds: &[MaintenanceKind]) -> Order {
    order_of(ORDER, 1, terms(kinds, None))
}

/// The run record of the rig's daemon, `None` when there is none.
pub fn record(rig: &Rig) -> Option<MaintenanceRecord> {
    match MaintenanceStore::beside(&rig.state).load() {
        Ok(MaintenanceLoaded::Current(record)) => Some(*record),
        _ => None,
    }
}

/// Every additive step launched, as `<service> <step>`, in order.
pub fn launched(rig: &Rig) -> Vec<String> {
    rig.launcher
        .additive_runs()
        .iter()
        .map(|ran| format!("{} {}", ran.service, ran.step))
        .collect()
}

/// The params file of the `n`-th additive step launched (from 0).
pub fn params_of(rig: &Rig, n: usize) -> String {
    rig.launcher.additive_runs()[n].params.clone()
}

/// The boot id `text`.
pub fn boot(text: &str) -> BootId {
    BootId::new("the test", text).expect("a boot id")
}

/// Makes the hook's `cutover` of `kind` act as a kind that needs a reboot: inside the launch, it
/// writes the kind's reboot under way into the run record beside its params file, with the boot
/// id the rig's clock reads, through the hook's own record ([`StoreRecord`]). Every other
/// additive step does nothing.
pub fn reboot_on_cutover(rig: &Rig, kind: HookKind, wire: MaintenanceKind) {
    let clock: Arc<FakeClock> = Arc::clone(&rig.clock);
    let named = format!("\"{}\"", wire.as_str_name());
    rig.launcher.during_additive(move |call| {
        let params = std::fs::read_to_string(call.params).unwrap_or_default();
        if call.step != "cutover" || !params.contains(&named) {
            return;
        }
        let record = StoreRecord::beside_params(call.params).expect("the record beside");
        let now = hostkeeper_steps::Clock::boot_id(&*clock).expect("the boot id");
        record
            .reboot_under_way(RUN, kind, &now)
            .expect("the reboot is recorded");
    });
}
