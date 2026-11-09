//! Each shipped plan run against a simulated fleet, its records analysed, and the results page
//! section and result file compared with the plan's golden `tests/golden/<plan>.fake-run.md` and
//! `.fake-run.tsv`. The goldens are simulated runs, not measurements: they hold the path from a
//! plan through the runner and its records to the page, and the layout of the page.
//!
//! A golden that differs fails the test, which writes what the run gave beside the test binary
//! (`CARGO_TARGET_TMPDIR`) to compare and, when the change is meant, copy over the golden.

use std::cell::RefCell;
use std::rc::Rc;

use hostkeeper_measure::analysis;
use hostkeeper_measure::record::{self, RunInfo};
use hostkeeper_measure::render;
use hostkeeper_testkit::Scratch;

use crate::support::{self, FakeClock, Fleet, Operator, SimHost};

const RUN: &str = "fake-run";

/// Writes `index.json` into a scratch directory and gives the `--param` items for repetitions
/// `1..=reps` of a plan with `version` and `index` per repetition, versions from `v2`.
fn per_rep(scratch: &Scratch, reps: u32) -> Vec<String> {
    let index = scratch.path().join("index.json");
    std::fs::write(&index, "{\"version\":\"simulated\"}\n").expect("write the index");
    (1..=reps)
        .flat_map(|k| {
            [
                format!("version@{k}=v{}", k + 1),
                format!("index@{k}={}", index.display()),
            ]
        })
        .collect()
}

fn fleet(hosts: Vec<SimHost>) -> Rc<RefCell<Fleet>> {
    Rc::new(RefCell::new(Fleet::new(
        Rc::new(FakeClock::default()),
        hosts,
    )))
}

/// Runs `id` with `items` against `fleet` and `operator`, checks the run ended well and every
/// record survives a write and a read, and compares the analysis with the goldens.
fn golden(id: &str, items: &[String], fleet: &Rc<RefCell<Fleet>>, mut operator: Operator) {
    let plan = support::shipped(id);
    let params = hostkeeper_measure::params::Params::parse(items).expect("parameters");
    let ran = support::run(&plan, &params, fleet, &mut operator);
    let summary = ran
        .result
        .unwrap_or_else(|e| panic!("{id}: the run failed: {e}"));
    let lines: String = ran.records.iter().map(|r| r.line()).collect();
    let records = record::parse(&lines).expect("the records read back");
    assert_eq!(
        records, ran.records,
        "{id}: a record changed on its way through the file"
    );
    let info = RunInfo {
        plan: plan.id.clone(),
        run: RUN.to_owned(),
        repetitions: plan.repetitions,
    };
    let a = analysis::analyze(&plan, &info, &records).expect("the records analyse");
    assert_eq!(a.started, plan.repetitions, "{id}: {summary:?}");
    let mut differ = Vec::new();
    for (ext, got) in [("md", render::markdown(&a)), ("tsv", render::tsv(&a))] {
        let name = format!("{id}.fake-run.{ext}");
        let path = support::crate_dir().join("tests/golden").join(&name);
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        if got != want {
            let actual = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(&name);
            std::fs::write(&actual, &got).expect("write what the run gave");
            differ.push(format!(
                "{} differs; what the run gave is in {}",
                path.display(),
                actual.display()
            ));
        }
    }
    assert!(differ.is_empty(), "{id}: {differ:#?}");
}

#[test]
fn the_rollout_plan_on_a_simulated_farm_is_its_golden() {
    let scratch = support::scratch("golden-rollout");
    let hosts = vec![
        SimHost::new("node-c", 0, &[("hostkeeper", "v1")]),
        SimHost::new("node-a", 5_000, &[("hostkeeper", "v1")]),
        SimHost::new("node-b", 10_000, &[("hostkeeper", "v1")]),
        SimHost::new("node-d", 3_000, &[("hello", "v1")]),
    ];
    let fleet = fleet(hosts);
    let operator = Operator::new(Rc::clone(&fleet));
    golden("rollout", &per_rep(&scratch, 3), &fleet, operator);
}

#[test]
fn the_update_and_rollback_plan_on_one_simulated_host_is_its_golden() {
    let scratch = support::scratch("golden-update-rollback");
    let hosts = vec![
        SimHost::new("node-a", 0, &[("hello", "v1"), ("hostkeeper", "v1")]),
        SimHost::new("node-b", 7_000, &[("hostkeeper", "v1")]),
    ];
    let fleet = fleet(hosts);
    {
        let mut f = fleet.borrow_mut();
        f.world.first_order_ms = 3_000;
        f.world.update_ms = 9_000;
    }
    let mut items = per_rep(&scratch, 5);
    items.push("previous=v1".to_owned());
    let operator = Operator::new(Rc::clone(&fleet));
    golden("update-rollback", &items, &fleet, operator);
}

#[test]
fn the_drift_plan_on_one_simulated_host_is_its_golden() {
    let scratch = support::scratch("golden-drift");
    let hosts = vec![SimHost::new("node-a", 0, &[("hello", "v1")])];
    let fleet = fleet(hosts);
    {
        let mut f = fleet.borrow_mut();
        f.world.first_order_ms = 3_000;
        f.world.update_ms = 7_000;
    }
    let unhealthy = |fleet: &mut Fleet, now: u64| {
        fleet.at(
            now + 7_000,
            Box::new(move |w| {
                let host = w.host("node-a");
                host.extra_beats.push(now + 7_000);
                if let Some(s) = host.services.get_mut("hello") {
                    s.health = "HEALTH_UNHEALTHY".to_owned();
                }
            }),
        );
    };
    // The second change is one the coordinator cannot see: the watch times out.
    let operator = Operator::new(Rc::clone(&fleet))
        .then(unhealthy)
        .then(|_, _| {})
        .then(unhealthy);
    let mut items = per_rep(&scratch, 3);
    items.push("change=hello's running executable replaced by another build".to_owned());
    golden("drift", &items, &fleet, operator);
}

#[test]
fn the_coordinator_restart_plan_on_a_simulated_farm_is_its_golden() {
    let hosts = vec![
        SimHost::new("node-a", 0, &[("hostkeeper", "v1")]),
        SimHost::new("node-b", 5_000, &[("hostkeeper", "v1")]),
        SimHost::new("node-c", 10_000, &[("hostkeeper", "v1")]),
    ];
    let fleet = fleet(hosts);
    let restart = |gone: u64, back: u64| {
        move |fleet: &mut Fleet, now: u64| fleet.world.down.push((now + gone, now + back))
    };
    let operator = Operator::new(Rc::clone(&fleet))
        .then(restart(3_000, 11_000))
        .then(restart(2_000, 6_000))
        .then(restart(4_000, 21_000));
    golden("coordinator-restart", &[], &fleet, operator);
}

#[test]
fn the_enrollment_plan_with_a_simulated_new_host_is_its_golden() {
    let fleet = fleet(vec![SimHost::new("node-a", 0, &[("hello", "v1")])]);
    let enroll = |fleet: &mut Fleet, now: u64| {
        fleet
            .world
            .audit_at(now, "cert_renewed", Some("node-a"), "coordinator");
        let at = |ms: u64, action: &'static str, node: Option<&'static str>, by: &'static str| {
            Box::new(move |w: &mut crate::support::World| w.audit_at(now + ms, action, node, by))
                as crate::support::Change
        };
        fleet.at(
            now + 12_000,
            at(12_000, "enroll_requested", None, "coordinator"),
        );
        fleet.at(
            now + 49_000,
            at(49_000, "enroll_approve", Some("node-new"), "operator"),
        );
        fleet.at(
            now + 51_000,
            at(51_000, "enroll_issued", Some("node-new"), "coordinator"),
        );
        let mut host = SimHost::new("node-new", now + 53_000, &[("hello", "v1")]);
        host.from_ms = now + 53_000;
        host.first_heartbeat_ms = Some(now + 53_000);
        host.first_serving_ms = Some(now + 83_000);
        fleet.world.hosts.push(host);
    };
    let operator = Operator::new(Rc::clone(&fleet)).then(enroll);
    golden("enrollment", &[], &fleet, operator);
}

#[test]
fn the_mac_enrollment_plan_with_a_simulated_mac_is_its_golden() {
    let fleet = fleet(vec![SimHost::new("node-a", 0, &[("hello", "v1")])]);
    fleet
        .borrow_mut()
        .world
        .devices
        .push(("device-old".to_owned(), Some(1_700_000_000)));
    let erase = |fleet: &mut Fleet, now: u64| {
        let secs = (support::T0_MS + now) / 1000;
        fleet.at(
            now + 300_000,
            Box::new(move |w| w.devices.push(("device-new".to_owned(), Some(secs + 300)))),
        );
        let at = |ms: u64, action: &'static str, node: Option<&'static str>, by: &'static str| {
            Box::new(move |w: &mut crate::support::World| w.audit_at(now + ms, action, node, by))
                as crate::support::Change
        };
        fleet.at(
            now + 330_000,
            at(330_000, "mdm_install_sent", None, "mdm-gate"),
        );
        // The gate pushes the nonce before the daemon's request is read: the watches do not
        // depend on the order of the entries.
        fleet.at(
            now + 404_000,
            at(404_000, "mdm_nonce_pushed", None, "mdm-gate"),
        );
        fleet.at(
            now + 405_000,
            at(405_000, "enroll_requested", None, "coordinator"),
        );
        fleet.at(
            now + 405_500,
            at(405_500, "enroll_approve", Some("node-other"), "operator"),
        );
        fleet.at(
            now + 406_000,
            at(406_000, "enroll_approve", Some("node-mac"), "mdm-nonce"),
        );
        fleet.at(
            now + 410_000,
            at(410_000, "enroll_issued", Some("node-mac"), "coordinator"),
        );
        let mut host = SimHost::new("node-mac", now + 416_000, &[("hello", "v1")]);
        host.from_ms = now + 416_000;
        host.first_heartbeat_ms = Some(now + 416_000);
        host.first_serving_ms = Some(now + 441_000);
        fleet.world.hosts.push(host);
    };
    let operator = Operator::new(Rc::clone(&fleet)).then(erase);
    golden("enrollment-mac", &[], &fleet, operator);
}
