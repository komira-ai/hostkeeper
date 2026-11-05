//! Acknowledged verdicts on a coordinator over real files (design sections 5.4 and 8.2): a
//! verdict is acknowledged only once the file of verdicts holds it; a host acknowledged a
//! failure is not ordered the version again when it stops sending it, before or after a
//! restart; a verdict resent, before or after a restart, is counted once; and an agent that did
//! not announce `verdict-acks-v1` is read and answered as before. Every time is explicit.

use std::fs;
use std::path::PathBuf;

use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Heartbeat, OrderId, ServiceBeat, ServiceReply, VerdictAck};

use crate::desired::DesiredStore;
use crate::rollback_tests::{AUTO, HOSTS, TWO, failure, release, restart, rig, seed, wanted, who};
use crate::service::Coordinator;
use crate::services::Services;
use crate::testutil::{LEGACY, Log, OPERATOR, Rig, beat, clock_of, verdicts_of, version};
use crate::verdict_file::{FEATURE, VerdictFile, verdicts_path};

/// `b` from an agent that announced `verdict-acks-v1` (and `heartbeat-v2`, so the services it
/// lists are all it runs).
fn acking(mut b: Heartbeat) -> Heartbeat {
    b.agent_features = vec![FEATURE.to_owned(), "heartbeat-v2".to_owned()];
    b
}

/// A release failure of 2.0 that ends try `attempt` of `order`, from a host at 1.0.
fn verdict(order: &str, attempt: u64) -> Heartbeat {
    let (phase, class) = release();
    let mut b = failure(phase, class, "2.0");
    let last = b.last_update.as_mut().expect("a verdict");
    last.order_id = OrderId::parse(order);
    last.order_try = Some(attempt);
    b
}

fn ack(service: &str, order: &str, attempt: u64) -> VerdictAck {
    VerdictAck {
        service: service.to_owned(),
        order_id: OrderId::parse(order).expect("an order id"),
        order_try: attempt,
    }
}

/// The answer to `b` from `host` at `at`: its reply and its acks.
fn ask(rig: &Rig, host: &str, b: Heartbeat, at: u64) -> (ServiceReply, Vec<VerdictAck>) {
    rig.at(at);
    let answer = rig
        .coordinator
        .heartbeat(who(host), b)
        .expect("a heartbeat");
    (answer.reply, answer.acks)
}

fn ordered(reply: &ServiceReply, want: &str) -> bool {
    matches!(reply, ServiceReply::Update { version: v, .. } if *v == version(want))
}

fn file_of(rig: &Rig) -> PathBuf {
    verdicts_path(&rig.dir.path().join("desired.json"))
}

/// How many verdicts about 2.0 the coordinator counts now.
fn counted(rig: &Rig) -> usize {
    rig.coordinator
        .lock_fleet()
        .verdicts_of(LEGACY, &version("2.0"))
        .count()
}

/// Catches: an ack sent before the verdict is durable, so a crash after the answer loses a
/// verdict the daemon will never send again: while the file of verdicts cannot be written the
/// answer acknowledges nothing and says why in the log, and the host is still read as failed
/// (it resends); the resend once the file can be written is acknowledged, and the file holds
/// it when the answer is out.
#[tokio::test]
async fn a_verdict_is_acknowledged_only_once_it_is_stored() {
    let rig = rig("acks-stored", "", TWO).await;
    let path = file_of(&rig);
    let mut temp = path.clone().into_os_string();
    temp.push(".new");
    fs::create_dir(&temp).expect("the temporary file's name taken");
    let log = Log::default();
    let guard = log.install();
    let (reply, acks) = ask(&rig, "d1", acking(verdict("o-1", 1)), 2_000);
    drop(guard);
    assert_eq!((reply, acks), (ServiceReply::Nothing, vec![]));
    assert!(!path.exists());
    assert!(
        log.text()
            .contains("verdicts not written: none acknowledged"),
        "{}",
        log.text()
    );
    fs::remove_dir(&temp).expect("out of the way");
    let (reply, acks) = ask(&rig, "d1", acking(verdict("o-1", 1)), 2_015);
    assert_eq!(reply, ServiceReply::Nothing);
    assert_eq!(acks, [ack(LEGACY, "o-1", 1)]);
    let stored = VerdictFile::open(&path).expect("the file");
    assert_eq!(
        stored.standing(LEGACY, &crate::testutil::id("d1")),
        verdict("o-1", 1).last_update
    );
}

/// Catches: the hazard acks open: a daemon stops sending a verdict once it is acknowledged, so
/// a coordinator that read only the heartbeat would order the version it failed again (try 1,
/// holding the fleet's slot), at once and after a restart that rebuilt the fleet from
/// heartbeats. Acknowledged, the failure stands for the host: it is ordered nothing before or
/// after the restart, while a host that never failed is ordered the version.
#[tokio::test]
async fn a_restart_after_an_ack_does_not_order_the_version_again() {
    let mut rig = rig("acks-restart", "", TWO).await;
    let (reply, acks) = ask(&rig, "d1", acking(verdict("o-1", 1)), 2_000);
    assert_eq!(
        (reply, acks),
        (ServiceReply::Nothing, vec![ack(LEGACY, "o-1", 1)])
    );
    let stopped = acking(beat("1.0"));
    assert_eq!(
        ask(&rig, "d1", stopped.clone(), 2_015),
        (ServiceReply::Nothing, vec![])
    );
    restart(&mut rig, "");
    assert_eq!(
        ask(&rig, "d1", stopped.clone(), 2_100),
        (ServiceReply::Nothing, vec![])
    );
    let (reply, _) = ask(&rig, "d2", stopped, 2_100);
    assert!(ordered(&reply, "2.0"), "{reply:?}");
}

/// Catches: a verdict counted per resend or per restart: one host resending its verdict after
/// a lost answer, and again after a restart that reloaded it, is one failure, below the
/// threshold of two of six; a second host's failure after the restart meets it, so the counted
/// verdict reloaded still counts.
#[tokio::test]
async fn a_duplicate_verdict_is_counted_once() {
    let mut rig = rig("acks-duplicate", AUTO, TWO).await;
    seed(&rig, |marks| marks.last_good = Some(version("1.0")));
    restart(&mut rig, AUTO);
    rig.at(2_100);
    for host in HOSTS {
        ask(&rig, host, beat("1.0"), 2_100);
    }
    let first = ask(&rig, "d1", acking(verdict("o-1", 1)), 2_100);
    let lost = ask(&rig, "d1", acking(verdict("o-1", 1)), 2_115);
    assert_eq!(first, lost);
    assert_eq!(first.1, [ack(LEGACY, "o-1", 1)]);
    assert_eq!(counted(&rig), 1);
    restart(&mut rig, AUTO);
    assert_eq!(counted(&rig), 1, "reloaded");
    rig.at(2_200);
    for host in HOSTS {
        ask(&rig, host, acking(beat("1.0")), 2_200);
    }
    ask(&rig, "d1", acking(verdict("o-1", 1)), 2_215);
    assert_eq!(counted(&rig), 1);
    assert_eq!(wanted(&rig), version("2.0"));
    ask(&rig, "d2", acking(verdict("o-2", 1)), 2_230);
    assert_eq!(counted(&rig), 2);
    assert_eq!(wanted(&rig), version("1.0"), "rolled back");
}

/// Catches: an agent that did not announce `verdict-acks-v1`, or a coordinator that does not
/// advertise it, read as if it had: no ack, nothing stored for the host, and its next heartbeat
/// without the verdict is read as it is, so it is ordered as before.
#[tokio::test]
async fn without_the_feature_on_both_sides_nothing_is_acknowledged_or_kept() {
    let mut rig = rig("acks-unannounced", "", TWO).await;
    let mut halting = verdict("o-1", 1);
    halting.agent_features = vec!["halt-v1".to_owned()];
    assert_eq!(
        ask(&rig, "d1", halting, 2_000),
        (ServiceReply::Nothing, vec![])
    );
    // Counted, as every release failure is, but standing for no host.
    assert_eq!(counted(&rig), 1);
    let stored = VerdictFile::open(&file_of(&rig)).expect("the file");
    assert_eq!(stored.standing(LEGACY, &crate::testutil::id("d1")), None);
    let (reply, _) = ask(&rig, "d1", beat("1.0"), 2_015);
    assert!(ordered(&reply, "2.0"), "{reply:?}");

    rig.coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_services(crate::rollback_tests::services_of(""))
        .with_features(&["halt-v1"]);
    rig.at(2_100);
    let (reply, acks) = ask(&rig, "d3", acking(verdict("o-3", 1)), 2_100);
    assert_eq!((reply, acks), (ServiceReply::Nothing, vec![]));
    let stored = VerdictFile::open(&file_of(&rig)).expect("the file");
    assert_eq!(stored.standing(LEGACY, &crate::testutil::id("d3")), None);
}

/// The coordinator of a rig of `kbf` and `web` on the role `daemon`, starting now.
fn start_two(rig: &Rig) -> Coordinator {
    let text = "[[service]]\nname = \"kbf\"\nroles = [\"daemon\"]\n\n[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";
    let list = format!(
        "{}services = [\"kbf\", \"web\"]\n",
        config_text(STATIC_ISSUER)
    );
    Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        .with_services(Services::parse(text).expect("services"))
}

/// A rig of `kbf` and `web`, both desired at 2.0, past the quiet start.
async fn two_services(test: &str) -> Rig {
    let mut rig = Rig::new(test);
    rig.coordinator = start_two(&rig);
    rig.set(TWO).await;
    rig.set(r#"{"service":"web","version":"2.0","min_up":{"storage":0,"frontend":0}}"#)
        .await;
    rig.at(2_000);
    rig
}

/// A host at kbf 2.0 that lists `web` as `entry`, and `extra`, a service no one manages.
fn listing(entry: ServiceBeat, extra: Option<ServiceBeat>) -> Heartbeat {
    let mut b = acking(beat("2.0"));
    b.services = Some(std::iter::once(entry).chain(extra).collect());
    b
}

fn web(running: &str, last: Option<&Heartbeat>) -> ServiceBeat {
    ServiceBeat {
        name: "web".to_owned(),
        version: Version::parse(running).expect("a version"),
        health: hostkeeper_wire::wire::Health::Healthy,
        last_update: last.and_then(|b| b.last_update.clone()),
        updating: None,
        step: None,
        commit: None,
        step_secs: Default::default(),
        promotion_pending: false,
        deferred: None,
    }
}

/// Catches: an ack naming the wrong service, or one given for a service the coordinator does
/// not manage (whose verdict it never counted), or a service entry's verdict forgotten once
/// acknowledged: the answer acknowledges the `web` entry's verdict by its service and not the
/// unmanaged one's; `web` is not ordered again when the entry stops carrying it, nor after a
/// restart, while another host is.
#[tokio::test]
async fn a_service_entry_is_acknowledged_by_its_service_and_still_read_as_failed() {
    let mut rig = two_services("acks-services").await;
    let failed = verdict("o-9", 1);
    let mut other = web("1.0", Some(&verdict("o-8", 1)));
    other.name = "unmanaged".to_owned();
    let (reply, acks) = ask(
        &rig,
        "d1",
        listing(web("1.0", Some(&failed)), Some(other)),
        2_000,
    );
    assert_eq!(
        (reply, acks),
        (ServiceReply::Nothing, vec![ack("web", "o-9", 1)])
    );
    let stopped = listing(web("1.0", None), None);
    assert_eq!(
        ask(&rig, "d1", stopped.clone(), 2_015).0,
        ServiceReply::Nothing
    );
    rig.coordinator = start_two(&rig);
    rig.at(2_100);
    assert_eq!(
        ask(&rig, "d1", stopped.clone(), 2_100).0,
        ServiceReply::Nothing
    );
    let (reply, _) = ask(&rig, "d2", stopped, 2_100);
    assert_eq!(
        reply,
        ServiceReply::Update {
            service: Some("web".to_owned()),
            version: version("2.0"),
        }
    );
}

/// Catches: the legacy service's verdict left unacknowledged on a coordinator whose services
/// file does not define the legacy service (its top-level fields still count), or a verdict
/// that names no try acknowledged: the first is acknowledged, the second is not.
#[test]
fn a_legacy_service_without_a_definition_is_acknowledged() {
    let dir = crate::testutil::scratch("acks-legacy-undefined");
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let store = DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("store");
    let (verifier, _) = hostkeeper_oidc::testkit::static_verifier();
    let coordinator = Coordinator::new(store, verifier, OPERATOR.to_owned(), clock_of(&clock))
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_verdicts(verdicts_of(&dir));
    let answer = coordinator
        .heartbeat(who("d1"), acking(verdict("o-1", 1)))
        .expect("a heartbeat");
    assert_eq!(answer.acks, [ack(LEGACY, "o-1", 1)]);
    let mut untried = verdict("o-2", 1);
    if let Some(last) = untried.last_update.as_mut() {
        last.order_try = None;
    }
    let answer = coordinator
        .heartbeat(who("d1"), acking(untried))
        .expect("a heartbeat");
    assert!(answer.acks.is_empty());
    assert_eq!(
        verdicts_of(&dir).standing(LEGACY, &crate::testutil::id("d1")),
        None,
        "a verdict that cannot be acknowledged ends the one standing"
    );
}

/// Catches: a verdict acknowledged from a host the service ignores (its role does not run the
/// service, so its word is not recorded and its verdict never counted), or one not acknowledged
/// from a host the service took out (excluded: its word is kept as what it occupies, its
/// verdict counted for its slot): the first gets no ack and nothing stored, the second its ack.
#[tokio::test]
async fn only_a_host_whose_word_is_taken_in_is_acknowledged() {
    let mut rig = rig("acks-standings", "", TWO).await;
    let stranger = crate::identity::Identity {
        host_id: crate::testutil::id("s1"),
        role: crate::testutil::role("storage"),
    };
    rig.at(2_000);
    let answer = rig
        .coordinator
        .heartbeat(stranger, acking(verdict("o-1", 1)))
        .expect("a heartbeat");
    assert_eq!(answer.acks, []);
    crate::testutil::ops_of(&rig.dir)
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("operator", "exclude"),
            |state| {
                state.excluded.push(crate::ops::Exclusion {
                    service: LEGACY.to_owned(),
                    node_id: "d1".to_owned(),
                    reason: "disk swap".to_owned(),
                });
            },
        )
        .expect("excluded");
    restart(&mut rig, "");
    let (_, acks) = ask(&rig, "d1", acking(verdict("o-2", 1)), 2_100);
    assert_eq!(acks, [ack(LEGACY, "o-2", 1)]);
    let stored = VerdictFile::open(&file_of(&rig)).expect("the file");
    assert_eq!(stored.standing(LEGACY, &crate::testutil::id("s1")), None);
}

/// Catches: an answer an agent refuses: the legacy service named twice in one heartbeat (its
/// top-level fields and an entry of `services`), each with the same verdict, is acknowledged
/// once, and the answer reads back.
#[tokio::test]
async fn a_service_named_twice_is_acknowledged_once() {
    let rig = rig("acks-twice", "", TWO).await;
    let mut b = acking(verdict("o-1", 1));
    let mut entry = web("1.0", Some(&verdict("o-1", 1)));
    entry.name = LEGACY.to_owned();
    b.services = Some(vec![entry]);
    let answer = rig
        .coordinator
        .heartbeat(who("d1"), b)
        .expect("a heartbeat");
    assert_eq!(answer.acks, [ack(LEGACY, "o-1", 1)]);
    let text = serde_json::to_string(&answer).expect("JSON");
    let read: hostkeeper_wire::wire::Answer = serde_json::from_str(&text).expect("an answer");
    assert_eq!(read.acks, answer.acks);

    // Named twice with two verdicts, the one heard last stands, and only it is acknowledged.
    let mut b = acking(verdict("o-2", 1));
    let mut entry = web("1.0", Some(&verdict("o-3", 1)));
    entry.name = LEGACY.to_owned();
    b.services = Some(vec![entry]);
    let answer = rig
        .coordinator
        .heartbeat(who("d1"), b)
        .expect("a heartbeat");
    assert_eq!(answer.acks, [ack(LEGACY, "o-3", 1)]);
}

/// Catches: the acks tied to a legacy service: on a coordinator with none, where only the
/// entries of `services` count, an entry's verdict is acknowledged.
#[test]
fn a_coordinator_without_a_legacy_service_acknowledges_the_entries() {
    let dir = crate::testutil::scratch("acks-no-legacy");
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let store = DesiredStore::open(&dir.path().join("desired.json"), None).expect("store");
    let (verifier, _) = hostkeeper_oidc::testkit::static_verifier();
    let text = "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";
    let coordinator = Coordinator::new(store, verifier, OPERATOR.to_owned(), clock_of(&clock))
        .with_services(Services::parse(text).expect("services"))
        .with_verdicts(verdicts_of(&dir));
    let answer = coordinator
        .heartbeat(
            who("d1"),
            listing(web("1.0", Some(&verdict("o-1", 1))), None),
        )
        .expect("a heartbeat");
    assert_eq!(answer.acks, [ack("web", "o-1", 1)]);
}

/// Catches: the verdicts reloaded at a start lost by the quiet period set after them (the
/// fleet built again empty): a coordinator over a file of counted verdicts counts them still
/// once `--quiet-secs` is applied, and keeps its start.
#[test]
fn the_quiet_period_keeps_the_verdicts_reloaded() {
    let dir = crate::testutil::scratch("acks-quiet");
    verdicts_of(&dir)
        .record(
            &crate::testutil::id("d1"),
            &[],
            &[(
                LEGACY.to_owned(),
                vec![(
                    crate::testutil::id("d1"),
                    verdict("o-1", 1).last_update.expect("a verdict"),
                )],
            )],
        )
        .expect("written");
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let store = DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("store");
    let (verifier, _) = hostkeeper_oidc::testkit::static_verifier();
    let coordinator = Coordinator::new(store, verifier, OPERATOR.to_owned(), clock_of(&clock))
        .with_verdicts(verdicts_of(&dir))
        .with_quiet(std::time::Duration::from_secs(120));
    let fleet = coordinator.lock_fleet();
    assert_eq!(fleet.verdicts_of(LEGACY, &version("2.0")).count(), 1);
    assert_eq!(fleet.started(), std::time::Duration::from_secs(1_000));
    assert!(fleet.is_quiet(std::time::Duration::from_secs(1_119)));
    assert!(!fleet.is_quiet(std::time::Duration::from_secs(1_120)));
}
