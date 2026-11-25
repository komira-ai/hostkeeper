//! The host's last probe and the kinds run only when named, in the heartbeat's maintenance report
//! (`maintenance_state::RunReported`, `on_demand_of`, `last_probe`): the probe file beside the run
//! record, as the `probe` kind writes it, is carried whole while it keeps the report's bounds and
//! left out when it does not read or breaks one, and a Mac's daemon, alone, lists `probe` on
//! demand. No program runs and no clock is read.

mod support;

use std::fs;

use hostkeeper_daemon::maintenance_hook::probe::{PROBE_FILE, write_probe};
use hostkeeper_daemon::maintenance_state::{LookAgain, RunReported, last_probe, on_demand_of};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::MaintenanceSource;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::{MAX_PROBE_OUTPUT, check_report};
use hostkeeper_steps::maintenance::MaintenanceStore;
use support::Scratch;

/// The time of the last look the fake facts report.
const LOOKED: u64 = 1_700_000_000;

/// Facts that report a fixed look.
struct Facts;

impl MaintenanceSource for Facts {
    fn report(&self) -> pb::MaintenanceReport {
        pb::MaintenanceReport {
            looked_unix: LOOKED,
            ..pb::MaintenanceReport::default()
        }
    }
}

impl LookAgain for Facts {
    fn look_again(&self) {}
}

/// A daemon's state directory in `scratch`, and the run record's directory beside it, made.
fn dirs(scratch: &Scratch) -> (std::path::PathBuf, std::path::PathBuf) {
    let state = scratch.path().join("state");
    fs::create_dir_all(&state).expect("the state directory");
    let maintenance = MaintenanceStore::beside(&state).dir().to_path_buf();
    fs::create_dir_all(&maintenance).expect("the record's directory");
    (state, maintenance)
}

/// A probe of run `run_id` whose one step wrote `output`.
fn probe(run_id: &str, output: &str) -> pb::MaintenanceProbe {
    pb::MaintenanceProbe {
        run_id: run_id.to_owned(),
        probed_unix: 1_800_000_000,
        steps: vec![pb::MaintenanceProbeStep {
            name: "console".to_owned(),
            exit_code: Some(0),
            output: output.to_owned(),
            cut: false,
        }],
    }
}

/// Proves: a Mac's daemon lists `probe` as run only when named, and a Linux host's (systemd or
/// OpenRC) lists nothing; the report carries the list as given, beside the facts' own fields.
///
/// Catches: a Linux host listed for a probe it has no work for (the coordinator would order it,
/// and its hook would halt it drained with `unsupported_kind`), or a Mac not listed, which no
/// probe could reach.
#[test]
fn a_mac_alone_lists_probe_on_demand() {
    assert_eq!(
        on_demand_of(PlatformKind::Macos),
        [pb::MaintenanceKind::Probe]
    );
    assert!(on_demand_of(PlatformKind::Linux).is_empty());
    assert!(on_demand_of(PlatformKind::Openrc).is_empty());
    let scratch = Scratch::new("probe-on-demand");
    let (state, _) = dirs(&scratch);
    let mac = RunReported::new(Facts, &state, on_demand_of(PlatformKind::Macos)).report();
    assert_eq!(mac.on_demand, [i32::from(pb::MaintenanceKind::Probe)]);
    assert_eq!(mac.looked_unix, LOOKED);
    assert_eq!(mac.probe, None, "no probe yet");
    let linux = RunReported::new(Facts, &state, on_demand_of(PlatformKind::Linux)).report();
    assert!(linux.on_demand.is_empty());
}

/// Proves: the probe file the `probe` kind writes beside the run record is in every report from
/// then on, whole, and the next probe replaces it; the report keeps every bound.
///
/// Catches: a probe written where the report does not read it, read once and dropped, or an
/// older probe shown after a newer one.
#[test]
fn the_last_probe_is_in_every_report_until_the_next() {
    let scratch = Scratch::new("probe-reported");
    let (state, maintenance) = dirs(&scratch);
    let source = RunReported::new(Facts, &state, on_demand_of(PlatformKind::Macos));
    let first = probe("run-1", "root");
    write_probe(&maintenance.join(PROBE_FILE), &first).expect("written");
    assert_eq!(source.report().probe, Some(first.clone()));
    assert_eq!(source.report().probe, Some(first));
    let second = probe("run-2", "adi");
    write_probe(&maintenance.join(PROBE_FILE), &second).expect("written");
    let report = source.report();
    assert_eq!(report.probe, Some(second));
    assert_eq!(check_report(&report), Ok(()));
}

/// Proves: a probe file that breaks a bound of the report (an output one byte over), that is not
/// a probe, or that is not there is left out of the report, and the rest of the report is as the
/// facts give it.
///
/// Catches: a damaged probe file carried into the report, which the coordinator would refuse
/// whole, losing every maintenance fact of the host until the file is removed.
#[test]
fn a_probe_file_that_breaks_a_bound_or_does_not_read_is_left_out() {
    let scratch = Scratch::new("probe-damaged");
    let (state, maintenance) = dirs(&scratch);
    let path = maintenance.join(PROBE_FILE);
    let source = RunReported::new(Facts, &state, Vec::new());
    write_probe(&path, &probe("run-1", &"a".repeat(MAX_PROBE_OUTPUT + 1))).expect("written");
    assert_eq!(last_probe(&maintenance), None);
    let report = source.report();
    assert_eq!(report.probe, None);
    assert_eq!(report.looked_unix, LOOKED);
    assert_eq!(check_report(&report), Ok(()));
    write_probe(&path, &probe("run-1", &"a".repeat(MAX_PROBE_OUTPUT))).expect("written");
    assert!(
        last_probe(&maintenance).is_some(),
        "at its bound it is carried"
    );
    fs::write(&path, [0xff, 0xff]).expect("write");
    assert_eq!(last_probe(&maintenance), None);
    fs::remove_file(&path).expect("remove");
    assert_eq!(last_probe(&maintenance), None);
    assert_eq!(last_probe(&scratch.path().join("absent")), None);
}
