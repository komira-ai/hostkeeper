//! The holds in the ops file and the calls that set them: an ops file of the release before still
//! reads, a newer one is refused, and `Services.Hold`, `Unhold` and `List` refuse what they must,
//! replace a hold of the same scope, keep to the cap and say each scope in the audit.

use std::fs;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde::Deserialize;
use serde_json::Value;

use crate::fleet::Marks;
use crate::holds::{HoldMark, HoldScope, MAX_HOLDS, holding, marks_of};
use crate::marks_support::{
    DB, WEB, hold, operator, rig, scope_fleet, scope_host, scope_role, start, unhold,
};
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsError, OpsState, OpsStore, Write};
use crate::rollback_tests::unwritable;
use crate::testutil::{OPERATOR, Rig, ci_token, scratch, version};

/// A hold of `web` on the host `host`, as the ops file keeps it.
fn hold_mark(host: &str) -> HoldMark {
    HoldMark {
        service: WEB.to_owned(),
        scope: HoldScope::Host(host.to_owned()),
        reason: "investigating".to_owned(),
        by: "operator".to_owned(),
        at_unix: 1_800_000_000,
    }
}

/// A hold request with a reason of the caller's choosing.
fn hold_with(
    rig: &Rig,
    scope: Option<proto::HoldScope>,
    reason: &str,
) -> Result<proto::HoldResponse, proto::ServicesRefusal> {
    rig.coordinator.hold(
        &operator(),
        proto::HoldRequest {
            scope,
            service: WEB.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

// ---- the ops file -----------------------------------------------------------------------

/// The ops file of the release before this one: schema 1, no holds and no force.
const PREVIOUS: &str = r#"{
  "schema": 1,
  "hosts": {"n1": {"labels": {"zone": "a"}}},
  "excluded": [{"service": "web", "node_id": "n2", "reason": "disk replaced"}],
  "services": {"web": {"halt": null, "halt_acked": [], "last_good": "1.0", "bad": ["1.9"]}},
  "audit": [{"seq": 7, "by": "operator", "action": "exclude", "service": "web",
             "node_id": "n2", "reason": "disk replaced"}]
}
"#;

/// The reader of the release before this one: its ops file types, strict as they were
/// (`deny_unknown_fields`), which refuse a key they do not know.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreviousOps {
    schema: u32,
    #[serde(default)]
    hosts: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    excluded: Vec<Value>,
    #[serde(default)]
    services: std::collections::BTreeMap<String, PreviousService>,
    #[serde(default)]
    audit: Vec<Value>,
}

/// One service of [`PreviousOps`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousService {
    #[serde(default)]
    halt: Option<Value>,
    #[serde(default)]
    halt_acked: Vec<Value>,
    #[serde(default)]
    last_good: Option<Value>,
    #[serde(default)]
    bad: Vec<Value>,
    #[serde(default)]
    failure_acked: Vec<Value>,
}

impl PreviousService {
    /// How much the service holds.
    fn entries(&self) -> usize {
        usize::from(self.halt.is_some())
            + self.halt_acked.len()
            + usize::from(self.last_good.is_some())
            + self.bad.len()
            + self.failure_acked.len()
    }
}

impl PreviousOps {
    /// What the previous release makes of `text`: its state, or its refusal.
    pub(crate) fn read(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// How much the file holds.
    fn entries(&self) -> usize {
        usize::from(self.schema == 1)
            + self.hosts.len()
            + self.excluded.len()
            + self.audit.len()
            + self
                .services
                .values()
                .map(PreviousService::entries)
                .sum::<usize>()
    }
}

/// This build reads the ops file of the release before it with no holds and no force; a state it
/// writes with neither has none of the keys the release before refuses; a hold or a force adds
/// its key and the release before refuses the file, the safe side.
///
/// Catches: a new key written when it is empty, which would make a farm that never holds
/// anything unable to roll back to the release before, or a hold written where that release
/// would ignore it.
#[test]
fn an_ops_file_from_the_previous_release_still_reads() {
    assert!(
        PreviousOps::read(PREVIOUS)
            .expect("the previous release reads its own file")
            .entries()
            > 0
    );
    let dir = scratch("holds-previous-ops");
    let path = dir.path().join("desired.json.ops");
    fs::write(&path, PREVIOUS).expect("written");

    let store = OpsStore::open(&path).expect("this build reads the previous file");
    assert!(store.state().holds.is_empty());
    let marks = store.marks(WEB);
    assert_eq!(marks.last_good, Some(version("1.0")));
    assert_eq!(marks.bad, vec![version("1.9")]);
    assert!(marks.force.is_none());
    assert_eq!(marks.force_generation, 0);
    assert!(marks.bad_by_force.is_empty());

    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "exclude"),
            |_| {},
        )
        .expect("written");
    let text = fs::read_to_string(&path).expect("read");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    assert!(written.get("holds").is_none(), "{text}");
    let service = written["services"][WEB].as_object().expect("an object");
    for key in ["force", "force_generation", "bad_by_force"] {
        assert!(!service.contains_key(key), "{key} in {text}");
    }
    assert!(
        PreviousOps::read(&text)
            .expect("the previous release reads what this build wrote")
            .entries()
            > 0
    );

    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "hold"),
            |state| {
                state.holds.push(hold_mark("d1"));
            },
        )
        .expect("written");
    let text = fs::read_to_string(&path).expect("read");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(written["holds"].as_array().map(Vec::len), Some(1), "{text}");
    let why = PreviousOps::read(&text).expect_err("the previous release refuses a hold");
    assert!(why.contains("holds"), "{why}");

    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "unhold"),
            |state| {
                state.holds.clear();
                let marks = state.services.entry(WEB.to_owned()).or_default();
                marks.force_generation = 3;
                marks.bad_by_force.push(version("1.9"));
            },
        )
        .expect("written");
    let text = fs::read_to_string(&path).expect("read");
    let written: Value = serde_json::from_str(&text).expect("JSON");
    assert!(written.get("holds").is_none(), "{text}");
    let why = PreviousOps::read(&text).expect_err("the previous release refuses a force");
    assert!(
        why.contains("force_generation") || why.contains("bad_by_force"),
        "{why}"
    );
}

/// An ops file of a newer schema is refused, naming the newer schema.
///
/// Catches: a coordinator that starts over a file a later release wrote and ignores its holds.
#[test]
fn a_newer_schema_ops_file_is_refused() {
    let dir = scratch("holds-newer-ops");
    let path = dir.path().join("desired.json.ops");
    fs::write(&path, r#"{"schema":2,"holds":[]}"#).expect("written");
    let why = OpsStore::open(&path).err().expect("refused");
    assert!(matches!(why, OpsError::Newer { found: 2, .. }), "{why}");
}

// ---- the calls --------------------------------------------------------------------------

/// A hold of the same service and scope replaces the reason and stays one hold; the same scope
/// of another service, and another scope of the service, are other holds.
///
/// Catches: a second hold appended for the same pair, or one that replaces a hold of another
/// service or scope.
#[tokio::test]
async fn a_hold_of_the_same_service_and_scope_replaces_the_reason() {
    let rig = rig("holds-replace", "");
    let first = hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(first.hold.expect("a hold").reason, "investigating");
    let second = hold_with(&rig, scope_host("d1"), "  a better reason  ").expect("held");
    let shown = second.hold.expect("a hold");
    assert_eq!(shown.scope, scope_host("d1"));
    assert_eq!(shown.service, WEB);
    assert_eq!(shown.reason, "a better reason");
    assert_eq!(shown.set_by, "operator");
    assert!(shown.set_unix > 0);
    assert_eq!(
        second.entry.expect("an entry").action,
        "hold",
        "the replacement is audited"
    );

    let holds = rig.coordinator.ops().read(|state| state.holds.clone());
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].reason, "a better reason");

    hold(&rig, DB, scope_host("d1")).expect("held");
    hold(&rig, WEB, scope_host("d2")).expect("held");
    assert_eq!(rig.coordinator.ops().read(|state| state.holds.len()), 3);
}

/// A hold past [`MAX_HOLDS`] is refused as an invalid request and changes nothing; a hold that
/// replaces one at the cap is accepted.
///
/// Catches: an unbounded list of holds, or a cap that refuses a replacement.
#[tokio::test]
async fn a_hold_past_the_cap_is_refused_and_a_replacing_hold_at_the_cap_is_accepted() {
    let rig = rig("holds-cap", "");
    rig.coordinator
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "hold"),
            |state| {
                state
                    .holds
                    .extend((0..MAX_HOLDS).map(|n| hold_mark(&format!("h{n}"))));
            },
        )
        .expect("seeded");
    let count = || rig.coordinator.ops().read(|state| state.holds.len());
    assert_eq!(count(), MAX_HOLDS);

    for (service, host) in [(WEB, "extra"), (DB, "h0")] {
        let refusal = hold(&rig, service, scope_host(host)).expect_err("past the cap");
        assert_eq!(refusal, proto::ServicesRefusal::InvalidRequest, "{service}");
    }
    assert_eq!(count(), MAX_HOLDS, "a refused hold changes nothing");

    hold_with(&rig, scope_host("h0"), "still looking").expect("a replacement fits");
    assert_eq!(count(), MAX_HOLDS);
    let reason = rig.coordinator.ops().read(|state| {
        state
            .holds
            .iter()
            .find(|h| h.scope == HoldScope::Host("h0".to_owned()))
            .map(|h| h.reason.clone())
    });
    assert_eq!(reason.as_deref(), Some("still looking"));

    unhold(&rig, WEB, scope_host("h1")).expect("released");
    hold(&rig, WEB, scope_host("extra")).expect("there is room again");
}

/// An unhold of a scope that is not held is refused as `not_held`, and a hold of another scope
/// or service does not satisfy it.
///
/// Catches: an unhold that succeeds on nothing, or one that matches a hold of another scope.
#[tokio::test]
async fn unhold_of_a_scope_not_held_is_refused() {
    let rig = rig("holds-not-held", "");
    let not_held = proto::ServicesRefusal::NotHeld;
    assert_eq!(
        unhold(&rig, WEB, scope_host("d1")).expect_err("nothing held"),
        not_held
    );
    hold(&rig, WEB, scope_fleet()).expect("held");
    for scope in [scope_host("d1"), scope_role("daemon")] {
        assert_eq!(
            unhold(&rig, WEB, scope).expect_err("another scope"),
            not_held
        );
    }
    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(
        unhold(&rig, DB, scope_host("d1")).expect_err("another service"),
        not_held
    );
    unhold(&rig, WEB, scope_fleet()).expect("released");
    assert_eq!(
        unhold(&rig, WEB, scope_fleet()).expect_err("already released"),
        not_held
    );
    assert_eq!(rig.coordinator.ops().read(|state| state.holds.len()), 1);
}

/// A hold or an unhold of a service the coordinator does not manage is `unknown_service`, before
/// the scope is looked at.
///
/// Catches: a hold recorded for a service no definition names.
#[tokio::test]
async fn a_hold_or_unhold_of_an_unknown_service_is_refused() {
    let rig = rig("holds-unknown-service", "");
    for scope in [scope_host("d1"), None] {
        assert_eq!(
            hold(&rig, "nope", scope.clone()).expect_err("unknown"),
            proto::ServicesRefusal::UnknownService
        );
        assert_eq!(
            unhold(&rig, "nope", scope).expect_err("unknown"),
            proto::ServicesRefusal::UnknownService
        );
    }
    assert!(rig.coordinator.ops().read(|state| state.holds.is_empty()));
}

/// A scope that is missing, empty or malformed, and a reason that is empty, too long or holds a
/// control character, are invalid requests that change nothing; a reason of the longest
/// allowed length is accepted.
///
/// Catches: a hold kept for a scope no host can match, or an unbounded or unprintable reason.
#[tokio::test]
async fn a_bad_scope_or_reason_is_an_invalid_request() {
    let rig = rig("holds-invalid", "");
    let invalid = proto::ServicesRefusal::InvalidRequest;
    for scope in [
        None,
        Some(proto::HoldScope { scope: None }),
        scope_host("Not A Host"),
        scope_host(""),
        scope_role("Bad_Role"),
        scope_role(""),
    ] {
        assert_eq!(
            hold(&rig, WEB, scope.clone()).expect_err("bad scope"),
            invalid,
            "{scope:?}"
        );
        assert_eq!(
            unhold(&rig, WEB, scope.clone()).expect_err("bad scope"),
            invalid,
            "{scope:?}"
        );
    }
    let too_long = "x".repeat(257);
    for reason in ["", "   ", "line\nbreak", too_long.as_str()] {
        assert_eq!(
            hold_with(&rig, scope_host("d1"), reason).expect_err("bad reason"),
            invalid,
            "{reason:?}"
        );
    }
    assert!(rig.coordinator.ops().read(|state| state.holds.is_empty()));
    hold_with(&rig, scope_host("d1"), &"x".repeat(256)).expect("the longest reason");
}

/// Only the operator token holds, unholds and lists: a CI token is `forbidden`, no token
/// `unauthorized`.
///
/// Catches: a hold made with a CI token or without one.
#[tokio::test]
async fn a_caller_that_is_no_operator_may_not_hold() {
    let rig = rig("holds-callers", "");
    let token = ci_token();
    let ci = rig.coordinator.caller(Some(token.as_str())).await;
    let anonymous = rig.coordinator.caller(None).await;
    let named = rig.coordinator.caller(Some(OPERATOR)).await;
    assert!(matches!(ci, Caller::Ci), "{ci:?}");
    assert!(matches!(anonymous, Caller::Anonymous), "{anonymous:?}");
    assert!(
        matches!(&named, Caller::Operator(name) if name.as_str() == "operator"),
        "{named:?}"
    );

    for (caller, refusal) in [
        (ci, proto::ServicesRefusal::Forbidden),
        (anonymous, proto::ServicesRefusal::Unauthorized),
    ] {
        let request = proto::HoldRequest {
            scope: scope_host("d1"),
            service: WEB.to_owned(),
            reason: "investigating".to_owned(),
        };
        assert_eq!(
            rig.coordinator.hold(&caller, request).expect_err("refused"),
            refusal
        );
        let request = proto::UnholdRequest {
            scope: scope_host("d1"),
            service: WEB.to_owned(),
            reason: "done".to_owned(),
        };
        assert_eq!(
            rig.coordinator
                .unhold(&caller, request)
                .expect_err("refused"),
            refusal
        );
        assert_eq!(
            rig.coordinator
                .list_services(&caller, proto::ListServicesRequest {})
                .expect_err("refused"),
            refusal
        );
    }
    assert!(rig.coordinator.ops().read(|state| state.holds.is_empty()));
}

/// `Services.List` lists every service in name order with the holds of each, and every hold, each
/// with its scope, service, reason, the name of the token that set it and the time.
///
/// Catches: holds listed under the wrong service, services out of name order, or a hold's fields
/// lost on the way to the message.
#[tokio::test]
async fn list_services_lists_every_service_in_name_order_with_its_holds() {
    let rig = rig("holds-list", "");
    hold(&rig, WEB, scope_host("d1")).expect("held");
    hold(&rig, WEB, scope_role("storage")).expect("held");
    hold(&rig, DB, scope_fleet()).expect("held");

    let listed = rig
        .coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");
    let names: Vec<&str> = listed.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(names, ["db", "web"]);
    assert_eq!(listed.services[0].holds.len(), 1);
    assert_eq!(listed.services[0].holds[0].scope, scope_fleet());
    assert_eq!(listed.services[1].holds.len(), 2);
    assert!(
        listed
            .services
            .iter()
            .all(|s| s.hosts.is_empty() && s.min_versions.is_empty())
    );

    assert_eq!(listed.holds.len(), 3);
    let scopes: Vec<_> = listed.holds.iter().map(|h| h.scope.clone()).collect();
    assert_eq!(
        scopes,
        [scope_host("d1"), scope_role("storage"), scope_fleet()]
    );
    let services: Vec<&str> = listed.holds.iter().map(|h| h.service.as_str()).collect();
    assert_eq!(services, [WEB, WEB, DB]);
    for mark in &listed.holds {
        assert_eq!(mark.reason, "investigating");
        assert_eq!(mark.set_by, "operator");
        assert!(mark.set_unix > 0);
    }
}

/// A hold and an unhold are audited with the scope in words (`host d1`, `role storage`,
/// `fleet`) and the operator's reason, a host scope naming the host; the scopes describe
/// themselves the same way, cover the hosts they name, and `holding` and `marks_of` read the
/// holds of a service back.
///
/// Catches: an audit entry without the scope, a scope that covers the wrong host, or a copy of
/// the holds that mixes the services.
#[tokio::test]
async fn a_hold_names_its_scope_in_the_audit() {
    let rig = rig("holds-audit", "");
    hold(&rig, WEB, scope_host("d1")).expect("held");
    hold(&rig, WEB, scope_role("storage")).expect("held");
    hold(&rig, DB, scope_fleet()).expect("held");
    unhold(&rig, WEB, scope_host("d1")).expect("released");

    let entries = |action: &str| {
        rig.coordinator
            .audit_list(
                &operator(),
                proto::ListAuditRequest {
                    action: Some(action.to_owned()),
                    ..Default::default()
                },
            )
            .expect("listed")
            .entries
    };
    let held = entries("hold");
    let reasons: Vec<Option<String>> = held.iter().map(|e| e.reason.clone()).collect();
    assert_eq!(
        reasons,
        [
            Some("host d1: investigating".to_owned()),
            Some("role storage: investigating".to_owned()),
            Some("fleet: investigating".to_owned()),
        ]
    );
    let nodes: Vec<Option<String>> = held.iter().map(|e| e.node_id.clone()).collect();
    assert_eq!(nodes, [Some("d1".to_owned()), None, None]);
    let released = entries("unhold");
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].reason.as_deref(), Some("host d1: done"));
    assert_eq!(released[0].node_id.as_deref(), Some("d1"));

    let host = HoldScope::Host("d1".to_owned());
    let storage = HoldScope::Role("storage".to_owned());
    assert_eq!(host.describe(), "host d1");
    assert_eq!(storage.describe(), "role storage");
    assert_eq!(HoldScope::Fleet.describe(), "fleet");
    assert!(host.covers("d1", "daemon") && !host.covers("d2", "daemon"));
    assert!(storage.covers("d9", "storage") && !storage.covers("d9", "daemon"));
    assert!(HoldScope::Fleet.covers("d9", "anything"));
    assert!(
        HoldScope::Role("all".to_owned()).covers("d9", "anything"),
        "all is every host"
    );

    let state = rig.coordinator.ops().state();
    let found = |service: &str, node: &str, of_role: &str| {
        holding(&state.holds, service, node, of_role)
            .map(|h| h.scope.describe())
            .collect::<Vec<String>>()
    };
    assert_eq!(found(WEB, "d1", "daemon"), Vec::<String>::new());
    assert_eq!(found(WEB, "s1", "storage"), ["role storage"]);
    assert_eq!(found(DB, "d1", "daemon"), ["fleet"]);
    assert_eq!(
        marks_of(&state),
        Marks {
            holds: state.holds.clone(),
            ..Marks::default()
        }
    );
}

/// A hold or an unhold whose ops file cannot be written is `store_failed` and changes nothing:
/// the hold list is as it was.
///
/// Catches: a hold that stands in memory after its write failed, or an unhold that releases a
/// pair the file still holds.
#[tokio::test]
async fn a_hold_that_cannot_be_written_is_store_failed_and_changes_nothing() {
    let mut rig = rig("holds-store-failed", "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(OpsState::default()));
    assert_eq!(
        hold(&rig, WEB, scope_host("d1")).expect_err("not written"),
        proto::ServicesRefusal::StoreFailed
    );
    assert!(rig.coordinator.ops().read(|state| state.holds.is_empty()));

    let mut state = OpsState::default();
    state.holds.push(hold_mark("d1"));
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(state));
    assert_eq!(
        unhold(&rig, WEB, scope_host("d1")).expect_err("not written"),
        proto::ServicesRefusal::StoreFailed
    );
    assert_eq!(
        hold(&rig, DB, scope_host("d1")).expect_err("not written"),
        proto::ServicesRefusal::StoreFailed
    );
    let listed = rig
        .coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");
    assert_eq!(listed.holds.len(), 1);
    assert_eq!(listed.holds[0].service, WEB);
}
