//! The sync, the token proof and the question of the approval ([`crate::abm::Abm`]) over the
//! simulated ABM and the tables of the file store: what a pass writes, what it asks Apple, and
//! what it does on each answer Apple may give.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::abm::client::{AbmError, Sleep};
use crate::abm::fake::{
    FakeAbm, Handler, NOW, Sim, body_of, counting_random, json_reply, recording_sleep, sample_token,
};
use crate::abm::transport::AbmRequest;
use crate::abm::{Abm, Pass, SyncError, SyncReport, sync_every};
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::store::mdm::files::FileMdm;
use crate::store::mdm::{AbmDevice, AbmState, MdmStore};
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};
use crate::testutil::scratch;
use hostkeeper_testkit::Scratch;

const DAY: u64 = 86_400;

/// Everything a pass touches.
struct Rig {
    abm: Arc<Abm>,
    setup: Arc<MdmSetup>,
    store: Arc<FileMdm>,
    fake: Arc<FakeAbm>,
    sim: Sim,
    clock: Arc<AtomicU64>,
    dir: Scratch,
}

fn token_json() -> Vec<u8> {
    let token = sample_token();
    serde_json::to_vec(&json!({
        "consumer_key": token.consumer_key,
        "consumer_secret": token.consumer_secret,
        "access_token": token.access_token,
        "access_secret": token.access_secret,
        "access_secret_expiry": token.access_secret_expiry,
    }))
    .expect("json")
}

fn keyed() -> MdmSetup {
    MdmSetup::volatile().with_key(MasterKey::from_bytes(&[3; 32]).expect("a key"))
}

/// A rig over `sim` answering by `handler`, whose coordinator holds the token (when `token`) and,
/// when `named`, has the setting `mdm_name`.
fn rig_with(name: &str, token: bool, named: bool, sim: Sim, handler: Handler) -> Rig {
    let dir = scratch(name);
    let store = Arc::new(FileMdm::open(&dir.path().join("mdm")).expect("the tables"));
    let setup = Arc::new(keyed());
    if token {
        setup
            .put_secret(SecretKind::AbmToken, &Secret::new(token_json()), None, 1)
            .expect("the token");
    }
    if named {
        setup
            .set_setting("mdm_name", "mdm.internal.komira.dev")
            .expect("the name");
    }
    let fake = FakeAbm::new(handler);
    let clock = Arc::new(AtomicU64::new(NOW));
    let ticking = Arc::clone(&clock);
    let (sleep, _) = recording_sleep();
    let abm = Arc::new(Abm::new(
        Arc::clone(&store) as _,
        Arc::clone(&fake) as _,
        Arc::new(move || ticking.load(Ordering::SeqCst)),
        counting_random(),
        sleep,
    ));
    Rig {
        abm,
        setup,
        store,
        fake,
        sim,
        clock,
        dir,
    }
}

/// A rig over a simulated ABM that the test arranged first.
fn simulated(name: &str, token: bool, named: bool, arrange: impl FnOnce(&Sim)) -> Rig {
    let sim = Sim::default();
    arrange(&sim);
    let handler = sim.handler();
    rig_with(name, token, named, sim, handler)
}

async fn done(rig: &Rig) -> SyncReport {
    match rig.abm.sync_once(&rig.setup).await.expect("a pass") {
        Pass::Done(report) => report,
        Pass::NoToken => panic!("no token"),
    }
}

fn rows(rig: &Rig) -> BTreeMap<String, AbmDevice> {
    rig.store
        .abm_devices()
        .expect("rows")
        .into_iter()
        .map(|d| (d.serial.clone(), d))
        .collect()
}

/// Proves: with no token held a pass asks Apple nothing and says so, and neither a pass without a
/// token nor the status of it is an error.
///
/// Catches: a coordinator that has not been given a token and logs an error every five minutes,
/// or one that opens a session to Apple with nothing to sign.
#[tokio::test]
async fn without_a_token_nothing_is_asked_of_abm() {
    let rig = simulated("abm-notoken", false, true, |_| {});
    assert_eq!(rig.abm.sync_once(&rig.setup).await, Ok(Pass::NoToken));
    assert!(rig.fake.seen().is_empty());
    rig.abm.sync_noted(&rig.setup).await;
    assert_eq!(rig.abm.status(), Default::default());
    assert_eq!(rig.store.abm_state().expect("state"), None);
}

/// Proves: the first pass is a full fetch: every device ABM lists is written with its model,
/// description, profile status and profile, first seen and synced now, the cursor of the fetch is
/// stored with the time of the pass, and, with no MDM name set, no profile is defined and none
/// assigned (nothing is written to ABM).
///
/// Catches: a sync that writes to Apple before the operator has named the MDM host, and a first
/// pass that loses the cursor and does a full fetch again every time.
#[tokio::test]
async fn the_first_pass_is_a_full_fetch_and_writes_nothing_to_abm() {
    let rig = simulated("abm-first", true, false, |sim| {
        sim.device("P24LH9H6JT", "pushed", Some("OLD-PROFILE"));
        sim.device("EMPTY1", "empty", None);
    });
    let report = done(&rig).await;
    assert_eq!(
        report,
        SyncReport {
            full: true,
            upserted: 2,
            ..SyncReport::default()
        }
    );
    let held = rows(&rig);
    assert_eq!(held["P24LH9H6JT"].profile_status, "pushed");
    assert_eq!(
        held["P24LH9H6JT"].profile_uuid.as_deref(),
        Some("OLD-PROFILE")
    );
    assert_eq!(held["P24LH9H6JT"].model.as_deref(), Some("Mac Studio"));
    assert_eq!(
        held["P24LH9H6JT"].description.as_deref(),
        Some("MAC STUDIO")
    );
    assert_eq!(
        held["P24LH9H6JT"].first_seen_unix,
        i64::try_from(NOW).expect("time")
    );
    assert_eq!(held["EMPTY1"].profile_uuid, None);
    assert_eq!(held["EMPTY1"].removed_unix, None);
    assert_eq!(
        rig.store.abm_state().expect("state"),
        Some(AbmState {
            cursor: "c0".to_owned(),
            profile_uuid: None,
            profile_digest: None,
            synced_unix: Some(i64::try_from(NOW).expect("time")),
        })
    );
    assert_eq!(
        rig.fake.calls(),
        ["GET /session", "POST /server/devices"],
        "no write to ABM"
    );
}

/// Proves: with the MDM name set, a pass defines the ADE profile (named by the setting, sending a
/// Mac to `https://<name>/enroll`, supervised, mandatory, not removable, not awaiting
/// configuration, pairing allowed), stores its uuid with its digest, and assigns it to the devices
/// that have none and to those alone: the one already on a profile keeps it; the table shows the
/// assignment; and a second pass changes nothing at Apple but the sync call.
///
/// Catches: a profile that lets a Mac skip the MDM, a device moved off a working profile, and a
/// sync that defines a profile or assigns again every five minutes.
#[tokio::test]
async fn the_profile_is_defined_once_and_given_to_devices_that_have_none() {
    let rig = simulated("abm-profile", true, true, |sim| {
        sim.device("P24LH9H6JT", "pushed", Some("OLD-PROFILE"));
        sim.device("NEW1", "empty", None);
        sim.device("NEW2", "empty", None);
    });
    let report = done(&rig).await;
    assert_eq!(
        report,
        SyncReport {
            full: true,
            upserted: 3,
            removed: 0,
            profile_defined: true,
            assigned: 2,
            unassigned: 0,
        }
    );
    let uuid = format!("PROFILE{:028}", 1);
    let profile = rig.sim.state().profiles[&uuid].clone();
    assert_eq!(profile["profile_name"], "Komira MDM");
    assert_eq!(profile["url"], "https://mdm.internal.komira.dev/enroll");
    for (flag, want) in [
        ("is_supervised", true),
        ("is_mandatory", true),
        ("is_mdm_removable", false),
        ("await_device_configured", false),
        ("allow_pairing", true),
        ("auto_advance_setup", true),
    ] {
        assert_eq!(profile[flag], json!(want), "{flag}");
    }
    assert_eq!(profile["anchor_certs"], json!([]));
    assert!(profile["skip_setup_items"].as_array().expect("items").len() > 10);

    let held = rows(&rig);
    assert_eq!(
        held["P24LH9H6JT"].profile_uuid.as_deref(),
        Some("OLD-PROFILE")
    );
    for serial in ["NEW1", "NEW2"] {
        assert_eq!(held[serial].profile_uuid.as_deref(), Some(uuid.as_str()));
        assert_eq!(held[serial].profile_status, "assigned");
    }
    let state = rig.store.abm_state().expect("state").expect("held");
    assert_eq!(state.profile_uuid.as_deref(), Some(uuid.as_str()));
    assert_eq!(state.profile_digest.as_deref().map(str::len), Some(64));
    let assigned = rig
        .fake
        .seen()
        .iter()
        .find(|r| r.path == "/profile/devices")
        .map(body_of)
        .expect("an assignment");
    assert_eq!(
        assigned,
        json!({ "profile_uuid": uuid, "devices": ["NEW1", "NEW2"] })
    );

    rig.fake.seen();
    let before = rig.fake.calls().len();
    let second = done(&rig).await;
    assert_eq!(second, SyncReport::default());
    assert_eq!(&rig.fake.calls()[before..], ["POST /devices/sync"]);
}

/// Proves: a sync's records change the table as `op_type` says: `added` and `modified` write the
/// device, `deleted` marks it removed (so that it is not ours), a device added again is ours again,
/// and a record whose status is `removed` is a removal too; the cursor moves to the one ABM
/// answered; nothing is written to ABM.
///
/// Catches: a disowned Mac that stays approvable, and one that comes back and stays removed.
#[tokio::test]
async fn a_sync_adds_changes_and_removes_devices() {
    let rig = simulated("abm-changes", true, false, |sim| {
        sim.device("KEEP", "pushed", Some("P"));
        sim.device("GONE", "pushed", Some("P"));
    });
    done(&rig).await;
    assert!(rig.abm.serial_in_abm("GONE").expect("read"));

    {
        let mut state = rig.sim.state();
        state.devices.retain(|d| d.serial != "GONE");
        state.log.push(("GONE".to_owned(), "deleted".to_owned()));
    }
    rig.sim.device("FRESH", "empty", None);
    rig.sim
        .state()
        .log
        .push(("FRESH".to_owned(), "added".to_owned()));
    let report = done(&rig).await;
    assert_eq!(
        report,
        SyncReport {
            upserted: 1,
            removed: 1,
            ..SyncReport::default()
        }
    );
    assert!(!rig.abm.serial_in_abm("GONE").expect("read"));
    assert!(rig.abm.serial_in_abm("FRESH").expect("read"));
    assert!(rig.abm.serial_in_abm("KEEP").expect("read"));
    assert!(!rig.abm.serial_in_abm("NOT-IN-ABM").expect("read"));
    assert_eq!(
        rows(&rig)["GONE"].removed_unix,
        Some(i64::try_from(NOW).expect("time"))
    );
    assert_eq!(
        rig.store.abm_state().expect("state").expect("held").cursor,
        "c2"
    );

    // GONE comes back; KEEP is modified.
    rig.sim.device("GONE", "assigned", Some("P"));
    {
        let mut state = rig.sim.state();
        state.log.push(("GONE".to_owned(), "added".to_owned()));
        state
            .devices
            .iter_mut()
            .find(|d| d.serial == "KEEP")
            .expect("keep")
            .profile_status = "removed".to_owned();
        state.log.push(("KEEP".to_owned(), "modified".to_owned()));
    }
    let report = done(&rig).await;
    assert_eq!((report.upserted, report.removed), (1, 1));
    assert!(rig.abm.serial_in_abm("GONE").expect("read"));
    assert!(!rig.abm.serial_in_abm("KEEP").expect("read"));
    assert!(
        !rig.fake
            .calls()
            .iter()
            .any(|c| c.contains("PUT") || c == "POST /profile"),
        "nothing is written to ABM"
    );
}

/// Proves: a full fetch marks removed every serial the table held and ABM no longer lists, keeps
/// the ones it lists, and pages: a list of three devices at a page of one is three calls whose
/// cursors chain, and the cursor stored is the last page's, which the next pass hands to
/// `/devices/sync`.
///
/// Catches: a device disowned while the coordinator was off that stays approvable, a page that is
/// dropped, and a full fetch whose cursor cannot seed the next sync.
#[tokio::test]
async fn a_full_fetch_removes_what_abm_no_longer_lists_and_follows_its_pages() {
    let rig = simulated("abm-full", true, false, |sim| {
        sim.device("A", "pushed", Some("P"));
        sim.device("B", "pushed", Some("P"));
        sim.device("C", "pushed", Some("P"));
        sim.state().page = 1;
    });
    let report = done(&rig).await;
    assert_eq!((report.full, report.upserted), (true, 3));
    let pages: Vec<_> = rig
        .fake
        .seen()
        .iter()
        .filter(|r| r.path == "/server/devices")
        .map(body_of)
        .collect();
    assert_eq!(
        pages,
        [
            json!({ "limit": 1000 }),
            json!({ "limit": 1000, "cursor": "full-1" }),
            json!({ "limit": 1000, "cursor": "full-2" }),
        ]
    );
    assert_eq!(
        rig.store.abm_state().expect("state").expect("held").cursor,
        "c0"
    );
    assert_eq!(done(&rig).await, SyncReport::default());
    assert!(rig.fake.calls().contains(&"POST /devices/sync".to_owned()));

    // ABM disowned B while nothing was syncing; a fetch with no cursor sees it gone.
    rig.sim.state().devices.retain(|d| d.serial != "B");
    rig.store
        .abm_apply(&crate::store::mdm::AbmBatch {
            state: Some(AbmState {
                cursor: String::new(),
                profile_uuid: None,
                profile_digest: None,
                synced_unix: None,
            }),
            ..Default::default()
        })
        .expect("cursor lost");
    let report = done(&rig).await;
    assert_eq!((report.full, report.upserted, report.removed), (true, 2, 1));
    assert!(!rig.abm.serial_in_abm("B").expect("read"));
    assert!(rig.abm.serial_in_abm("A").expect("read"));

    // A second full fetch finds B already removed and removes nothing more.
    rig.store
        .abm_apply(&crate::store::mdm::AbmBatch {
            state: Some(AbmState {
                cursor: String::new(),
                profile_uuid: None,
                profile_digest: None,
                synced_unix: None,
            }),
            ..Default::default()
        })
        .expect("cursor lost again");
    let report = done(&rig).await;
    assert_eq!((report.full, report.upserted, report.removed), (true, 2, 0));
}

/// Proves: when ABM answers `INVALID_CURSOR` the pass falls back to a full fetch in the same pass
/// and stores the new cursor; and a cursor older than a week is not tried: the pass fetches fully
/// at once.
///
/// Catches: a sync that stays down for ever after a cursor expires, which Apple does after seven
/// days of no sync.
#[tokio::test]
async fn a_dead_or_old_cursor_gives_a_full_fetch() {
    let rig = simulated("abm-cursor", true, false, |sim| {
        sim.device("A", "pushed", Some("P"));
    });
    done(&rig).await;
    rig.sim.state().dead_cursors.push("c0".to_owned());
    let before = rig.fake.calls().len();
    let report = done(&rig).await;
    assert!(report.full);
    assert_eq!(
        &rig.fake.calls()[before..],
        ["POST /devices/sync", "POST /server/devices"]
    );
    rig.sim.state().dead_cursors.clear();

    rig.clock.store(NOW + 8 * DAY, Ordering::SeqCst);
    let before = rig.fake.calls().len();
    let report = done(&rig).await;
    assert!(report.full);
    assert_eq!(&rig.fake.calls()[before..], ["POST /server/devices"]);

    // A cursor 7 days old is still tried.
    rig.clock.store(NOW + 15 * DAY, Ordering::SeqCst);
    let before = rig.fake.calls().len();
    assert!(!done(&rig).await.full);
    assert_eq!(&rig.fake.calls()[before..], ["POST /devices/sync"]);
}

/// Proves: a sync that comes in pages is read to its last page: the devices of every page are
/// applied in one change with the cursor of the last, a device that is listed twice takes its
/// last record, and one added and then deleted in the same pass ends removed.
///
/// Catches: the second page of a busy day dropped, and a delete that an earlier add in the same
/// pass undoes.
#[tokio::test]
async fn every_page_of_a_sync_is_applied_with_the_last_cursor() {
    let pages: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sim = Sim::default();
    sim.device("S1", "empty", None);
    let inner = sim.handler();
    let script = Arc::clone(&pages);
    let handler: Handler = Box::new(move |request: &AbmRequest| {
        if request.path == "/devices/sync" && body_of(request)["cursor"] != json!("c0") {
            let next = script.lock().expect("script").remove(0);
            return json_reply(200, &next);
        }
        inner(request)
    });
    let rig = rig_with("abm-pages", true, false, sim, handler);
    done(&rig).await;
    *pages.lock().expect("script") = vec![
        json!({ "cursor": "p1", "more_to_follow": true, "devices": [
            { "serial_number": "S1", "op_type": "modified", "profile_status": "assigned", "profile_uuid": "X" },
            { "serial_number": "S2", "op_type": "added", "profile_status": "empty" },
        ]}),
        json!({ "cursor": "p2", "more_to_follow": false, "devices": [
            { "serial_number": "S1", "op_type": "modified", "profile_status": "pushed", "profile_uuid": "Y" },
            { "serial_number": "S2", "op_type": "deleted" },
            { "serial_number": "S3", "op_type": "deleted" },
        ]}),
    ];
    // The cursor "c0" is the sim's; make the stored one something else so the script answers.
    rig.store
        .abm_apply(&crate::store::mdm::AbmBatch {
            state: Some(AbmState {
                cursor: "stored".to_owned(),
                profile_uuid: None,
                profile_digest: None,
                synced_unix: Some(i64::try_from(NOW).expect("time")),
            }),
            ..Default::default()
        })
        .expect("a cursor");
    let report = done(&rig).await;
    assert_eq!(
        (report.full, report.upserted, report.removed),
        (false, 1, 2)
    );
    let held = rows(&rig);
    assert_eq!(held["S1"].profile_status, "pushed");
    assert_eq!(held["S1"].profile_uuid.as_deref(), Some("Y"));
    // S2 and S3 were never in the table: a removal of a serial it does not hold is ignored.
    assert!(!held.contains_key("S2") && !held.contains_key("S3"));
    assert!(!rig.abm.serial_in_abm("S2").expect("read"));
    assert!(!rig.abm.serial_in_abm("S3").expect("read"));
    assert_eq!(
        rig.store.abm_state().expect("state").expect("held").cursor,
        "p2"
    );
}

/// Proves: when the profile setting changes, a new profile is defined and stored with its digest,
/// devices on the old profile keep it, and devices still without one get the new one; a name that
/// is not a host name defines nothing.
///
/// Catches: a changed profile name that is never applied, and a device taken off a profile it was
/// enrolling with.
#[tokio::test]
async fn a_changed_profile_is_defined_again_and_a_bad_name_defines_none() {
    let rig = simulated("abm-reprofile", true, true, |sim| {
        sim.device("ONE", "empty", None);
        sim.device("DISOWNED", "empty", None);
    });
    done(&rig).await;
    let first = rig.store.abm_state().expect("state").expect("held");
    // A device ABM disowned later is not given the new profile.
    rig.sim.state().devices.retain(|d| d.serial != "DISOWNED");
    rig.sim
        .state()
        .log
        .push(("DISOWNED".to_owned(), "deleted".to_owned()));
    rig.setup
        .set_setting("ade_profile_name", "Other name")
        .expect("set");
    rig.sim.device("TWO", "empty", None);
    rig.sim
        .state()
        .log
        .push(("TWO".to_owned(), "added".to_owned()));
    let report = done(&rig).await;
    assert!(report.profile_defined);
    assert_eq!(report.assigned, 1);
    let second = rig.store.abm_state().expect("state").expect("held");
    assert_ne!(first.profile_uuid, second.profile_uuid);
    assert_ne!(first.profile_digest, second.profile_digest);
    let held = rows(&rig);
    assert_eq!(held["ONE"].profile_uuid, first.profile_uuid);
    assert_eq!(held["TWO"].profile_uuid, second.profile_uuid);
    assert!(!rig.abm.serial_in_abm("DISOWNED").expect("read"));
    assert_eq!(held["DISOWNED"].profile_uuid, first.profile_uuid);

    for bad in ["not a host", "-bad.example", "a..b", ""] {
        rig.setup.set_setting("mdm_name", bad).expect("set");
        let report = done(&rig).await;
        assert!(!report.profile_defined, "{bad:?}");
    }
    assert_eq!(rig.sim.state().profiles.len(), 2);
}

/// Proves: a device ABM does not give the profile (`NOT_ACCESSIBLE`) is counted, stays without
/// one in the table, and is tried again by the next pass, which assigns it once ABM does; and an
/// assignment ABM refuses with 404 to `POST` is made with `PUT`.
///
/// Catches: a device given up on after one refusal, and a table that says assigned for a device
/// ABM did not assign.
#[tokio::test]
async fn an_assignment_apple_refuses_is_tried_again_and_put_is_the_fallback() {
    let rig = simulated("abm-retry", true, true, |sim| {
        sim.device("OK", "empty", None);
        sim.device("LATER", "empty", None);
        sim.state().inaccessible = vec!["LATER".to_owned()];
        sim.state().refuse_assign = vec![hyper::Method::POST];
    });
    let report = done(&rig).await;
    assert_eq!((report.assigned, report.unassigned), (1, 1));
    assert!(
        rig.fake
            .calls()
            .contains(&"PUT /profile/devices".to_owned())
    );
    assert_eq!(rows(&rig)["LATER"].profile_uuid, None);
    assert_eq!(rows(&rig)["LATER"].profile_status, "empty");

    rig.sim.state().inaccessible.clear();
    let report = done(&rig).await;
    assert_eq!(
        (report.assigned, report.unassigned, report.profile_defined),
        (1, 0, false)
    );
    assert_eq!(rows(&rig)["LATER"].profile_status, "assigned");
}

/// Proves: what a pass had written stays when a later call fails: Apple refusing the assignment
/// leaves the devices and the new cursor in the table and the profile's uuid in the state, and the
/// error is Apple's; a refusal of the session is the credential fault and stops before any table
/// is written.
///
/// Catches: a pass that rolls back what it learned and so asks for it again, or one that writes a
/// half page.
#[tokio::test]
async fn a_failed_call_keeps_what_the_pass_had_written() {
    let rig = simulated("abm-partial", true, true, |sim| {
        sim.device("NEW", "empty", None);
        sim.state().refuse_assign = vec![hyper::Method::POST, hyper::Method::PUT];
    });
    let failed = rig.abm.sync_once(&rig.setup).await;
    assert_eq!(failed, Err(SyncError::Abm(AbmError::Status(404))));
    assert!(rows(&rig).contains_key("NEW"));
    let state = rig.store.abm_state().expect("state").expect("held");
    assert_eq!(state.cursor, "c0");
    assert!(state.profile_uuid.is_some());

    let rig = simulated("abm-fault", true, true, |sim| {
        sim.device("NEW", "empty", None);
        sim.state().session_fault = Some((403, "signature_invalid".to_owned()));
    });
    assert_eq!(
        rig.abm.sync_once(&rig.setup).await,
        Err(SyncError::Abm(AbmError::CredentialRefused))
    );
    assert!(rows(&rig).is_empty());
    assert_eq!(rig.store.abm_state().expect("state"), None);
}

/// Proves: a token held that is not the five parts is [`SyncError::Token`], a token the key does
/// not open (the setup holds another key) is a setup error, and a table that cannot be written is
/// a store error; none asks Apple for devices.
///
/// Catches: a sync that signs with half a token, and one that reports a lost key as a network
/// fault.
#[tokio::test]
async fn a_token_that_is_unreadable_or_a_store_that_fails_stops_the_pass() {
    let rig = simulated("abm-badtoken", false, false, |_| {});
    rig.setup
        .put_secret(SecretKind::AbmToken, &Secret::new(b"{}".to_vec()), None, 1)
        .expect("a bad token");
    assert_eq!(rig.abm.sync_once(&rig.setup).await, Err(SyncError::Token));
    assert!(rig.fake.seen().is_empty());

    let other_key = MdmSetup::new(
        // The store of the first setup, a key that did not seal it.
        Arc::new(FileSetup::volatile()),
        Some(MasterKey::from_bytes(&[4; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    let shared: Arc<dyn SetupStore> = Arc::new(FileSetup::volatile());
    let sealed_by_other = MdmSetup::new(
        Arc::clone(&shared),
        Some(MasterKey::from_bytes(&[5; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    sealed_by_other
        .put_secret(SecretKind::AbmToken, &Secret::new(token_json()), None, 1)
        .expect("sealed");
    let wrong_key = MdmSetup::new(
        shared,
        Some(MasterKey::from_bytes(&[6; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    let outcome = rig.abm.sync_once(&wrong_key).await;
    assert!(matches!(outcome, Err(SyncError::Setup(_))), "{outcome:?}");
    drop(other_key);

    let rig = simulated("abm-nostore", true, false, |sim| {
        sim.device("A", "empty", None)
    });
    std::fs::remove_dir_all(rig.dir.path().join("mdm")).expect("the directory goes");
    let outcome = rig.abm.sync_once(&rig.setup).await;
    assert!(matches!(outcome, Err(SyncError::Store(_))), "{outcome:?}");
}

/// Proves: the status keeps when the last good pass ended and the last error: a good pass sets the
/// first and clears the error, a failed one sets the error and keeps the time, and a pass with no
/// token changes neither.
///
/// Catches: a status that says healthy after a failure, or forgets when it last was.
#[tokio::test]
async fn the_status_keeps_the_last_good_pass_and_the_last_error() {
    let rig = simulated("abm-status", true, false, |sim| {
        sim.device("A", "pushed", Some("P"))
    });
    rig.abm.sync_noted(&rig.setup).await;
    let ok = rig.abm.status();
    assert_eq!((ok.last_ok_unix, ok.last_error), (Some(NOW), None));
    rig.sim.state().session_fault = Some((403, "timestamp_refused".to_owned()));
    rig.sim.state().session = None;
    rig.clock.store(NOW + 10, Ordering::SeqCst);
    rig.abm.sync_noted(&rig.setup).await;
    let failed = rig.abm.status();
    assert_eq!(failed.last_ok_unix, Some(NOW));
    assert_eq!(
        failed.last_error.as_deref(),
        Some(
            "ABM refused the timestamp (timestamp_refused): this machine's clock is off by an hour or more"
        )
    );
    rig.sim.state().session_fault = None;
    rig.abm.sync_noted(&rig.setup).await;
    let recovered = rig.abm.status();
    assert_eq!(
        (recovered.last_ok_unix, recovered.last_error),
        (Some(NOW + 10), None)
    );
}

/// Proves: the token proof opens a session and reads the account, answers who the token is for,
/// and passes on the faults Apple names; a setup whose settings cannot be read is a setup error.
///
/// Catches: a token kept before Apple has accepted it.
#[tokio::test]
async fn a_token_is_proved_by_a_session_and_the_account() {
    let rig = simulated("abm-prove", false, false, |_| {});
    let account = rig
        .abm
        .accept_token(&rig.setup, &sample_token())
        .await
        .expect("accepted");
    assert_eq!(
        (account.server_name.as_str(), account.org_name.as_str()),
        ("js-mdm", "Org")
    );
    assert_eq!(rig.fake.calls(), ["GET /session", "GET /account"]);
    rig.sim.state().session_fault = Some((403, "signature_invalid".to_owned()));
    rig.sim.state().session = None;
    // A second token: a new session is needed whatever the first one left.
    let other = crate::mdm_secrets::AbmToken {
        access_token: "x".to_owned(),
        ..sample_token()
    };
    assert_eq!(
        rig.abm.accept_token(&rig.setup, &other).await,
        Err(SyncError::Abm(AbmError::CredentialRefused))
    );

    let broken = MdmSetup::new(
        Arc::new(FailingSettings(FileSetup::volatile())),
        None,
        crate::enrollment::os_random(),
    );
    let outcome = rig.abm.accept_token(&broken, &sample_token()).await;
    assert!(matches!(outcome, Err(SyncError::Setup(_))), "{outcome:?}");
}

/// A setup store whose settings cannot be read.
struct FailingSettings(FileSetup);

impl SetupStore for FailingSettings {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        self.0.put_secret(row)
    }
    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        self.0.secrets()
    }
    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError> {
        self.0.prune_superseded(superseded_unix)
    }
    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        Err(SetupError::Store("unreadable".to_owned()))
    }
    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.0.set_setting(name, value)
    }
}

/// Proves: the timer runs a pass, then waits `abm_sync_s` seconds as the setting says now (300
/// when it is not set, the setting's value after it is set, 300 again when the settings cannot be
/// read), and then runs the next; nothing in the test waits.
///
/// Catches: a timer that reads its interval once at start, so that the operator's change waits for
/// a restart, and one that stops on an unreadable setting.
#[tokio::test]
async fn the_timer_runs_a_pass_and_waits_the_interval_of_the_setting() {
    let rig = simulated("abm-timer", true, false, |sim| {
        sim.device("A", "pushed", Some("P"))
    });
    let waits: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let reached = Arc::new(tokio::sync::Notify::new());
    let setup = Arc::clone(&rig.setup);
    let (seen, wake) = (Arc::clone(&waits), Arc::clone(&reached));
    let sleep: Sleep = Arc::new(move |wait| {
        let mut seen = seen.lock().expect("waits");
        seen.push(wait);
        let count = seen.len();
        drop(seen);
        // After the first wait the operator changes the setting; after the second the store fails.
        if count == 1 {
            setup.set_setting("abm_sync_s", "120").expect("set");
        }
        if count == 3 {
            wake.notify_one();
            return Box::pin(std::future::pending());
        }
        Box::pin(async {})
    });
    let abm = Arc::clone(&rig.abm);
    let timer = sync_every(abm, Arc::clone(&rig.setup), sleep);
    tokio::select! {
        never = timer => match never {},
        () = reached.notified() => {}
    }
    assert_eq!(
        *waits.lock().expect("waits"),
        [
            Duration::from_secs(300),
            Duration::from_secs(120),
            Duration::from_secs(120)
        ]
    );
    assert!(
        rig.fake
            .calls()
            .iter()
            .filter(|c| *c == "POST /devices/sync")
            .count()
            >= 2
    );
}

/// Proves: with the settings unreadable the timer still waits, 300 seconds.
///
/// Catches: a timer that stops for good, or spins, on a store that is down.
#[tokio::test]
async fn an_unreadable_interval_is_the_default() {
    let rig = simulated("abm-timer-down", false, false, |_| {});
    let broken = Arc::new(MdmSetup::new(
        Arc::new(FailingSettings(FileSetup::volatile())),
        None,
        crate::enrollment::os_random(),
    ));
    let waits: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let reached = Arc::new(tokio::sync::Notify::new());
    let (seen, wake) = (Arc::clone(&waits), Arc::clone(&reached));
    let sleep: Sleep = Arc::new(move |wait| {
        seen.lock().expect("waits").push(wait);
        wake.notify_one();
        Box::pin(std::future::pending())
    });
    // A pass with a setup whose token cannot be read ends in an error, which the timer logs.
    let timer = sync_every(Arc::clone(&rig.abm), broken, sleep);
    tokio::select! {
        never = timer => match never {},
        () = reached.notified() => {}
    }
    assert_eq!(*waits.lock().expect("waits"), [Duration::from_secs(300)]);
}

/// Proves: a record that says nothing of its profile status is a device that has none (`empty`),
/// and the fields it does carry are kept.
///
/// Catches: a device with a blank status that is never given a profile.
#[test]
fn a_record_with_no_status_is_empty() {
    let record: crate::abm::wire::DeviceRecord =
        serde_json::from_str(r#"{"serial_number":"S","model":"M","unknown":1}"#).expect("a record");
    let device = crate::abm::device_of(&record, 5);
    assert_eq!(
        (
            device.profile_status.as_str(),
            device.model.as_deref(),
            device.first_seen_unix
        ),
        ("empty", Some("M"), 5)
    );
}
