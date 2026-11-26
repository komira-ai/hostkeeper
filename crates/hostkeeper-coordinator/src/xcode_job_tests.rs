//! The job `xcode-feed` over a coordinator: the list read from a fake origin, kept, and compared
//! with what the Xcode store holds by the alert `xcode_release_waiting`; a read that fails; the
//! job's loop over files; and a coordinator with no feed. The parse of the list is in
//! `xcode_feed_tests.rs`; the job over a database is in `store/postgres/tests/xcode_jobs.rs`.

use std::io::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_oidc::Dial;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::audit_actions::XCODE_REMOVE;
use crate::host_groups_support::{SELF, World};
use crate::jobs::periodic;
use crate::marks_support::{operator, start};
use crate::ops::AuditEntry;
use crate::repository_origin::RepositoryOrigin;
use crate::repository_origin_support::{FakeOrigin, Then, refusing_dial, response};
use crate::service::Coordinator;
use crate::testutil::{Log, Rig, scratch};
use crate::xcode_feed::{
    Channel, FEED_EVERY, FeedError, FeedUrl, KnownReleases, MemoryReleases, XcodeHeld, XcodeId,
    XcodeRelease, XcodeVersion, read_every,
};
use crate::xcode_store::{Archive, Digesting, XcodeStore};

/// A copy of the 12 newest entries of the Xcode Releases site's `data.json`: the newest release is
/// 27.2 (27B52).
const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/xcode/data.json");

/// The address of the list at the fake origin.
const FEED: &str = "https://origin.test/data.json";

/// The message of the alert for Xcode 27.2 (27B52) when the store holds nothing as new.
const WAITING_27_2: &str = "Xcode 27.2 (27B52), which requires macOS 26.6 or later, is released and the coordinator's Xcode store holds no release as new: add it from a Mac signed in to the Apple developer account with `hk xcode add 27.2`";

/// The Xcode `number` of `build`.
fn id(number: &str, build: &str) -> XcodeId {
    XcodeId {
        version: XcodeVersion::parse(number).expect("a version in a test"),
        build: build.to_owned(),
    }
}

/// A release of `channel` requiring macOS 26.6.
fn release(number: &str, build: &str, channel: Channel) -> XcodeRelease {
    XcodeRelease {
        id: id(number, build),
        channel,
        requires_macos: "26.6".to_owned(),
    }
}

/// The list kept in memory, counting how many times it was replaced.
#[derive(Default)]
struct Counting {
    list: MemoryReleases,
    keeps: AtomicUsize,
}

impl KnownReleases for Counting {
    fn keep(&self, releases: &[XcodeRelease]) -> Result<(), String> {
        self.list.keep(releases)?;
        self.keeps.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        self.list.known()
    }
}

/// An Xcode store whose releases the test sets.
#[derive(Default)]
struct Held(Mutex<Vec<XcodeId>>);

impl Held {
    fn set(&self, ids: Vec<XcodeId>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = ids;
    }
}

impl XcodeHeld for Held {
    fn held(&self) -> Result<Vec<XcodeId>, String> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }
}

/// A coordinator over files whose list is at the fake origin (or nowhere), with what it logs.
struct Farm {
    /// Ends the capture of the log: it is dropped before the rig's.
    _guard: tracing::subscriber::DefaultGuard,
    rig: Rig,
    fake: FakeOrigin,
    known: Arc<Counting>,
    held: Arc<Held>,
    log: Log,
}

/// A farm named `test` reading the list at `feed`, over the dial `dial` makes of the fake origin.
fn farm(test: &str, feed: Option<&str>, dial: impl FnOnce(&FakeOrigin) -> Dial) -> Farm {
    let mut rig = Rig::new(test);
    let log = Log::default();
    let guard = log.install();
    let fake = FakeOrigin::new();
    let origin = RepositoryOrigin::new(fake.roots())
        .expect("the origin client")
        .with_dial(dial(&fake));
    let known = Arc::new(Counting::default());
    let held = Arc::new(Held::default());
    let coordinator = rig.coordinator;
    rig.coordinator = coordinator
        .with_repository_origin(origin)
        .with_xcode_feed(feed.map(|text| FeedUrl::parse(text).expect("a feed address")))
        .with_known_xcode_releases(Arc::clone(&known) as Arc<dyn KnownReleases>)
        .with_xcode_held(Arc::clone(&held) as Arc<dyn XcodeHeld>);
    Farm {
        _guard: guard,
        rig,
        fake,
        known,
        held,
        log,
    }
}

/// The dial of an origin that answers 200 with `body` to every request.
fn serving(body: Vec<u8>) -> impl FnOnce(&FakeOrigin) -> Dial {
    move |fake| fake.dial_all(Then::Write(response("200 OK", &body)))
}

/// The alerts `xcode_release_waiting` of the coordinator after one more observation.
fn waiting_alerts(coordinator: &Coordinator) -> Vec<proto::Alert> {
    coordinator
        .observe_alerts()
        .expect("the alerts are observed");
    coordinator
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("the alerts are listed")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind() == proto::AlertKind::XcodeReleaseWaiting)
        .collect()
}

/// The line the log holds for a read of the list that failed.
fn failure_line(log: &Log) -> String {
    log.text()
        .lines()
        .find(|line| line.contains("the list of Xcode releases could not be read;"))
        .unwrap_or_default()
        .to_owned()
}

/// Lets every task that can run, run.
async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

/// Waits, yielding, until `count` is at least `n`.
async fn until(count: &AtomicUsize, n: usize) {
    while count.load(Ordering::SeqCst) < n {
        tokio::task::yield_now().await;
    }
}

/// Proves: after the job reads the real list, with an Xcode store that holds nothing, the
/// coordinator's alerts hold exactly one `xcode_release_waiting`, a warning for version 27.2 whose
/// message is the one that names 27.2 (27B52), the macOS it requires and the verb that pushes it;
/// the 12 releases read are kept.
///
/// Catches: a job that reads the list and keeps it where the alerts do not look, an alert for a
/// beta or a release candidate (27.3 is newer than 27.2), and a list that is not parsed whole.
#[tokio::test]
async fn a_new_release_with_nothing_held_raises_one_warning_naming_it() {
    let farm = farm("xcode-job-new", Some(FEED), serving(FIXTURE.to_vec()));
    assert_eq!(farm.rig.coordinator.read_xcode_feed().await, Ok(12));
    assert_eq!(farm.known.known().expect("the list").len(), 12);
    let alerts = waiting_alerts(&farm.rig.coordinator);
    let [alert] = alerts.as_slice() else {
        panic!("one alert: {alerts:?}");
    };
    assert_eq!(alert.severity(), proto::AlertSeverity::Warning);
    assert_eq!(alert.version, "27.2");
    assert_eq!(alert.message, WAITING_27_2);
}

/// Proves: when the Xcode store already holds 27.2 (27B52), reading the same list raises no
/// `xcode_release_waiting`.
///
/// Catches: an alert for a release the store holds.
#[tokio::test]
async fn a_release_the_store_holds_raises_nothing() {
    let farm = farm("xcode-job-held", Some(FEED), serving(FIXTURE.to_vec()));
    farm.held.set(vec![id("27.2", "27B52")]);
    assert_eq!(farm.rig.coordinator.read_xcode_feed().await, Ok(12));
    assert_eq!(waiting_alerts(&farm.rig.coordinator), vec![]);
}

/// Proves: an alert raised for 27.2 (27B52) while the store holds nothing is gone at the next
/// observation after the store holds it.
///
/// Catches: an alert that stays after the store holds the release.
#[tokio::test]
async fn the_alert_clears_when_the_store_holds_the_release() {
    let farm = farm("xcode-job-clears", Some(FEED), serving(FIXTURE.to_vec()));
    assert_eq!(farm.rig.coordinator.read_xcode_feed().await, Ok(12));
    assert_eq!(waiting_alerts(&farm.rig.coordinator).len(), 1);
    farm.held.set(vec![id("27.2", "27B52")]);
    assert_eq!(waiting_alerts(&farm.rig.coordinator), vec![]);
}

/// Proves: a list whose only entries newer than the release the store holds (27.1, 27A9275) are a
/// beta and a release candidate raises no `xcode_release_waiting`.
///
/// Catches: a beta or a release candidate counted as a release that is waiting.
#[tokio::test]
async fn only_betas_and_release_candidates_newer_than_the_held_release_raise_nothing() {
    let entry = |number: &str, build: &str, kind: &str| {
        serde_json::json!({
            "version": {"number": number, "build": build, "release": {(kind): true}},
            "requires": "26.6",
        })
    };
    let list = serde_json::to_vec(&[
        entry("27.2", "27B5038j", "beta"),
        entry("27.2", "27B48", "rc"),
        entry("27.1", "27A9275", "release"),
    ])
    .expect("a list as JSON");
    let farm = farm("xcode-job-beta", Some(FEED), serving(list));
    farm.held.set(vec![id("27.1", "27A9275")]);
    assert_eq!(farm.rig.coordinator.read_xcode_feed().await, Ok(3));
    assert_eq!(waiting_alerts(&farm.rig.coordinator), vec![]);
}

/// Seeds the list `farm` kept before with the release 27.1 (27A9275), which the store holds, and
/// runs the job: what it kept stands, and nothing is alerted.
async fn failed_job_keeps_the_list(farm: &Farm) -> String {
    let before = vec![release("27.1", "27A9275", Channel::Release)];
    farm.known.keep(&before).expect("the list is kept");
    farm.held.set(vec![id("27.1", "27A9275")]);
    farm.rig.coordinator.xcode_feed_job().await;
    assert_eq!(farm.known.known(), Ok(before));
    assert_eq!(farm.known.keeps.load(Ordering::SeqCst), 1);
    assert_eq!(waiting_alerts(&farm.rig.coordinator), vec![]);
    assert!(!farm.log.text().contains("ERROR"), "{}", farm.log.text());
    failure_line(&farm.log)
}

/// Proves: an answer that is not JSON is a warning that names the parse failure and the address,
/// the list kept before stands, and no alert is raised; the read itself returns the failure.
///
/// Catches: a list replaced by one that was not read, a failed read that is an error line or an
/// alert, and a warning that does not say why.
#[tokio::test]
async fn a_body_that_is_not_a_list_is_a_warning_and_keeps_the_list() {
    let farm = farm(
        "xcode-job-garbage",
        Some(FEED),
        serving(b"<html>no</html>".to_vec()),
    );
    let line = failed_job_keeps_the_list(&farm).await;
    assert!(line.contains("WARN"), "{line}");
    assert!(
        line.contains(
            "why=the list is not a JSON array of releases: expected value at line 1 column 1"
        ),
        "{line}"
    );
    assert!(line.contains("url=https://origin.test/data.json"), "{line}");
    assert_eq!(
        farm.rig.coordinator.read_xcode_feed().await,
        Err(FeedError::NotList(
            "expected value at line 1 column 1".to_owned()
        ))
    );
}

/// Proves: an answer of 500 is a warning that names the status and the address, the list kept
/// before stands, and no alert is raised.
///
/// Catches: a status that is read as a list, and a failed read that is an alert.
#[tokio::test]
async fn an_answer_of_500_is_a_warning_and_keeps_the_list() {
    let farm = farm("xcode-job-500", Some(FEED), |fake| {
        fake.dial_all(Then::Write(response("500 Internal Server Error", b"")))
    });
    let line = failed_job_keeps_the_list(&farm).await;
    assert!(line.contains("WARN"), "{line}");
    assert!(
        line.contains("why=the list could not be read: the origin answered 500"),
        "{line}"
    );
    assert!(line.contains("url=https://origin.test/data.json"), "{line}");
    assert_eq!(farm.fake.dials(), 1);
}

/// Proves: a connection that is refused is a warning that says the connection failed, the list
/// kept before stands, and no alert is raised.
///
/// Catches: a failed dial that ends the job loop or raises an alert.
#[tokio::test]
async fn a_refused_connection_is_a_warning_and_keeps_the_list() {
    let farm = farm("xcode-job-refused", Some(FEED), |_| refusing_dial());
    let line = failed_job_keeps_the_list(&farm).await;
    assert!(line.contains("WARN"), "{line}");
    assert!(
        line.contains("why=the list could not be read: the connection failed"),
        "{line}"
    );
    assert!(line.contains("url=https://origin.test/data.json"), "{line}");
}

/// Proves: the job over files reads the list at once, does not read it again before a day
/// ([`FEED_EVERY`]) has passed, and reads it again when the day has passed, each time keeping it.
///
/// Catches: a job that is never started over files, one that reads every tick of another job's
/// period, and one that reads once.
#[tokio::test(start_paused = true)]
async fn over_files_the_list_is_read_at_once_and_again_after_a_day() {
    let farm = farm("xcode-job-loop", Some(FEED), serving(FIXTURE.to_vec()));
    let task = tokio::spawn(periodic(Arc::new(farm.rig.coordinator)));
    until(&farm.known.keeps, 1).await;
    assert_eq!(farm.fake.dials(), 1);
    tokio::time::advance(FEED_EVERY - Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(farm.known.keeps.load(Ordering::SeqCst), 1);
    assert_eq!(farm.fake.dials(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    until(&farm.known.keeps, 2).await;
    assert_eq!(farm.fake.dials(), 2);
    task.abort();
}

/// Proves: without a feed address the coordinator does not read the list: the read returns 0 and
/// keeps nothing, the job's loop never dials, and `reads_xcode_feed` says so.
///
/// Catches: a read of a third party that no operator asked for.
#[tokio::test(start_paused = true)]
async fn without_a_feed_address_nothing_is_read() {
    let farm = farm("xcode-job-off", None, serving(FIXTURE.to_vec()));
    assert!(!farm.rig.coordinator.reads_xcode_feed());
    assert_eq!(farm.rig.coordinator.read_xcode_feed().await, Ok(0));
    assert_eq!(farm.known.known(), Ok(vec![]));
    assert_eq!(farm.known.keeps.load(Ordering::SeqCst), 0);
    let task = tokio::spawn(read_every(Arc::new(farm.rig.coordinator), FEED_EVERY));
    settle().await;
    tokio::time::advance(FEED_EVERY).await;
    settle().await;
    assert_eq!(farm.fake.dials(), 0);
    task.abort();
}

/// A list whose keep fails with `why`, or panics when `why` is `None`.
struct Failing(Option<&'static str>);

impl KnownReleases for Failing {
    fn keep(&self, _releases: &[XcodeRelease]) -> Result<(), String> {
        match self.0 {
            Some(why) => Err(why.to_owned()),
            None => panic!("the store's thread died"),
        }
    }

    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        Ok(vec![])
    }
}

/// A farm over `known`, serving the fixture.
fn farm_keeping(test: &str, known: Arc<dyn KnownReleases>) -> Farm {
    let mut farm = farm(test, Some(FEED), serving(FIXTURE.to_vec()));
    farm.rig.coordinator = farm.rig.coordinator.with_known_xcode_releases(known);
    farm
}

/// Proves: a coordinator with a feed address and no HTTPS client fails the read with a message
/// that says so, and the job logs a warning and no error.
///
/// Catches: a read that panics or silently succeeds without a client.
#[tokio::test]
async fn a_coordinator_without_an_https_client_fails_the_read() {
    let mut rig = Rig::new("xcode-job-no-client");
    let log = Log::default();
    let _guard = log.install();
    let coordinator = rig.coordinator;
    rig.coordinator =
        coordinator.with_xcode_feed(Some(FeedUrl::parse(FEED).expect("a feed address")));
    assert_eq!(
        rig.coordinator.read_xcode_feed().await,
        Err(FeedError::Read(
            "the coordinator has no HTTPS client".to_owned()
        ))
    );
    rig.coordinator.xcode_feed_job().await;
    assert!(!log.text().contains("ERROR"), "{}", log.text());
    assert!(failure_line(&log).contains("the coordinator has no HTTPS client"));
}

/// Proves: a store that refuses the list makes the read return its message as the failure, and
/// the job logs a warning and no error.
///
/// Catches: a failed keep that is dropped, or one that is an error line.
#[tokio::test]
async fn a_store_that_refuses_the_list_fails_the_read() {
    let farm = farm_keeping("xcode-job-refused", Arc::new(Failing(Some("disk full"))));
    assert_eq!(
        farm.rig.coordinator.read_xcode_feed().await,
        Err(FeedError::Keep("disk full".to_owned()))
    );
    farm.rig.coordinator.xcode_feed_job().await;
    assert!(!farm.log.text().contains("ERROR"), "{}", farm.log.text());
    assert!(failure_line(&farm.log).contains("disk full"));
}

/// Proves: a store whose write panics makes the read return a keep failure instead of taking the
/// job down.
///
/// Catches: a panic in the store's thread that reaches the job's loop.
#[tokio::test]
async fn a_store_that_panics_fails_the_read() {
    let farm = farm_keeping("xcode-job-panics", Arc::new(Failing(None)));
    assert!(matches!(
        farm.rig.coordinator.read_xcode_feed().await,
        Err(FeedError::Keep(_))
    ));
}

/// Every alert of the coordinator after one more observation, of every kind.
fn all_alerts(coordinator: &Coordinator) -> Vec<proto::Alert> {
    coordinator
        .observe_alerts()
        .expect("the alerts are observed");
    coordinator
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("the alerts are listed")
        .alerts
}

/// The dial a test makes of the fake origin.
type DialOf = fn(&FakeOrigin) -> Dial;

/// The three ways the read of the list fails, as the dial of each.
fn failing_reads() -> Vec<(&'static str, DialOf)> {
    vec![
        ("status-500", |fake| {
            fake.dial_all(Then::Write(response("500 Internal Server Error", b"")))
        }),
        ("garbage", |fake| {
            fake.dial_all(Then::Write(response("200 OK", b"<html>no</html>")))
        }),
        ("refused", |_| refusing_dial()),
    ]
}

/// The log line `line` without its timestamp, level and target: what the job said, then its fields.
fn said(line: &str) -> &str {
    line.split_once(" WARN ")
        .map_or(line, |(_, rest)| rest)
        .split_once(": ")
        .map_or(line, |(_, rest)| rest)
}

/// Proves: with nothing waiting (the store holds the newest release the list kept), a read of
/// the list that fails (an answer of 500, a body that is not a list, a refused connection)
/// leaves the alerts of every kind exactly as they were, the empty list, and logs the one warning
/// that names the failure and the address.
///
/// Catches: a failed read that raises an alert of its own (of any kind, which the filtered
/// tests above cannot see), and a failure that is not logged.
#[tokio::test]
async fn a_failed_read_raises_no_alert_when_nothing_is_waiting() {
    let wants = [
        "the list of Xcode releases could not be read; the job reads it again at its next run why=the list could not be read: the origin answered 500 url=https://origin.test/data.json",
        "the list of Xcode releases could not be read; the job reads it again at its next run why=the list is not a JSON array of releases: expected value at line 1 column 1 url=https://origin.test/data.json",
        "the list of Xcode releases could not be read; the job reads it again at its next run why=the list could not be read: the connection failed url=https://origin.test/data.json",
    ];
    for ((name, dial), want) in failing_reads().into_iter().zip(wants) {
        let farm = farm(&format!("xcode-job-nothing-{name}"), Some(FEED), dial);
        farm.known
            .keep(&[release("27.1", "27A9275", Channel::Release)])
            .expect("the list is kept");
        farm.held.set(vec![id("27.1", "27A9275")]);
        assert_eq!(all_alerts(&farm.rig.coordinator), vec![], "{name}");
        farm.rig.coordinator.xcode_feed_job().await;
        assert_eq!(all_alerts(&farm.rig.coordinator), vec![], "{name}");
        assert_eq!(said(&failure_line(&farm.log)), want, "{name}");
    }
}

/// Proves: with a newer release kept and not held, the one `xcode_release_waiting` alert there is
/// before a failed read of the list (an answer of 500, a body that is not a list, a refused
/// connection) is the whole alert list after it, unchanged and not doubled, and the list kept
/// stands.
///
/// Catches: a failed read that clears the alert of the release waiting, repeats it, or adds an
/// alert of another kind beside it.
#[tokio::test]
async fn a_failed_read_leaves_the_one_waiting_alert_as_it_was() {
    for (name, dial) in failing_reads() {
        let farm = farm(&format!("xcode-job-waiting-{name}"), Some(FEED), dial);
        let kept = vec![
            release("27.2", "27B52", Channel::Release),
            release("27.1", "27A9275", Channel::Release),
        ];
        farm.known.keep(&kept).expect("the list is kept");
        farm.held.set(vec![id("27.1", "27A9275")]);
        let before = all_alerts(&farm.rig.coordinator);
        assert_eq!(before.len(), 1, "{name}: {before:?}");
        assert_eq!(before[0].kind(), proto::AlertKind::XcodeReleaseWaiting);
        assert_eq!(before[0].version, "27.2");
        farm.rig.coordinator.xcode_feed_job().await;
        assert_eq!(all_alerts(&farm.rig.coordinator), before, "{name}");
        assert_eq!(farm.known.known(), Ok(kept), "{name}");
        assert!(failure_line(&farm.log).contains("WARN"), "{name}");
    }
}

/// Stores an archive of `version` and `build` whose bytes name the build in `store`.
fn stored(store: &XcodeStore, version: &str, build: &str) -> Archive {
    let bytes = format!("xip of {build}").into_bytes();
    let mut digest = Digesting::default();
    digest.feed(&bytes);
    let sha256 = digest.finish();
    let (path, mut file) = store.upload_file(&sha256).expect("an upload file");
    file.write_all(&bytes).expect("written");
    drop(file);
    let archive = Archive {
        sha256,
        id: id(version, build),
        size: u64::try_from(bytes.len()).expect("a size"),
        by: "uploader".to_owned(),
        at_unix: 1_800_000_000,
    };
    let _changing = store.changing();
    store.commit(&path, &archive).expect("committed");
    archive
}

/// The sha256 of each archive `store` holds, newest release first.
fn held_by(store: &XcodeStore) -> Vec<String> {
    store
        .archives()
        .expect("listed")
        .into_iter()
        .map(|archive| archive.sha256)
        .collect()
}

/// The entry the pruning of `archive` is audited as, by its number `seq`.
fn removal(seq: u64, archive: &Archive) -> AuditEntry {
    AuditEntry {
        seq,
        by: "coordinator".to_owned(),
        action: XCODE_REMOVE.to_owned(),
        service: None,
        node_id: None,
        host_group: None,
        reason: Some(format!(
            "pruned: Xcode {} ({}), sha256 {}",
            archive.id.version, archive.id.build, archive.sha256
        )),
        subject: Some(format!(
            "Xcode {} ({}) {}",
            archive.id.version, archive.id.build, archive.sha256
        )),
        source: None,
        api_token: None,
    }
}

/// Proves: the job `xcode-feed`, run once over a real Xcode store holding the archives of 27.1,
/// 27.2 and 27.3 and a group that keeps 1 Xcode release, prunes the store to 27.3 whether the
/// list is read (200) or the read fails (500): the two older archives' files are gone, and the
/// audit holds exactly one `xcode_remove` by `coordinator` for each of them, naming the release
/// and archive; a second run changes nothing.
///
/// Catches: a job that does not prune, one that prunes only when the list was read (the prune
/// must not wait for the origin), and a prune with no audit line or under another name.
#[tokio::test]
async fn the_job_prunes_the_store_whether_the_list_is_read_or_not() {
    let reads: [(&str, Vec<u8>, &str); 2] = [
        ("read", FIXTURE.to_vec(), "200 OK"),
        ("failed", Vec::new(), "500 Internal Server Error"),
    ];
    for (name, body, status) in reads {
        let mut world = World::new(&format!("xcode-job-prune-{name}"));
        let log = Log::default();
        let _guard = log.install();
        let dir = world.rig.dir.path().join("xcode");
        std::fs::create_dir(&dir).expect("the store's directory");
        let store =
            Arc::new(XcodeStore::open(&dir, 1 << 20, std::time::UNIX_EPOCH).expect("a store"));
        let spare = scratch(&format!("xcode-job-prune-{name}-spare"));
        let placeholder = start(&spare, &world.rig.clock, "");
        let built = std::mem::replace(&mut world.rig.coordinator, placeholder);
        let fake = FakeOrigin::new();
        let origin = RepositoryOrigin::new(fake.roots())
            .expect("the origin client")
            .with_dial(fake.dial_all(Then::Write(response(status, &body))));
        world.rig.coordinator = built
            .with_repository_origin(origin)
            .with_xcode_feed(Some(FeedUrl::parse(FEED).expect("a feed address")))
            .with_xcode_store(Arc::clone(&store));
        let policy = proto::RolloutPolicy {
            maintenance: Some(proto::MaintenancePolicy {
                xcode_keep: 1,
                ..proto::MaintenancePolicy::default()
            }),
            ..proto::RolloutPolicy::default()
        };
        world
            .put_with("alpha", &[SELF], Some(policy), None)
            .expect("a group put");
        let oldest = stored(&store, "27.1", "27A10");
        let second = stored(&store, "27.2", "27B20");
        let newest = stored(&store, "27.3", "27C30");
        assert_eq!(
            held_by(&store),
            vec![
                newest.sha256.clone(),
                second.sha256.clone(),
                oldest.sha256.clone()
            ]
        );

        world.rig.coordinator.xcode_feed_job().await;

        assert_eq!(held_by(&store), vec![newest.sha256.clone()], "{name}");
        for gone in [&second, &oldest] {
            assert!(!store.archive_path(&gone.sha256).exists(), "{name}");
            assert!(
                !dir.join(format!("{}.json", gone.sha256)).exists(),
                "{name}"
            );
        }
        assert!(store.archive_path(&newest.sha256).exists(), "{name}");
        let removals: Vec<AuditEntry> = world
            .audit()
            .into_iter()
            .filter(|entry| entry.action == XCODE_REMOVE)
            .collect();
        assert_eq!(removals.len(), 2, "{name}: {removals:?}");
        assert_eq!(
            removals,
            vec![
                removal(removals[0].seq, &second),
                removal(removals[1].seq, &oldest)
            ],
            "{name}"
        );
        let audited = world.audit();
        world.rig.coordinator.xcode_feed_job().await;
        assert_eq!(world.audit(), audited, "{name}");
        assert_eq!(held_by(&store), vec![newest.sha256], "{name}");
        assert_eq!(failure_line(&log).is_empty(), name == "read", "{name}");
    }
}
