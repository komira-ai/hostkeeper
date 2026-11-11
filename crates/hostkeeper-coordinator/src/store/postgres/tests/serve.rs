//! The coordinator over Postgres as `serve` runs it, and `Audit.List` against the files.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_tls::TlsSource;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};

use super::AT;
use super::coordinator::{DAEMON, Farm, Holding, daemon, exclude};
use crate::config::{Parts, StartError, run};
use crate::lock::LockGuard;
use crate::marks_support::{DB, WEB, operator, who};
use crate::ops::{AuditEntry, Write};
use crate::store::{Backend, Health};
use crate::testutil::Rig;

/// A TLS source over a certificate for `localhost` of a CA of its own: what the listeners of a
/// serve that accepts no connection are given.
fn tls() -> TlsSource {
    let mut authority = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    authority.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let issuer = CertifiedIssuer::self_signed(authority, KeyPair::generate().expect("a CA key"))
        .expect("a CA certificate");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = vec![SanType::DnsName(
        "localhost".to_owned().try_into().expect("a DNS name"),
    )];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().expect("a key");
    let cert = params.signed_by(&key, &issuer).expect("signed");
    TlsSource::fixed(
        "test",
        cert.pem().as_bytes(),
        key.serialize_pem().as_bytes(),
        None,
    )
    .expect("a TLS source")
}

/// Proves: a coordinator that `serve` runs over Postgres stops when its store is deposed: with
/// its session lost and another coordinator's epoch raised meanwhile, the revival finds the
/// store deposed, the next heartbeat deposes the stores, and the serving future ends with
/// `StartError::Deposed` naming the epoch held and the one found, by which `serve` exits
/// non-zero.
///
/// Catches: a serve that goes on with a stale memory over a database another coordinator led,
/// or whose exit does not say which epochs it saw.
#[tokio::test]
async fn a_serve_whose_store_is_deposed_ends_with_the_epochs() {
    let farm = Farm::plain("pg-serve-deposed").seeded().await;
    let Farm {
        rig,
        store,
        database,
        ..
    } = farm;
    let Rig { coordinator, .. } = rig;
    let coordinator = Arc::new(coordinator);
    let held = store.epoch().expect("an epoch");
    let source = tls();
    let parts = Parts {
        coordinator: Arc::clone(&coordinator),
        heartbeat_tls: source.clone(),
        control_tls: source,
        heartbeat_listen: "127.0.0.1:1".parse().expect("an address"),
        control_listen: "127.0.0.1:2".parse().expect("an address"),
        lan_listen: None,
        mdm_listen: None,
        health_listen: None,
        reload: Duration::from_secs(3_600),
        lock: LockGuard::new(()),
    };
    // Listeners that accept nothing: the serve is driven by the coordinator's own calls.
    let bind = |addr: SocketAddr| async move {
        let accept = || std::future::pending::<io::Result<tokio::io::DuplexStream>>();
        Ok::<_, io::Error>((addr, accept))
    };
    let (bound, ready) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(run(parts, bind, move |_| {
        let _ = bound.send(());
    }));
    ready.await.expect("the listeners are bound");

    database.end_sessions();
    let another = Holding::another_led(&database);
    assert_eq!(
        coordinator.exclude(&operator(), exclude("d1")),
        Err(proto::HostsRefusal::StoreFailed)
    );
    another.release();
    store.revive();
    store.on_writer(|_| ()).expect("the writer runs");
    assert_eq!(store.health(), Health::Deposed { found: held + 1 });

    coordinator
        .heartbeat(who("d1", DAEMON), daemon("1.0"))
        .expect("answered");
    let ended = serving.await.expect("the serve's task");
    assert!(
        matches!(ended, Err(StartError::Deposed { epoch, found }) if epoch == held && found == held + 1),
        "{ended:?}"
    );
}

/// One change by `by` of `action` on `service` for `node`, through the ops store.
fn record(rig: &Rig, by: &str, action: &str, service: &str, node: Option<&str>) {
    let mut entry = AuditEntry::new(by, action);
    entry.service = Some(service.to_owned());
    entry.node_id = node.map(str::to_owned);
    rig.coordinator
        .ops()
        .change(Write::BeforeMemory, entry, |_| {})
        .expect("a recorded change");
}

/// Proves: `Audit.List` answers the same from the files and from the database: the same audited
/// changes, listed under every filter (actor, name, action, service, node, since, newest, limit),
/// give equal answers.
///
/// Catches: a filter the database applies differently from the files, an order or a page edge
/// that differs, or an entry whose time or name the database does not keep.
#[test]
fn audit_list_answers_the_same_from_files_and_from_the_database() {
    let mut files = crate::marks_support::rig("pg-audit-files", "");
    files.coordinator = crate::marks_support::start(&files.dir, &files.clock, "")
        .with_ops(crate::testutil::ops_of(&files.dir).with_unix(Arc::new(|| AT)));
    let farm = Farm::plain("pg-audit-database");

    let changes = [
        ("alice", "exclude", WEB, Some("d1")),
        ("bob", "exclude", DB, Some("d2")),
        ("alice", "hold", WEB, None),
        ("bob", "hold", DB, None),
        ("ci", "set_desired", WEB, None),
        ("coordinator", "last_good", DB, None),
        ("alice", "exclude", DB, Some("d1")),
    ];
    for (by, action, service, node) in changes {
        record(&files, by, action, service, node);
        record(&farm.rig, by, action, service, node);
    }

    let base = proto::ListAuditRequest::default;
    let requests = [
        base(),
        proto::ListAuditRequest {
            actor: Some("operator".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            actor: Some("ci".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            actor: Some("coordinator".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            by_name: Some("alice".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            action: Some("exclude".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            service: Some(DB.to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            node_id: Some("d1".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            since_seq: 3,
            ..base()
        },
        proto::ListAuditRequest {
            newest: true,
            limit: 3,
            ..base()
        },
        proto::ListAuditRequest {
            limit: 2,
            since_seq: 1,
            ..base()
        },
        proto::ListAuditRequest {
            newest: true,
            limit: 2,
            since_seq: 2,
            action: Some("hold".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            by_name: Some("alice".to_owned()),
            service: Some(WEB.to_owned()),
            action: Some("exclude".to_owned()),
            node_id: Some("d1".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            by_name: Some("carol".to_owned()),
            ..base()
        },
        proto::ListAuditRequest {
            since_seq: 99,
            ..base()
        },
    ];
    for request in requests {
        let from_files = files
            .coordinator
            .audit_list(&operator(), request.clone())
            .expect("a page from the files");
        let from_database = farm
            .rig
            .coordinator
            .audit_list(&operator(), request.clone())
            .expect("a page from the database");
        assert_eq!(from_database, from_files, "{request:?}");
    }
    assert_eq!(
        farm.rig
            .coordinator
            .audit_list(&operator(), base())
            .expect("a page")
            .entries
            .len(),
        changes.len()
    );
}
