//! A probe of a Mac end to end (docs/operations/maintenance.md, "Probe: drain, read, put back";
//! docs/operations/mac-proof.md, section 1): the operator's one `Maintenance.Schedule` of `probe`
//! for one Mac, and nothing else from the operator; the real coordinator orders the Mac's turn
//! from its heartbeats; the real daemon drains its service, runs the real maintenance hook's
//! `probe` kind over a fake Mac (`support_mac`), checks, puts the service back; and the Mac's next
//! report carries what the probe read, which the operator reads with `Hosts.Get`. Each test ends
//! with exact equality on what the fake Mac went through and on the probe the operator reads.

mod support;
mod support_e2e;
mod support_mac;
mod support_maintain;

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use op::MaintenanceHostStep as Step;
use support_mac::{
    ENV, FDESETUP, ID, LAUNCHCTL, Mac, Node, PROBED_UNIX, SERVICE, STAT, SW_VERS, WHO, World,
    first_offer, one_at_a_time,
};

/// The group of every test.
const GROUP: &str = "macs";

/// `Hosts.Get`, as the operator calls it.
const HOSTS_GET: &str = "/hostkeeper.operator.v1.Hosts/Get";

/// `program` run with `words`, as an effect line.
fn ran(program: &str, words: &[&str]) -> String {
    let mut line = vec![program];
    line.extend_from_slice(words);
    line.join(" ")
}

/// A step of the probe as the report carries it, exit 0.
fn step(name: &str, output: &str) -> pb::MaintenanceProbeStep {
    pb::MaintenanceProbeStep {
        name: name.to_owned(),
        exit_code: Some(0),
        output: output.to_owned(),
        cut: false,
    }
}

/// The probe of `h1` as `Hosts.Get` answers it to the operator.
fn probe_read_by_the_operator(world: &World, host: &str) -> Option<pb::MaintenanceProbe> {
    let answer: op::GetHostResponse = world
        .rpc(
            HOSTS_GET,
            &op::GetHostRequest {
                node_id: host.to_owned(),
            },
        )
        .expect("the host");
    answer.host.and_then(|host| host.maintenance)?.probe
}

/// Proves: one `Schedule` of `probe` for one Mac, with nothing due on it, is the whole of the
/// operator's part: the coordinator orders the Mac a turn of `probe` alone; the daemon stages
/// nothing, drains its service (js-agent's cordon on a farm Mac), runs the hook's `cutover`, in
/// which the probe's eight programs run in order and nothing else (no install, no restart),
/// checks, and puts the service back; the run ends done, the Mac is back in service, and the
/// operator's `Hosts.Get` of the Mac shows the probe the run made, each program with what it
/// wrote; the other Mac of the group is not touched.
///
/// Catches: a probe that runs while the Mac still takes work (no drain), leaves it out of work
/// (no resume), installs or restarts anything, or whose result never reaches the operator.
#[test]
fn one_schedule_drains_the_mac_probes_it_and_puts_it_back() {
    let test = "mac-probe-e2e";
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2"], one_at_a_time());
    let mut h1 = Node::mac(&world, test, "h1", GROUP, Mac::up_to_date());
    let mut h2 = Node::mac(&world, test, "h2", GROUP, Mac::up_to_date());
    h1.report();
    h2.report();
    assert_eq!(
        h1.last_report().on_demand,
        [i32::from(pb::MaintenanceKind::Probe)]
    );
    assert_eq!(probe_read_by_the_operator(&world, "h1"), None);

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        kinds: vec![pb::MaintenanceKind::Probe.into()],
        host_ids: vec!["h1".to_owned()],
        ..op::ScheduleMaintenanceRequest::default()
    });
    let order = first_offer(&run, &[pb::MaintenanceKind::Probe]);
    assert_eq!(h1.beat(), Some(order.clone()));
    for _ in 0..3 {
        assert_eq!(h1.beat(), None);
    }

    assert_eq!(
        h1.machine.effects(),
        [
            "prepare".to_owned(),
            format!("drain {SERVICE}"),
            "cutover".to_owned(),
            ran(STAT, &["-f", "%Su", "/dev/console"]),
            ran(WHO, &[]),
            ran(ID, &[]),
            ran(LAUNCHCTL, &["managername"]),
            ran(ENV, &[]),
            ran(SW_VERS, &[]),
            ran(FDESETUP, &["status"]),
            ran(FDESETUP, &["supportsauthrestart"]),
            "verify".to_owned(),
            format!("resume {SERVICE}"),
        ]
    );
    assert!(h1.machine.inputs().is_empty(), "nothing installed");
    assert!(!h1.machine.restart_asked(), "no restart");
    assert!(!h1.machine.cordoned(), "the service is back in service");
    assert_eq!(world.steps(&run), [("h1".to_owned(), Step::Done)]);
    let probe = pb::MaintenanceProbe {
        run_id: run.clone(),
        probed_unix: PROBED_UNIX,
        steps: vec![
            step("console", "root"),
            step("who", ""),
            step("id", "uid=0(root) gid=0(wheel) groups=0(wheel)"),
            step("managername", "System"),
            step("env", ""),
            step(
                "sw_vers",
                "ProductName:\t\tmacOS; ProductVersion:\t\t26.6; BuildVersion:\t\t25G62",
            ),
            step("fdesetup_status", "FileVault is Off."),
            step("fdesetup_supportsauthrestart", "true"),
        ],
    };
    assert_eq!(h1.last_report().probe, Some(probe.clone()));
    assert_eq!(probe_read_by_the_operator(&world, "h1"), Some(probe));
    assert_eq!(h1.orders(), [order]);
    assert_eq!(h2.beat(), None);
    assert_eq!(h2.machine.effects(), Vec::<String>::new());
}
