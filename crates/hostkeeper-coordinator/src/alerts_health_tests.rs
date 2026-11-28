//! `storage_failing` of a nearly full filesystem (`alerts_health.rs`): the two thresholds and
//! their boundaries, no cordon at any level, the alert ended under the threshold, and no word on
//! fullness from a heartbeat with no filesystems.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{HealthAlert, storage_failing, used_percent};

/// A filesystem mounted at `mount` of 100 GB, `used` hundredths of a percent of it used.
fn filesystem(mount: &str, used: u64) -> agent::FilesystemUse {
    let size: u64 = 100_000_000_000;
    agent::FilesystemUse {
        mount: mount.to_owned(),
        size_bytes: size,
        available_bytes: size - size / 10_000 * used,
    }
}

/// A health that reports `filesystems` and holds no hardware record.
fn reporting(filesystems: Vec<agent::FilesystemUse>) -> agent::HostHealth {
    agent::HostHealth {
        boot_id: "b7c1".to_owned(),
        filesystems,
        ..agent::HostHealth::default()
    }
}

/// The `storage_failing` alert at `severity` of the first filesystem `health` reports.
fn root_alert(severity: proto::AlertSeverity, health: &agent::HostHealth) -> HealthAlert {
    let root = &health.filesystems[0];
    HealthAlert {
        kind: proto::AlertKind::StorageFailing,
        severity,
        finding: "filesystem_full/".to_owned() + &root.mount,
        message: format!(
            "filesystem {} is {}% full: {} of {} bytes available",
            root.mount,
            used_percent(root).expect("a size"),
            root.available_bytes,
            root.size_bytes
        ),
        cordons: false,
    }
}

/// Proves a filesystem 97% used raises a critical `storage_failing`, which does not cordon the
/// host, and is no hardware record: the health it is derived from holds none.
///
/// Catches: a full disk that cordons its host (its jobs drained for a disk to clean), or a
/// critical fullness read as a warning.
#[test]
fn a_filesystem_97_percent_used_is_critical_and_cordons_nothing() {
    let health = reporting(vec![filesystem("/", 9_700)]);
    let alerts = storage_failing(&health).expect("a word on fullness");
    assert_eq!(
        alerts,
        [root_alert(proto::AlertSeverity::Critical, &health)]
    );
    assert_eq!(
        alerts[0].message,
        "filesystem / is 97% full: 3000000000 of 100000000000 bytes available"
    );
    assert!(alerts.iter().all(|alert| !alert.cordons));
    assert!(health.events.is_empty());
}

/// Proves a filesystem 91% used raises a warning `storage_failing`, which does not cordon either.
#[test]
fn a_filesystem_91_percent_used_is_a_warning() {
    let health = reporting(vec![filesystem("/", 9_100)]);
    assert_eq!(
        storage_failing(&health),
        Some(vec![root_alert(proto::AlertSeverity::Warning, &health)])
    );
}

/// Proves a filesystem 80% used raises nothing, and a health with it is a word on fullness: no
/// alert, so one raised before ends.
#[test]
fn a_filesystem_80_percent_used_raises_nothing() {
    assert_eq!(
        storage_failing(&reporting(vec![filesystem("/", 8_000)])),
        Some(Vec::new())
    );
}

/// Proves the boundaries of the two thresholds: exactly 90% is a warning and just under it
/// nothing; exactly 97% critical and just under it a warning; a full filesystem critical.
#[test]
fn each_threshold_is_reached_at_exactly_its_percent() {
    let severity = |used: u64| {
        storage_failing(&reporting(vec![filesystem("/", used)]))
            .expect("a word")
            .first()
            .map(|alert| alert.severity)
    };
    assert_eq!(severity(8_999), None);
    assert_eq!(severity(9_000), Some(proto::AlertSeverity::Warning));
    assert_eq!(severity(9_699), Some(proto::AlertSeverity::Warning));
    assert_eq!(severity(9_700), Some(proto::AlertSeverity::Critical));
    assert_eq!(severity(10_000), Some(proto::AlertSeverity::Critical));
}

/// Proves the alert ends at the first heartbeat under the threshold, with no margin: 90% raises,
/// 89% the heartbeat after says nothing is full.
#[test]
fn an_alert_ends_at_the_first_heartbeat_under_its_threshold() {
    let raised = storage_failing(&reporting(vec![filesystem("/", 9_000)])).expect("a word");
    assert_eq!(raised.len(), 1);
    let after = storage_failing(&reporting(vec![filesystem("/", 8_900)]));
    assert_eq!(after, Some(Vec::new()));
}

/// Proves a heartbeat with no filesystems (a daemon of the release before, which sends none, or a
/// heartbeat that left them out to keep its bound) says nothing of fullness: no alert raised, and
/// none to end.
///
/// Catches: a host whose disk is 99% full read as cleared by the heartbeat that left its
/// filesystems out, or an older daemon's host alerted.
#[test]
fn a_heartbeat_with_no_filesystems_says_nothing() {
    assert_eq!(storage_failing(&agent::HostHealth::default()), None);
    let older = agent::HostHealth {
        boot_id: "b7c1".to_owned(),
        sources: vec![agent::SourceState {
            source: "smart".to_owned(),
            status: agent::SourceStatus::Ok.into(),
            reason: String::new(),
        }],
        ..agent::HostHealth::default()
    };
    assert_eq!(storage_failing(&older), None);
}

/// Proves each filesystem is its own alert, by its mount, in the order the health lists them, and
/// those under the threshold none; a filesystem of no size, or one whose available bytes say more
/// than its size, is not full.
#[test]
fn each_filesystem_is_its_own_alert() {
    let empty = agent::FilesystemUse {
        mount: "/mnt/empty".to_owned(),
        size_bytes: 0,
        available_bytes: 0,
    };
    let odd = agent::FilesystemUse {
        mount: "/mnt/odd".to_owned(),
        size_bytes: 10,
        available_bytes: 20,
    };
    let health = reporting(vec![
        filesystem("/", 5_000),
        filesystem("/var/lib/js-agent", 9_800),
        empty.clone(),
        odd.clone(),
        filesystem("/boot/efi", 9_200),
    ]);
    let alerts = storage_failing(&health).expect("a word");
    let found: Vec<(&str, proto::AlertSeverity, bool)> = alerts
        .iter()
        .map(|alert| (alert.finding.as_str(), alert.severity, alert.cordons))
        .collect();
    assert_eq!(
        found,
        [
            (
                "filesystem_full//var/lib/js-agent",
                proto::AlertSeverity::Critical,
                false
            ),
            (
                "filesystem_full//boot/efi",
                proto::AlertSeverity::Warning,
                false
            ),
        ]
    );
    assert_eq!(used_percent(&empty), None);
    assert_eq!(used_percent(&odd), Some(0));
}
