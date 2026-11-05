//! The rules of assignment as pure functions ([`crate::assignment`]): which entry wins, the
//! minimum version that applies to a host, what each change writes, which hosts an `Unassign`
//! orders to uninstall, and the roles a service is listed with.

use std::collections::BTreeMap;

use crate::assignment::{
    Change, Entry, Known, NotAssigned, Placement, Scope, ServiceAssignments, apply, assigned,
    by_rollout, effective_min_version, roles_of, set_min_version,
};
use crate::services::{ServiceDef, Services};
use crate::testutil::version;

/// `web` on the roles `daemon` and `storage`, with `extra` lines of its definition.
fn web(extra: &str) -> ServiceDef {
    Services::parse(&format!(
        "[[service]]\nname = \"web\"\nroles = [\"daemon\", \"storage\"]\n{extra}\n"
    ))
    .expect("services")
    .get("web")
    .cloned()
    .expect("web")
}

/// A host the coordinator heard from: its role, its labels, and whether it lists the service.
fn known(node_id: &str, role: &str, lists: bool) -> Known {
    Known {
        node_id: node_id.to_owned(),
        role: role.to_owned(),
        labels: None,
        lists,
    }
}

fn at<'a>(node_id: &'a str, role: &'a str) -> Placement<'a> {
    Placement {
        node_id,
        role,
        labels: None,
    }
}

fn host(id: &str) -> Scope {
    Scope::Host(id.to_owned())
}

fn role(name: &str) -> Scope {
    Scope::Role(name.to_owned())
}

/// Catches: an entry of the role standing before the host's own, the rollout file standing
/// before an entry, or a selector's labels ignored in the base.
#[test]
fn a_host_entry_stands_before_its_roles_and_the_roles_before_the_rollout_file() {
    let def = web("");
    assert!(assigned(&def, None, &at("d1", "daemon")));
    assert!(!assigned(&def, None, &at("f1", "frontend")));
    let mut entries = ServiceAssignments::default();
    entries.roles.insert("daemon".to_owned(), Entry::Exclude);
    entries.roles.insert("frontend".to_owned(), Entry::Include);
    entries.hosts.insert("d1".to_owned(), Entry::Include);
    entries.hosts.insert("f2".to_owned(), Entry::Exclude);
    assert!(assigned(&def, Some(&entries), &at("d1", "daemon")));
    assert!(!assigned(&def, Some(&entries), &at("d2", "daemon")));
    assert!(assigned(&def, Some(&entries), &at("f1", "frontend")));
    assert!(!assigned(&def, Some(&entries), &at("f2", "frontend")));
    assert!(assigned(&def, Some(&entries), &at("s1", "storage")));

    let zoned = web("[service.selector]\nlabels = { zone = \"a\" }");
    let zone_a = BTreeMap::from([("zone".to_owned(), "a".to_owned())]);
    let labelled = Placement {
        node_id: "d3",
        role: "daemon",
        labels: Some(&zone_a),
    };
    assert!(by_rollout(&zoned, &labelled));
    assert!(!by_rollout(&zoned, &at("d3", "daemon")));
    assert!(assigned(&zoned, None, &labelled));
}

/// Catches: a role's minimum standing before the host's own, a minimum set below the rollout
/// file's `min_version` lowering the floor, or a floor that appears from nowhere.
#[test]
fn the_minimum_of_a_host_stands_before_its_roles_and_never_below_the_rollout_files() {
    let def = web("");
    assert_eq!(effective_min_version(&def, None, &at("d1", "daemon")), None);
    let mut entries = ServiceAssignments::default();
    entries
        .role_min_versions
        .insert("daemon".to_owned(), version("1.5"));
    entries
        .host_min_versions
        .insert("d1".to_owned(), version("2.0"));
    let min = |id: &str, of: &str| {
        effective_min_version(&def, Some(&entries), &at(id, of)).map(ToString::to_string)
    };
    assert_eq!(min("d1", "daemon").as_deref(), Some("2.0"));
    assert_eq!(min("d2", "daemon").as_deref(), Some("1.5"));
    assert_eq!(min("s1", "storage"), None);

    let floored = web("min_version = \"1.8\"");
    let min = |id: &str, of: &str| {
        effective_min_version(&floored, Some(&entries), &at(id, of)).map(ToString::to_string)
    };
    assert_eq!(min("d1", "daemon").as_deref(), Some("2.0"));
    assert_eq!(
        min("d2", "daemon").as_deref(),
        Some("1.8"),
        "never below the floor"
    );
    assert_eq!(min("s1", "storage").as_deref(), Some("1.8"));
}

/// Catches: a host assigned by its role alone after an `Assign` of it (so that taking the role
/// off would take it off too), a role entry written where the rollout file already assigns the
/// role, or an earlier removal left standing under the new assignment.
#[test]
fn an_assign_writes_the_hosts_entry_and_only_the_role_entries_the_rollout_file_does_not_make() {
    let def = web("");
    let mut entries = ServiceAssignments::default();
    let hosts = [known("d1", "daemon", true), known("f1", "frontend", false)];
    apply(&def, &mut entries, &Change::Assign(host("d1")), &hosts).expect("assigned");
    assert_eq!(entries.hosts.get("d1"), Some(&Entry::Include));
    let off = Change::Unassign {
        scope: role("daemon"),
        uninstall: true,
    };
    assert_eq!(apply(&def, &mut entries, &off, &hosts), Ok(Vec::new()));
    assert!(assigned(&def, Some(&entries), &at("d1", "daemon")));
    assert!(entries.uninstall.is_empty());
    entries = ServiceAssignments::default();
    apply(&def, &mut entries, &Change::Assign(host("f1")), &hosts).expect("assigned");
    assert_eq!(entries.hosts.get("f1"), Some(&Entry::Include));
    apply(&def, &mut entries, &Change::Assign(role("daemon")), &hosts).expect("assigned");
    assert!(
        entries.roles.is_empty(),
        "the rollout file runs it on daemon"
    );
    apply(
        &def,
        &mut entries,
        &Change::Assign(role("frontend")),
        &hosts,
    )
    .expect("assigned");
    assert_eq!(entries.roles.get("frontend"), Some(&Entry::Include));

    entries.hosts.insert("d1".to_owned(), Entry::Exclude);
    entries.roles.insert("storage".to_owned(), Entry::Exclude);
    apply(&def, &mut entries, &Change::Assign(host("d1")), &hosts).expect("assigned");
    assert_eq!(
        entries.hosts.get("d1"),
        Some(&Entry::Include),
        "the removal is replaced"
    );
    apply(&def, &mut entries, &Change::Assign(role("storage")), &hosts).expect("assigned");
    assert_eq!(entries.roles.get("storage"), None);

    // A selector's role is assigned whole by an entry: its labels no longer pick the hosts.
    let zoned = web("[service.selector]\nlabels = { zone = \"a\" }");
    let mut entries = ServiceAssignments::default();
    apply(
        &zoned,
        &mut entries,
        &Change::Assign(role("daemon")),
        &hosts,
    )
    .expect("assigned");
    assert_eq!(entries.roles.get("daemon"), Some(&Entry::Include));
}

/// Catches: an `Unassign` of a scope that is not assigned changing anything, a removal that the
/// rollout file would undo, or an added assignment kept as a removal.
#[test]
fn an_unassign_refuses_a_scope_not_assigned_and_writes_what_takes_the_service_off() {
    let def = web("");
    let hosts = [known("d1", "daemon", true), known("f1", "frontend", true)];
    let off = |scope: Scope| Change::Unassign {
        scope,
        uninstall: false,
    };
    let mut entries = ServiceAssignments::default();
    assert_eq!(
        apply(&def, &mut entries, &off(host("f1")), &hosts),
        Err(NotAssigned)
    );
    assert_eq!(
        apply(&def, &mut entries, &off(role("frontend")), &hosts),
        Err(NotAssigned)
    );
    assert!(entries.is_empty());

    assert_eq!(
        apply(&def, &mut entries, &off(host("d1")), &hosts),
        Ok(vec!["d1".to_owned()])
    );
    assert_eq!(entries.hosts.get("d1"), Some(&Entry::Exclude));
    assert_eq!(
        apply(&def, &mut entries, &off(host("d1")), &hosts),
        Err(NotAssigned)
    );

    apply(&def, &mut entries, &Change::Assign(host("f1")), &hosts).expect("assigned");
    assert_eq!(
        apply(&def, &mut entries, &off(host("f1")), &hosts),
        Ok(vec!["f1".to_owned()])
    );
    assert_eq!(entries.hosts.get("f1"), None, "nothing else assigns f1");

    apply(
        &def,
        &mut entries,
        &Change::Assign(role("frontend")),
        &hosts,
    )
    .expect("assigned");
    assert_eq!(
        apply(&def, &mut entries, &off(role("frontend")), &hosts),
        Ok(vec!["f1".to_owned()])
    );
    assert_eq!(entries.roles.get("frontend"), None);
    assert_eq!(
        apply(&def, &mut entries, &off(role("storage")), &hosts),
        Ok(Vec::new())
    );
    assert_eq!(entries.roles.get("storage"), Some(&Entry::Exclude));
    assert!(entries.uninstall.is_empty(), "no uninstall was asked");
}

/// Catches: an uninstall ordered to a host that keeps an assignment (by its own entry), to one
/// that does not list the service, under the generation of an earlier call, or kept once the
/// host is assigned again.
#[test]
fn an_unassign_with_uninstall_orders_only_the_hosts_that_stop_being_assigned_and_list_it() {
    let def = web("");
    let hosts = [
        known("d1", "daemon", true),
        known("d2", "daemon", true),
        known("d3", "daemon", false),
    ];
    let mut entries = ServiceAssignments::default();
    entries.hosts.insert("d2".to_owned(), Entry::Include);
    let off = Change::Unassign {
        scope: role("daemon"),
        uninstall: true,
    };
    assert_eq!(
        apply(&def, &mut entries, &off, &hosts),
        Ok(vec!["d1".to_owned(), "d3".to_owned()])
    );
    assert_eq!(entries.uninstall, BTreeMap::from([("d1".to_owned(), 1)]));
    assert_eq!(
        entries.uninstall_order("d1").as_deref(),
        Some("uninstall-1")
    );
    assert_eq!(entries.uninstall_order("d2"), None);

    let d2_off = Change::Unassign {
        scope: host("d2"),
        uninstall: true,
    };
    apply(&def, &mut entries, &d2_off, &hosts).expect("unassigned");
    assert_eq!(
        entries.uninstall_order("d2").as_deref(),
        Some("uninstall-2")
    );
    assert_eq!(
        entries.uninstall_order("d1").as_deref(),
        Some("uninstall-1")
    );

    // A call that stops no host that lists the service takes no generation.
    let none = Change::Unassign {
        scope: role("storage"),
        uninstall: true,
    };
    apply(&def, &mut entries, &none, &hosts).expect("unassigned");
    assert_eq!(entries.uninstall_generation, 2);

    // Assigned again by their role, d1 and d2 are no longer ordered to uninstall; the generation
    // stays, so a later order never reuses an id.
    apply(&def, &mut entries, &Change::Assign(role("daemon")), &hosts).expect("assigned");
    assert!(entries.uninstall.is_empty(), "assigned again");
    assert_eq!(entries.uninstall_generation, 2);
    assert_eq!(entries.len(), 1, "the storage removal");
    entries.roles.clear();
    assert!(!entries.is_empty(), "the generation alone is kept");
}

/// Catches: a minimum version set under the wrong scope, or one that cannot be removed.
#[test]
fn a_minimum_version_is_set_and_removed_per_scope() {
    let mut entries = ServiceAssignments::default();
    set_min_version(&mut entries, &host("d1"), Some(version("2.0")));
    set_min_version(&mut entries, &role("daemon"), Some(version("1.0")));
    assert_eq!(entries.host_min_versions.len(), 1);
    assert_eq!(entries.role_min_versions.len(), 1);
    set_min_version(&mut entries, &host("d1"), None);
    set_min_version(&mut entries, &role("daemon"), None);
    assert!(entries.is_empty());
    assert_eq!(host("d1").describe(), "host d1");
    assert_eq!(role("daemon").describe(), "role daemon");
}

/// Catches: a role an operator took the service off still listed, or one it assigned left out.
#[test]
fn the_roles_listed_are_the_rollout_files_with_the_operators_entries_on_top() {
    let def = web("");
    assert_eq!(roles_of(&def, None), ["storage", "daemon"]);
    let mut entries = ServiceAssignments::default();
    entries.roles.insert("storage".to_owned(), Entry::Exclude);
    entries.roles.insert("builder".to_owned(), Entry::Include);
    entries.roles.insert("daemon".to_owned(), Entry::Include);
    assert_eq!(roles_of(&def, Some(&entries)), ["daemon", "builder"]);
}
