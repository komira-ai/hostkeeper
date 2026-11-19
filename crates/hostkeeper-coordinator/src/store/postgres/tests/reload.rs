//! The copies of the parts and their versions (`docs/design/coordinator-multi-instance.md`,
//! section 6, "Invalidation" and "Reload rule"): a part is read again with its version in one
//! snapshot, so its copy never carries a version newer than its rows; a request reads again every
//! part another coordinator changed, and no other; a copy that holds a change not yet written is
//! kept until the change lands.
//!
//! A commit between the read of a version and the read of the rows is made by the store's own hook
//! between them ([`Postgres::between_reads`]). Nothing sleeps and no time is measured.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};

use super::coordinator::{Holding, start_on_postgres};
use super::edits::{Refused, instance, label, labels, version};
use super::{AT, TestDatabase, admin, options};
use crate::grpc::route;
use crate::http::Surface;
use crate::ops::OpsStore;
use crate::store::conformance_tests::rich_accounts;
use crate::store::postgres::Postgres;
use crate::store::{Backend, Part};
use crate::testutil::{OPERATOR, Rig, desired};

/// Commits, beside the store, a label of `host-2` and the ops part's version moved on, as another
/// coordinator's edit does, on the database `name`.
fn label_beside(name: String) -> impl FnOnce() + Send + 'static {
    move || {
        admin(Some(&name))
            .batch_execute(
                "SET search_path TO hostkeeper; BEGIN; \
                 INSERT INTO host_labels (node_id, key, value) VALUES ('host-2', 'rack', 'r2'); \
                 UPDATE state_versions SET version = version + 1 WHERE part = 'ops'; COMMIT",
            )
            .expect("the other coordinator's edit commits");
    }
}

/// The labels (host, key, value) of `rows`.
fn labelled(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|(host, key, value)| ((*host).to_owned(), (*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// Proves: a copy of the operations state read again while another coordinator commits a change
/// of it between the read of the version and the read of the rows holds the rows of the version it
/// read, not the change: it is labelled with that version, so the next look at the versions finds
/// it behind and reads it again, and then it holds the change.
///
/// Catches: a reload that reads the rows and the version in two snapshots, which labels the old
/// rows with the new version, so no later look finds the copy behind and the change is never seen
/// (until the next change of the part).
#[test]
fn cache_is_never_labelled_newer_than_its_content() {
    let db = TestDatabase::new();
    let (a, b) = (instance(&db, "a"), instance(&db, "b"));
    label(&b.ops, "host-1", "rack", "r1");
    let read_at = version(&db, Part::Ops);
    a.store.between_reads(label_beside(db.name().to_owned()));
    a.ops.refresh();
    assert_eq!(
        labels(&a.ops.state()),
        labelled(&[("host-1", "rack", "r1")])
    );
    assert_eq!(version(&db, Part::Ops), read_at + 1);
    assert_eq!(
        a.store.stale(),
        [Part::Ops],
        "the copy carries the version it read"
    );

    a.ops.refresh();
    let both = labelled(&[("host-1", "rack", "r1"), ("host-2", "rack", "r2")]);
    assert_eq!(labels(&a.ops.state()), both);
    assert_eq!(a.store.stale(), []);
    a.ops.refresh();
    assert_eq!(labels(&a.ops.state()), both, "a copy not behind is kept");
}

/// Proves: a request answered by a coordinator first reads again each part another coordinator
/// changed since its copy was read (the desired state, the operations state, the accounts, the
/// layout and the verdicts), and answers from them: the status it answers holds the other's
/// desired version and label, and its accounts are the other's. After it the copies carry the
/// database's versions, and a second request answers the same.
///
/// Catches: a coordinator that answers from the copies it loaded at start, so an edit another
/// coordinator committed is never seen by its requests, its heartbeats among them.
#[test]
fn a_request_reads_again_the_parts_another_instance_changed() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("serving").expect("the store opens");
    let rig = Rig::new("a_request_reads_again");
    let marker = rig.dir.path().join("halt.marker");
    let serving = Arc::new(start_on_postgres(&rig, &store, loaded, &marker));
    let other = instance(&db, "other");
    other
        .desired
        .set("web", desired("2.0", 1, 1))
        .expect("the other sets web");
    label(&other.ops, "host-1", "rack", "r1");
    other
        .accounts
        .change(|_| Ok(rich_accounts()))
        .expect("the other sets the accounts");
    other
        .layout
        .change(|held| Ok((held.with_name("lab-b").expect("a name"), ())))
        .expect("the other names the layout");
    let report = hostkeeper_wire::wire::UpdateResult {
        order_id: hostkeeper_wire::wire::OrderId::parse("order-1"),
        order_try: Some(1),
        ..hostkeeper_wire::wire::UpdateResult::new(
            crate::testutil::version("2.0"),
            hostkeeper_wire::wire::Outcome::Failed,
        )
    };
    other
        .verdicts
        .record(
            &crate::testutil::id("host-1"),
            &[("web".to_owned(), Some(report))],
            &[],
        )
        .expect("the other acknowledges a verdict");
    let mut stale = store.stale();
    stale.sort();
    assert_eq!(stale, Part::ALL);

    let status = answer(&serving, "/v1/status");
    assert!(status.contains("\"2.0\""), "{status}");
    assert!(status.contains("\"rack\""), "{status}");
    assert_eq!(store.stale(), []);
    assert_eq!(serving.accounts().snapshot(), rich_accounts());
    assert_eq!(serving.layout().snapshot().name(), "lab-b");
    assert!(
        serving
            .verdicts
            .standing("web", &crate::testutil::id("host-1"))
            .is_some()
    );
    assert_eq!(
        serving
            .desired
            .get("web")
            .map(|d| d.version.as_str().to_owned()),
        Some("2.0".to_owned())
    );
    let ops_at = version(&db, Part::Ops);
    let again = answer(&serving, "/v1/status");
    assert_eq!(again, status, "a second request reads the copies it holds");
    assert_eq!(version(&db, Part::Ops), ops_at);
}

/// The body `serving` answers a `GET` of `path` with, by the operator.
fn answer(serving: &Arc<crate::service::Coordinator>, path: &str) -> String {
    let request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let serving = Arc::clone(serving);
    runtime.block_on(async move {
        let response = route(
            &serving,
            Surface::Control,
            &None,
            request,
            Duration::from_secs(5),
        )
        .await;
        let body = response
            .into_body()
            .collect()
            .await
            .expect("the body")
            .to_bytes();
        String::from_utf8_lossy(&body).into_owned()
    })
}

/// Proves: a coordinator with a change pending in memory (a halt made while the gate was not free)
/// keeps its memory when another coordinator changes the operations state: a read of the part again
/// leaves the halt standing. When the halt lands, only its own rows are written, so the other's
/// label stays, and the next read takes the other's label into memory beside the halt.
///
/// Catches: a read of the part again that drops a change memory holds and the rows do not yet,
/// and a pending change's commit that writes memory's whole copy over the other's rows.
#[test]
fn a_change_pending_in_memory_is_kept_until_it_lands() {
    let db = TestDatabase::new();
    let short = crate::store::postgres::Options {
        gate_wait: Duration::from_millis(50),
        ..options()
    };
    let (store, loaded): (Postgres, _) = db.open_options(&short, "a").expect("the store opens");
    let ops = OpsStore::over(Arc::new(store.clone()), loaded.ops).with_unix(Arc::new(|| AT));
    let other = instance(&db, "b");
    let holder = Holding::start(
        &db,
        "BEGIN; SELECT 1 FROM hostkeeper.rollout_gate WHERE id = 1 FOR UPDATE",
        "COMMIT",
    );
    let target = crate::testutil::version("1.0");
    let raised = crate::halt::raise_by_operator(&ops, "web", &target, "alice", "stop", Refused);
    assert!(raised.is_err());
    holder.release();
    label(&other.ops, "host-1", "rack", "r1");
    assert_eq!(store.stale(), [Part::Ops]);

    ops.refresh();
    assert!(ops.marks("web").halt.is_some(), "the pending halt stands");
    assert_eq!(labels(&ops.state()), []);

    ops.retry_unwritten(Duration::ZERO);
    assert!(!ops.halt_unwritten());
    assert_eq!(
        db.rows("SELECT node_id FROM host_labels")[0][0].as_deref(),
        Some("host-1")
    );
    assert_eq!(db.rows("SELECT service FROM halts").len(), 1);
    assert_eq!(store.stale(), [Part::Ops]);
    ops.refresh();
    assert!(ops.marks("web").halt.is_some());
    assert_eq!(labels(&ops.state()), labelled(&[("host-1", "rack", "r1")]));
}
