//! The daemon's Sigstore trusted root in its heartbeat, end to end: the package store's report of
//! a real root file aged on a fake unix clock, the real heartbeat loop over the daemon's ports,
//! and the real coordinator in process, whose `Hosts.Get` and `Alerts.List` an operator reads.

mod support;

use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_coordinator::operator::Caller;
use hostkeeper_coordinator::service::Coordinator;
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::cli::DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS;
use hostkeeper_daemon::packages::{PackageError, Packages, VersionDir};
use hostkeeper_daemon::trusted_root::{PACKAGED_TRUSTED_ROOT, RootReport, TrustedRoot};
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_link::testing::FakeClock;
use hostkeeper_proto::hostkeeper::agent::v1::TrustedRootReason;
use hostkeeper_proto::hostkeeper::operator::v1::{
    AlertKind, AlertSeverity, GetHostRequest, ListAlertsRequest,
};
use support::coordinated::{Host, coordinator_of};
use support::{OWN, Rig, config};

/// Seconds in a day.
const DAY: u64 = 86_400;

/// When the root file was written, in unix seconds.
const WRITTEN: u64 = 1_800_000_000;

/// A root as `gh attestation trusted-root` writes it.
const ROOT: &str =
    "{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json;version=0.1\",\"tlogs\":[]}\n";

/// The rig's package store, whose trusted root is `root`, read at each call as the real store
/// reads it.
struct Rooted {
    inner: Arc<hostkeeper_daemon::testing::FakePackages>,
    root: TrustedRoot,
}

impl Packages for Rooted {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        digest: &PackageDigest,
        index_digest: Option<&PackageDigest>,
    ) -> Result<VersionDir, PackageError> {
        self.inner
            .version_dir(service, version, digest, index_digest)
    }

    fn trusted_root(&self) -> Option<RootReport> {
        Some(self.root.report())
    }
}

/// Writes `text` at `path`, its modification time set to `at` unix seconds.
fn write_at(path: &Path, text: &str, at: u64) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("make the parent");
    fs::write(path, text).expect("write the root");
    File::options()
        .write(true)
        .open(path)
        .and_then(|file| {
            file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(at)))
        })
        .expect("set the modification time");
}

/// What `Hosts.Get` answers of a host's trusted root: its age and its reason.
type RootSaid = (Option<u64>, TrustedRootReason);

/// A `trusted_root` alert: its host, its severity, and whether its message names the reason.
type RootAlert = (String, AlertSeverity, bool);

/// What `Hosts.Get` answers of `host-1`'s trusted root, and the `trusted_root` alerts of
/// `Alerts.List`, each one's message checked for the reason `word`.
fn heard(coordinator: &Coordinator, word: &str) -> (RootSaid, Vec<RootAlert>) {
    let operator = Caller::Operator("operator".parse().expect("a token name"));
    let request = GetHostRequest {
        node_id: "host-1".to_owned(),
    };
    let host = coordinator
        .get_host(&operator, &request)
        .expect("Hosts.Get")
        .host
        .expect("a host");
    let alerts = coordinator
        .list_alerts(&operator, ListAlertsRequest {})
        .expect("Alerts.List")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind() == AlertKind::TrustedRoot)
        .map(|alert| {
            let names = alert.message.contains(&format!("trusted root {word} "));
            (alert.host_id.clone(), alert.severity(), names)
        })
        .collect();
    (
        (host.trusted_root_age_secs, host.trusted_root_reason()),
        alerts,
    )
}

/// Catches a daemon whose heartbeat leaves out the report of its trusted root, reads it once
/// and keeps it, or sends another reason or age than the report's, and a coordinator that does
/// not answer it or alert on it: with the packaged root 91 days old on the daemon's clock (the
/// limit is 90), `Hosts.Get` answers the age 91 days and the reason stale, and `Alerts.List`
/// one warning `trusted_root` alert of `host-1` that says stale; once a new root is written at
/// the clock's second, the next heartbeat says age 0 and no reason, and the alert is gone; once
/// both files are gone, it says missing, with no age.
#[test]
fn a_daemon_with_a_stale_root_reports_its_age_and_reason_to_the_coordinator() {
    let rig = Rig::new("trusted-root-beat");
    let coordinator = coordinator_of(rig.scratch.path(), "web", "1.0");
    let packaged = rig.packages.join(PACKAGED_TRUSTED_ROOT);
    let installed = rig
        .scratch
        .path()
        .join("usr/lib/hostkeeper/trusted_root.json");
    write_at(&packaged, ROOT, WRITTEN);
    let now = WRITTEN + 91 * DAY;
    let clock = Arc::new(FakeClock::new(now, 7));
    let root = TrustedRoot::new(
        packaged.clone(),
        installed,
        DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS.get(),
        Arc::clone(&clock) as Arc<dyn hostkeeper_link::UnixClock>,
    );
    let ports = hostkeeper_daemon::Ports {
        packages: Arc::new(Rooted {
            inner: Arc::clone(&rig.store),
            root,
        }),
        ..rig.ports()
    };
    let daemon = Arc::new(Daemon::new(config(), rig.dirs(), ports, OWN));
    let mut host = Host::of(daemon, &coordinator, None);

    host.beat();
    assert_eq!(
        heard(&coordinator, "stale"),
        (
            (Some(91 * DAY), TrustedRootReason::Stale),
            vec![("host-1".to_owned(), AlertSeverity::Warning, true)]
        )
    );

    write_at(&packaged, ROOT, now);
    host.beat();
    assert_eq!(
        heard(&coordinator, "stale"),
        ((Some(0), TrustedRootReason::None), Vec::new())
    );

    fs::remove_file(&packaged).expect("remove the root");
    host.beat();
    assert_eq!(
        heard(&coordinator, "missing"),
        (
            (None, TrustedRootReason::Missing),
            vec![("host-1".to_owned(), AlertSeverity::Warning, true)]
        )
    );
}

/// Catches a daemon whose store keeps no root read by the coordinator as a root that needs no
/// attention: its heartbeat says nothing of the root, `Hosts.Get` answers no age and the reason
/// unknown, and there is no `trusted_root` alert.
#[test]
fn a_daemon_whose_store_keeps_no_root_is_unknown_to_the_coordinator() {
    let rig = Rig::new("trusted-root-beat-none");
    let coordinator = coordinator_of(rig.scratch.path(), "web", "1.0");
    let mut host = Host::of(Arc::new(rig.daemon()), &coordinator, None);
    host.beat();
    assert_eq!(
        heard(&coordinator, "stale"),
        ((None, TrustedRootReason::Unspecified), Vec::new())
    );
}

/// Catches a problem of the report sent as another reason, and an age dropped or changed on the
/// way into the loop's words: each problem is its own reason, no problem is no reason, and the
/// age is the report's, none when the report has none.
#[test]
fn the_report_of_the_root_is_the_heartbeats_age_and_reason() {
    use hostkeeper_daemon::link_adapters::root_facts;
    use hostkeeper_daemon::trusted_root::RootProblem;
    use hostkeeper_link::{RootReason, TrustedRootFacts};

    let cases = [
        (
            Some(91 * DAY),
            Some(RootProblem::Stale),
            Some(RootReason::Stale),
        ),
        (None, Some(RootProblem::Missing), Some(RootReason::Missing)),
        (
            Some(5),
            Some(RootProblem::Unparseable),
            Some(RootReason::Unparseable),
        ),
        (Some(0), None, None),
    ];
    for (age_secs, problem, reason) in cases {
        let report = RootReport {
            path: "trusted_root.json".into(),
            age_secs,
            problem,
        };
        assert_eq!(
            root_facts(&report),
            TrustedRootFacts { age_secs, reason },
            "{problem:?}"
        );
    }
}
