//! `Audit.List` of the coordinator over Postgres against the files.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::AT;
use super::coordinator::{Farm, exclude};
use crate::marks_support::{DB, WEB, operator};
use crate::ops::{AuditEntry, Write};
use crate::testutil::Rig;

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
