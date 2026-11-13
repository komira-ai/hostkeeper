//! The contract of a [`Backend`], as one suite run against every implementation: the files, the
//! fake and Postgres (`docs/design/coordinator-storage.md` section 12). Each clause is a function that answers how the backend fared, so that the suite's own
//! test can run it against backends wrong in one way each and see the clause that names the
//! fault fail, and only that one.
//!
//! The clauses:
//! - `all_or_nothing`: a commit of several parts that fails leaves every part as it was. Its
//!   audit lines may stay in a log that is only appended to (the files append them before the
//!   ops file is written); no number is held twice.
//! - `load_gives_back`: a load gives back what was committed, every list in its order; an entry
//!   at its default may come back as absent ([`normal`]), which the code reads the same. A state's
//!   audit ring is the entries of the lines committed with it, as a store keeps it, which a
//!   backend may give back from the lines it holds. The desired state holds a release (targets
//!   and an index digest), given back equal.
//! - `failed_keeps`: a commit of one part (the verdicts, the accounts) that fails leaves that part
//!   as it was.
//! - `numbering_continues`: a backend started again holds every audit line, in order, and its
//!   newest number ([`Backend::last_seq`]), so a store numbers on from it.
//! - `audit_pages`: `audit_page` answers every filter, `since_seq` and `newest` alike, as the
//!   lines held filtered by hand.
//! - `spent_until_past`: a spent id is refused again until it is past, and after a restart.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_testkit::Scratch;
use serde_json::json;

use super::fake::store_failure;
use super::{Backend, BackendError, Commit, Fake, Files, Loaded, Shared};
use crate::accounts::{Accounts, accounts_path};
use crate::audit_file::{Actor, AuditLine, DEFAULT_ROTATION, Filter, Logged, Query};
use crate::desired::{self, Desired, StoreError};
use crate::host_groups::{GroupName, HostGroups};
use crate::jti_file::jti_path;
use crate::layout::{Layout, layout_path};
use crate::ops::{AuditEntry, OpsError, OpsState};
use crate::registry::{Digest, Release};
use crate::repositories::Registry;
use crate::repositories_support::komira;
use crate::testutil::{scratch, version};
use crate::verdict_file::{self, verdicts_path};

/// A backend under the suite: the one live, and one started again on what it holds.
pub(crate) trait Subject {
    /// The backend that commits.
    fn live(&self) -> Shared;
    /// A backend started again on what the live one made durable.
    fn restarted(&self) -> Shared;
    /// From now on, every commit of the live backend fails (`true`), or none does.
    fn failing(&self, on: bool);
}

/// How one clause fared: `Err` names what it found.
type Outcome = Result<(), String>;

/// A clause of the suite.
type Clause = fn(&dyn Subject) -> Outcome;

/// `Err(what)` unless `holds`.
fn check(holds: bool, what: impl FnOnce() -> String) -> Outcome {
    if holds { Ok(()) } else { Err(what()) }
}

/// The state with every entry at its default removed: what a backend that does not store such
/// an entry gives back, and what the code reads the same (`an_entry_at_its_default_reads_as_absent`).
pub(crate) fn normal(mut state: OpsState) -> OpsState {
    state.hosts.retain(|_, host| *host != Default::default());
    state
        .services
        .retain(|_, marks| *marks != Default::default());
    state
        .assignments
        .retain(|_, entries| *entries != Default::default());
    state
        .rollouts
        .retain(|_, rollout| *rollout != Default::default());
    state.host_groups = state.host_groups.without_defaults();
    state
}

/// The report of a halted update to `target`.
fn report(target: &str, reason: &str) -> serde_json::Value {
    json!({"target": target, "outcome": "failed", "phase": "halt", "reason": reason})
}

/// An operations state with something in every list, lists out of any sorted order, and entries
/// at their default; one inventory record states its platform.
pub(crate) fn rich_ops() -> OpsState {
    let mut state: OpsState = serde_json::from_value(json!({
        "schema": 1,
        "hosts": {"s1": {"labels": {"rack": "r1", "zone": "a"}}, "s8": {}},
        "excluded": [
            {"service": "kbf", "node_id": "s9", "reason": "last in name, first in the list"},
            {"service": "kbf", "node_id": "s2", "reason": "bad disk"}
        ],
        "services": {
            "kbf": {
                "halt": {"node_id": "s1", "check": "disk_tainted", "target": "2.0",
                         "report": report("2.0", "disk_tainted"), "origin": "updater"},
                "halt_acked": [{"node_id": "s3", "target": "1.9", "report": report("1.9", "x")}],
                "halt_seen": [
                    {"node_id": "s6", "target": "2.0", "report": report("2.0", "late")},
                    {"node_id": "s4", "target": "2.0", "report": report("2.0", "early")}
                ],
                "halt_generation": 3,
                "last_good": "1.0",
                "bad": ["3.0", "2.5"],
                "failure_acked": [{"node_id": "s5", "report": report("2.5", "boot")}],
                "force_generation": 2,
                "bad_by_force": ["2.5"]
            },
            "idle": {}
        },
        "audit": [{"seq": 7, "by": "operator", "action": "exclude", "service": "kbf",
                   "node_id": "s2", "reason": "bad disk"}],
        "holds": [
            {"service": "kbf", "scope": {"host": "s3"}, "reason": "b", "by": "op", "at_unix": 5},
            {"service": "kbf", "scope": "fleet", "reason": "a", "by": "op", "at_unix": 4}
        ],
        "assignments": {
            "kbf": {"roles": {"storage": "include"}, "hosts": {"s4": "exclude"},
                    "role_min_versions": {"storage": "1.0"}, "uninstall": {"s5": 2},
                    "uninstall_generation": 2},
            "idle": {}
        },
        "inventory": [
            {"host_id": "s7", "role": "storage", "data_macs": ["aa:bb:cc:dd:ee:01"],
             "serial": "SN7", "by": "op", "at_unix": 9},
            {"host_id": "s0", "by": "op", "at_unix": 8}
        ]
    }))
    .expect("an operations state");
    state.rollouts = serde_json::from_value(json!({
        "kbf": {"version": "2.0", "entered": 1, "done": ["s1"],
                "paused": {"by": "op", "reason": "look first"}},
        "idle": {}
    }))
    .expect("plan positions");
    state.inventory[0].platform = Some(hostkeeper_wire::Platform::LinuxArm64);
    state
}

/// The desired state of `kbf` at `at`, registered with a package digest made of `fill`.
pub(crate) fn desired_at(at: &str, fill: char) -> desired::Held {
    let mut held = desired::Held::default();
    let desired = Desired::parse(
        json!({"version": at, "min_up": {"storage": 1, "frontend": 0}})
            .to_string()
            .as_bytes(),
    )
    .expect("a desired state");
    let digest = Digest::parse(&fill.to_string().repeat(64)).expect("a digest");
    let mut registry = crate::registry::Registry::default();
    registry.record(&version(at), Release::Package(digest), |_| true);
    held.desired.insert("kbf".to_owned(), desired);
    held.digests.insert("kbf".to_owned(), registry);
    held
}

/// A desired state of `kbf` at `at` that also holds the release of that version: two platforms,
/// the first with two entries, one with a release index digest.
pub(crate) fn desired_with_release(at: &str, fill: char) -> desired::Held {
    use hostkeeper_proto::hostkeeper::release::v1::{Location, Target, TargetList};
    let entry = |installer: &str, c: char| Target {
        installer: installer.to_owned(),
        locations: vec![Location {
            store: "github".to_owned(),
            locator: format!("example-org/kbf/releases/download/v{at}/kbf-{installer}"),
        }],
        digest: c.to_string().repeat(64),
        bytes: 4096,
        ..Target::default()
    };
    let targets = std::collections::BTreeMap::from([
        (
            "linux-x86_64".to_owned(),
            TargetList {
                entries: vec![entry("tarball", fill), entry("deb", 'd')],
            },
        ),
        (
            "macos-arm64".to_owned(),
            TargetList {
                entries: vec![entry("tarball", 'e')],
            },
        ),
    ]);
    let index = Digest::parse(&"9".repeat(64)).expect("a digest");
    let mut held = desired_at(at, fill);
    held.releases.entry("kbf".to_owned()).or_default().insert(
        at.to_owned(),
        Arc::new(crate::releases::Release::new(targets, Some(index))),
    );
    held
}

/// Verdicts with a standing one and two counted, out of host order.
pub(crate) fn rich_verdicts() -> verdict_file::Held {
    let with_order = |reason: &str| {
        json!({"target": "2.0", "outcome": "failed", "phase": "boot", "reason": reason,
               "order_id": "update-1", "order_try": 1})
    };
    serde_json::from_value(json!({"services": {"kbf": {
        "standing": {"s1": with_order("a")},
        "counted": [{"node_id": "s2", "report": with_order("b")},
                    {"node_id": "s1", "report": with_order("a")}]
    }}}))
    .expect("verdicts")
}

/// Accounts with two users and two tokens, out of name order: one user with every field set, one
/// with none of the optional ones, one of them disabled.
pub(crate) fn rich_accounts() -> Accounts {
    use crate::accounts::{ApiToken, User};
    let hash = |salt: char| format!("$argon2id$v=19$m=19456,t=2,p=1${salt}$hash");
    let user = |name: &str, hash: String| User {
        name: name.to_owned(),
        hash,
        enabled: true,
        created_by: "coordinator".to_owned(),
        created_at: 1_800_000_000,
        ..User::default()
    };
    let token = |name: &str, fill: char| ApiToken {
        name: name.to_owned(),
        sha256: fill.to_string().repeat(64),
        created_by: "zed".to_owned(),
        created_at: 1_800_000_100,
    };
    Accounts::default()
        .with_user(User {
            must_change: true,
            initial_expires_at: Some(1_800_003_600),
            initial_id: Some("first".to_owned()),
            initial_void: true,
            last_login_at: Some(1_800_000_500),
            ..user("zed", hash('b'))
        })
        .and_then(|held| {
            held.with_user(User {
                enabled: false,
                ..user("alice", hash('a'))
            })
        })
        .and_then(|held| held.with_token(token("ci-ops", 'c')))
        .and_then(|held| held.with_token(token("backup", 'a')))
        .expect("accounts")
}

/// A layout named `Lab`, with a row, two racks and two placements, one of them of an inventory entry.
pub(crate) fn rich_layout() -> Layout {
    let (next, row) = Layout::default()
        .with_name("Lab")
        .and_then(|named| named.with_next_row())
        .expect("a row");
    let (next, rack) = next.with_rack("", &row.id, 42).expect("a rack");
    let (next, _) = next.with_rack("", &row.id, 24).expect("a second rack");
    let host = crate::layout::Subject {
        kind: crate::layout::Kind::Host,
        id: "node-1".into(),
    };
    let entry = crate::layout::Subject {
        kind: crate::layout::Kind::Inventory,
        id: "rec-7".into(),
    };
    let (next, _) = next
        .with_placement(crate::operator_layout::placement_of(host, &rack.id, 39, 2))
        .expect("a placement");
    let (next, _) = next
        .with_placement(crate::operator_layout::placement_of(entry, &rack.id, 1, 1))
        .expect("a second placement");
    let (next, device) = next
        .with_device(
            "",
            crate::layout::Device {
                id: String::new(),
                name: "bmc-switch".into(),
                role: crate::layout::Segment::Bmc,
                console_host: "node-1".into(),
                console_device: "/dev/ttyUSB0".into(),
                management_mac: "aa:bb:cc:dd:ee:ff".into(),
                notes: "top of the rack".into(),
            },
        )
        .expect("a device");
    let switch = crate::layout::Subject {
        kind: crate::layout::Kind::Inventory,
        id: device.id,
    };
    let (next, _) = next
        .with_placement(crate::operator_layout::placement_of(switch, &rack.id, 3, 1))
        .expect("the device placed");
    next
}

/// The audit lines numbered `from` to `to`, varied in who, what, service and host.
pub(crate) fn lines(from: u64, to: u64) -> Vec<AuditLine> {
    (from..=to)
        .map(|seq| {
            let mut entry = AuditEntry::new(
                ["alice", "coordinator", "ci"][usize::try_from(seq % 3).unwrap_or(0)],
                ["exclude", "halt"][usize::try_from(seq % 2).unwrap_or(0)],
            );
            entry.seq = seq;
            entry.service = (seq % 4 != 0).then(|| ["kbf", "web"][usize::from(seq > 6)].to_owned());
            entry.node_id = (seq % 5 != 0).then(|| format!("s{}", seq % 3));
            entry.reason = Some(format!("change {seq}"));
            AuditLine::of(&entry, 1_800_000_000 + seq)
        })
        .collect()
}

/// The ring of a state committed with `lines`: their entries, as the store keeps them.
fn ring(lines: &[AuditLine]) -> Vec<AuditEntry> {
    lines
        .iter()
        .map(|line| line.clone().into_logged().entry)
        .collect()
}

/// What `backend` holds, with the built-in group `all` left out while it is as the migration makes
/// it: a database holds it from `0013`, a file store only once it is changed, and the two read
/// alike ([`HostGroups::is_bare`]).
fn loaded(backend: &Shared) -> Result<Loaded, String> {
    let mut loaded = backend
        .load()
        .map_err(|why| format!("the load failed: {why}"))?;
    if loaded.desired.host_groups.0.get(&GroupName::all())
        == HostGroups::default().with_all().0.get(&GroupName::all())
    {
        loaded.desired.host_groups.0.remove(&GroupName::all());
    }
    Ok(loaded)
}

fn every_line(backend: &Shared) -> Result<Vec<u64>, String> {
    let mut seqs = Vec::new();
    backend
        .audit_each(&mut |logged| seqs.push(logged.entry.seq))
        .map_err(|why| format!("the audit could not be read: {why}"))?;
    Ok(seqs)
}

fn all_or_nothing(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    let (empty_desired, empty_ops) = (desired::Held::default(), OpsState::default());
    let first_desired = desired_at("1.0", 'a');
    let mut first_ops = OpsState::default();
    first_ops.excluded = rich_ops().excluded.split_off(1);
    let first_lines = lines(1, 2);
    first_ops.audit = ring(&first_lines);
    let first = Commit {
        desired: Some((&empty_desired, &first_desired)),
        ops: Some((&empty_ops, &first_ops)),
        audit: &first_lines,
        ..Commit::default()
    };
    live.commit(&first)
        .map_err(|why| format!("the first commit failed: {why}"))?;
    let second_desired = desired_at("2.0", 'b');
    let second_lines = lines(3, 3);
    let mut second_ops = rich_ops();
    second_ops.audit = ring(&lines(1, 3));
    subject.failing(true);
    let refused = live.commit(&Commit {
        desired: Some((&first_desired, &second_desired)),
        ops: Some((&first_ops, &second_ops)),
        audit: &second_lines,
        ..Commit::default()
    });
    subject.failing(false);
    check(refused.is_err(), || {
        "a commit told to fail was taken".into()
    })?;
    let after = loaded(&subject.restarted())?;
    check(after.desired == first_desired, || {
        format!(
            "the desired state of a failed commit landed: {:?}",
            after.desired
        )
    })?;
    check(normal(after.ops.clone()) == normal(first_ops), || {
        format!(
            "the operations state of a failed commit landed: {:?}",
            after.ops
        )
    })?;
    let seqs = every_line(&subject.restarted())?;
    check(seqs.windows(2).all(|w| w[0] < w[1]), || {
        format!("an audit number is held twice or out of order: {seqs:?}")
    })?;
    check(seqs.starts_with(&[1, 2]), || {
        format!("the lines of the commit that landed are not held: {seqs:?}")
    })
}

/// Puts two host groups and their desired versions into `desired`, and into `ops` the marks of
/// two of their packages, one at its default, and an inventory record's group.
fn with_host_groups(desired: &mut desired::Held, ops: &mut OpsState) {
    use crate::host_groups::{GroupMarks, HostGroup, HostGroups};
    let web = crate::host_groups::tests::web();
    let canary = HostGroup {
        packages: vec!["hostkeeper".to_owned()],
        generation: 1,
        desired: std::collections::BTreeMap::new(),
        ..web.clone()
    };
    desired.host_groups = HostGroups(
        [("web", web), ("web-canary", canary)]
            .into_iter()
            .filter_map(|(name, group)| Some((name.parse().ok()?, group)))
            .collect(),
    );
    let marks = std::collections::BTreeMap::from([
        (
            "tierstore".to_owned(),
            crate::host_groups::tests::every_mark(),
        ),
        ("hostkeeper".to_owned(), GroupMarks::default()),
    ]);
    if let Ok(name) = "web".parse() {
        ops.host_groups.0.insert(name, marks);
    }
    ops.inventory[1].host_group = "web".to_owned();
}

fn load_gives_back(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    let (mut desired, mut ops, verdicts, accounts, layout) = (
        desired_with_release("2.0", 'c'),
        rich_ops(),
        rich_verdicts(),
        rich_accounts(),
        rich_layout(),
    );
    desired.repositories = Registry::default()
        .put(komira(1))
        .and_then(|registry| registry.put(komira(2)))
        .map_err(|why| format!("the registry could not be made: {why}"))?;
    with_host_groups(&mut desired, &mut ops);
    let mut audit = lines(1, 3);
    audit[1].detail.host_group = Some("web".to_owned());
    ops.audit = ring(&audit);
    live.commit(&Commit {
        desired: Some((&desired::Held::default(), &desired)),
        ops: Some((&OpsState::default(), &ops)),
        audit: &audit,
        ..Commit::default()
    })
    .map_err(|why| format!("the commit failed: {why}"))?;
    live.commit(&Commit {
        verdicts: Some((&verdict_file::Held::default(), &verdicts)),
        ..Commit::default()
    })
    .map_err(|why| format!("the verdicts' commit failed: {why}"))?;
    live.commit(&Commit {
        accounts: Some((&Accounts::default(), &accounts)),
        ..Commit::default()
    })
    .map_err(|why| format!("the accounts' commit failed: {why}"))?;
    live.commit(&Commit {
        layout: Some((&Layout::default(), &layout)),
        ..Commit::default()
    })
    .map_err(|why| format!("the layout's commit failed: {why}"))?;
    let back = loaded(&subject.restarted())?;
    check(back.desired == desired, || {
        format!("the desired state came back as {:?}", back.desired)
    })?;
    check(back.desired.releases == desired.releases, || {
        format!("the releases came back as {:?}", back.desired.releases)
    })?;
    check(back.desired.host_groups == desired.host_groups, || {
        format!(
            "the host groups came back as {:?}",
            back.desired.host_groups
        )
    })?;
    check(
        back.ops.host_groups.clone().without_defaults()
            == ops.host_groups.clone().without_defaults(),
        || format!("the groups' marks came back as {:?}", back.ops.host_groups),
    )?;
    check(back.desired.repositories == desired.repositories, || {
        format!(
            "the repositories came back as {:?}",
            back.desired.repositories
        )
    })?;
    check(normal(back.ops.clone()) == normal(ops), || {
        format!("the operations state came back as {:?}", back.ops)
    })?;
    check(back.verdicts == verdicts, || {
        format!("the verdicts came back as {:?}", back.verdicts)
    })?;
    check(back.accounts == accounts, || {
        format!("the accounts came back as {:?}", back.accounts)
    })?;
    check(back.layout == layout, || {
        format!("the layout came back as {:?}", back.layout)
    })
}

fn failed_keeps(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    let (none, first, second) = (
        verdict_file::Held::default(),
        rich_verdicts(),
        verdict_file::Held::default(),
    );
    live.commit(&Commit {
        verdicts: Some((&none, &first)),
        ..Commit::default()
    })
    .map_err(|why| format!("the first commit failed: {why}"))?;
    subject.failing(true);
    let refused = live.commit(&Commit {
        verdicts: Some((&first, &second)),
        ..Commit::default()
    });
    subject.failing(false);
    check(refused.is_err(), || {
        "a commit told to fail was taken".into()
    })?;
    let (held, changed) = (
        rich_accounts()
            .without_token("ci-ops")
            .and_then(|held| held.without_token("backup"))
            .map_err(|why| format!("the accounts could not be made: {why}"))?,
        Accounts::default(),
    );
    live.commit(&Commit {
        accounts: Some((&changed, &held)),
        ..Commit::default()
    })
    .map_err(|why| format!("the accounts' first commit failed: {why}"))?;
    subject.failing(true);
    let refused = live.commit(&Commit {
        accounts: Some((&held, &changed)),
        ..Commit::default()
    });
    subject.failing(false);
    check(refused.is_err(), || {
        "an accounts' commit told to fail was taken".into()
    })?;
    let back = loaded(&subject.restarted())?;
    check(back.verdicts == first, || {
        format!("a failed commit changed the verdicts: {:?}", back.verdicts)
    })?;
    check(back.accounts == held, || {
        format!("a failed commit changed the accounts: {:?}", back.accounts)
    })
}

fn numbering_continues(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    let state = OpsState::default();
    for line in lines(1, 3) {
        live.commit(&Commit {
            ops: Some((&state, &state)),
            audit: std::slice::from_ref(&line),
            ..Commit::default()
        })
        .map_err(|why| format!("a commit failed: {why}"))?;
    }
    let again = subject.restarted();
    check(again.last_seq() == 3, || {
        format!("a restart holds {} as its newest number", again.last_seq())
    })?;
    again
        .commit(&Commit {
            ops: Some((&state, &state)),
            audit: &lines(4, 4),
            ..Commit::default()
        })
        .map_err(|why| format!("a commit after the restart failed: {why}"))?;
    let seqs = every_line(&subject.restarted())?;
    check(seqs == [1, 2, 3, 4], || format!("the audit holds {seqs:?}"))
}

fn audit_pages(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    let state = OpsState::default();
    // The lines numbered 1 to 8 name a host group: `alpha` for the odd ones, `beta` for 2 and 6.
    let held: Vec<AuditLine> = lines(1, 12)
        .into_iter()
        .map(|line| {
            let mut logged = line.into_logged();
            logged.entry.host_group = match logged.entry.seq {
                1..=8 if logged.entry.seq % 2 == 1 => Some("alpha".to_owned()),
                2 | 6 => Some("beta".to_owned()),
                _ => None,
            };
            AuditLine::of(&logged.entry, logged.at_unix.unwrap_or_default())
        })
        .collect();
    live.commit(&Commit {
        ops: Some((&state, &state)),
        audit: &held,
        ..Commit::default()
    })
    .map_err(|why| format!("the commit failed: {why}"))?;
    let filters: [(&str, Filter<'_>); 10] = [
        ("every", Filter::default()),
        (
            "by",
            Filter {
                by_name: Some("alice"),
                ..Filter::default()
            },
        ),
        (
            "action",
            Filter {
                actions: &["halt"],
                ..Filter::default()
            },
        ),
        (
            "actions",
            Filter {
                actions: &["halt", "exclude"],
                ..Filter::default()
            },
        ),
        (
            "service",
            Filter {
                service: Some("web"),
                ..Filter::default()
            },
        ),
        (
            "node",
            Filter {
                node_id: Some("s1"),
                ..Filter::default()
            },
        ),
        (
            "operator",
            Filter {
                actor: Some(Actor::Operator),
                ..Filter::default()
            },
        ),
        (
            "ci",
            Filter {
                actor: Some(Actor::Ci),
                service: Some("kbf"),
                ..Filter::default()
            },
        ),
        (
            "group",
            Filter {
                host_group: Some("alpha"),
                ..Filter::default()
            },
        ),
        (
            "group and service",
            Filter {
                host_group: Some("beta"),
                service: Some("kbf"),
                ..Filter::default()
            },
        ),
    ];
    for (name, filter) in filters {
        for (since_seq, limit, newest) in [(0, 5, false), (4, 3, false), (0, 4, true), (9, 5, true)]
        {
            let query = Query {
                since_seq,
                limit,
                newest,
                filter,
            };
            let mut want: Vec<Logged> = held
                .iter()
                .map(|line| line.clone().into_logged())
                .filter(|logged| logged.entry.seq > since_seq && filter.keeps(&logged.entry))
                .collect();
            if newest {
                want.drain(..want.len().saturating_sub(limit));
            } else {
                want.truncate(limit);
            }
            let page = live
                .audit_page(&query)
                .map_err(|why| format!("a page could not be read: {why}"))?;
            check(page == want, || {
                format!(
                    "{name} since {since_seq} limit {limit} newest {newest}: {:?}",
                    page.iter().map(|l| l.entry.seq).collect::<Vec<_>>()
                )
            })?;
        }
    }
    for (group, seqs) in [
        ("alpha", vec![1, 3, 5, 7]),
        ("beta", vec![2, 6]),
        ("gamma", vec![]),
    ] {
        let page = live
            .audit_page(&Query {
                since_seq: 0,
                limit: 50,
                newest: false,
                filter: Filter {
                    host_group: Some(group),
                    ..Filter::default()
                },
            })
            .map_err(|why| format!("a page could not be read: {why}"))?;
        let found: Vec<u64> = page.iter().map(|logged| logged.entry.seq).collect();
        check(found == seqs, || {
            format!("the lines of {group} are {found:?}")
        })?;
    }
    Ok(())
}

fn spent_until_past(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    live.spend("j1", 100, 10)
        .map_err(|why| format!("a first spend failed: {why}"))?;
    check(
        matches!(live.spend("j1", 100, 50), Err(BackendError::Spent)),
        || "an id spent and not past was taken again".into(),
    )?;
    live.spend("j1", 200, 100)
        .map_err(|why| format!("an id past was refused: {why}"))?;
    let again = subject.restarted();
    let back = loaded(&again)?;
    check(back.spent.contains(&("j1".to_owned(), 200)), || {
        format!("a restart lost the spent id: {:?}", back.spent)
    })?;
    check(
        matches!(again.spend("j1", 300, 150), Err(BackendError::Spent)),
        || "a restart takes an id it holds as spent".into(),
    )
}

/// The clauses, by name.
pub(crate) const CLAUSES: [(&str, Clause); 6] = [
    ("all_or_nothing", all_or_nothing),
    ("load_gives_back", load_gives_back),
    ("failed_keeps", failed_keeps),
    ("numbering_continues", numbering_continues),
    ("audit_pages", audit_pages),
    ("spent_until_past", spent_until_past),
];

/// Runs every clause on a subject of its own, made by `make` with the clause's name: the names of
/// the clauses that failed, each with what it found.
pub(crate) fn run(make: &dyn Fn(&str) -> Box<dyn Subject>) -> Vec<(&'static str, String)> {
    CLAUSES
        .iter()
        .filter_map(|(name, clause)| clause(&*make(name)).err().map(|why| (*name, why)))
        .collect()
}

// ---- the subjects --------------------------------------------------------------------------

/// The fake: clones share what it holds, so a clone is the fake started again.
pub(crate) struct OnFake(pub(crate) Fake);

impl Subject for OnFake {
    fn live(&self) -> Shared {
        Arc::new(self.0.clone())
    }

    fn restarted(&self) -> Shared {
        Arc::new(self.0.clone())
    }

    fn failing(&self, on: bool) {
        if on { self.0.fail() } else { self.0.heal() }
    }
}

/// The files, in a scratch directory: started again by opening them again. A failing commit is a
/// file system that fails the ops file's write, and a temporary file of the verdicts' or the
/// accounts' that cannot be made.
struct OnFiles {
    dir: Scratch,
    failing: Arc<AtomicBool>,
    live: Shared,
}

/// The seconds of the spent ids' clock: what a restart drops the past ids by.
const SPENT_NOW: u64 = 150;

impl OnFiles {
    fn new(name: &str) -> Self {
        let dir = scratch(&format!("conformance-{name}"));
        let failing = Arc::new(AtomicBool::new(false));
        let live = Arc::new(Self::open(&dir, Some(Arc::clone(&failing))));
        Self { dir, failing, live }
    }

    fn state_file(dir: &Scratch) -> PathBuf {
        dir.path().join("desired.json")
    }

    fn open(dir: &Scratch, failing: Option<Arc<AtomicBool>>) -> Files {
        let state = Self::state_file(dir);
        let ops = dir.path().join("desired.json.ops");
        let files = Files::default()
            .with_jti(&jti_path(&state), Arc::new(|| SPENT_NOW))
            .expect("the spent ids")
            .with_desired(&state, None)
            .with_ops(&ops, DEFAULT_ROTATION)
            .expect("the ops files")
            .with_verdicts(&verdicts_path(&state))
            .with_accounts(&accounts_path(&state))
            .with_layout(&layout_path(&state));
        let Some(failing) = failing else {
            return files;
        };
        files.with_ops_write(Box::new(move |state| {
            if failing.load(Ordering::SeqCst) {
                return Err(OpsError::Io(StoreError::Io {
                    path: ops.clone(),
                    source: std::io::Error::other("the disk is full"),
                }));
            }
            crate::ops_file::write(&ops, state)
        }))
    }

    /// Where the verdicts' file and the accounts' file are written before their rename.
    fn temps(&self) -> [PathBuf; 2] {
        let state = Self::state_file(&self.dir);
        [verdicts_path(&state), accounts_path(&state)].map(|path| {
            let mut path = path.into_os_string();
            path.push(".new");
            PathBuf::from(path)
        })
    }
}

impl Subject for OnFiles {
    fn live(&self) -> Shared {
        Arc::clone(&self.live)
    }

    fn restarted(&self) -> Shared {
        Arc::new(Self::open(&self.dir, None))
    }

    fn failing(&self, on: bool) {
        self.failing.store(on, Ordering::SeqCst);
        for temp in self.temps() {
            if on {
                fs::create_dir(temp).expect("a directory in the way");
            } else {
                fs::remove_dir(temp).expect("the way cleared");
            }
        }
    }
}

/// The ways [`Flawed`] is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flaw {
    /// A failing commit of the desired state and more writes the desired state.
    WritesHalf,
    /// A failing commit of the verdicts or the accounts writes them.
    KeepsFailed,
    /// A load gives no API token back.
    DropsTokens,
    /// A load gives the exclusions back in the other order.
    DropsOrder,
    /// A load gives no release back.
    DropsReleases,
    /// It says it holds no audit line.
    ForgetsNumbers,
    /// A page asked for the newest lines answers the oldest.
    IgnoresNewest,
    /// A spend records nothing.
    ForgetsSpends,
}

/// The fake, wrong in one way.
struct Flawed {
    inner: Fake,
    flaw: Flaw,
    failing: Arc<AtomicBool>,
}

impl Backend for Flawed {
    fn load(&self) -> Result<Loaded, BackendError> {
        let mut loaded = self.inner.load()?;
        if self.flaw == Flaw::DropsOrder {
            loaded.ops.excluded.reverse();
        }
        if self.flaw == Flaw::DropsReleases {
            loaded.desired.releases.clear();
        }
        if self.flaw == Flaw::DropsTokens {
            loaded.accounts = Accounts::new(
                loaded
                    .accounts
                    .users()
                    .map(|user| (user.name.clone(), user.clone()))
                    .collect(),
                std::collections::BTreeMap::new(),
            )
            .map_err(BackendError::Accounts)?;
        }
        Ok(loaded)
    }

    fn commit(&self, change: &Commit<'_>) -> Result<(), BackendError> {
        if !self.failing.load(Ordering::SeqCst) {
            return self.inner.commit(change);
        }
        let kept = match self.flaw {
            Flaw::WritesHalf => change.desired.map(|part| Commit {
                desired: Some(part),
                ..Commit::default()
            }),
            Flaw::KeepsFailed => Some(Commit {
                verdicts: change.verdicts,
                accounts: change.accounts,
                ..Commit::default()
            }),
            _ => None,
        };
        if let Some(kept) = kept {
            self.inner.commit(&kept)?;
        }
        Err(store_failure())
    }

    fn audit_page(&self, query: &Query<'_>) -> Result<Vec<Logged>, BackendError> {
        let newest = query.newest && self.flaw != Flaw::IgnoresNewest;
        self.inner.audit_page(&Query { newest, ..*query })
    }

    fn audit_each(&self, visit: &mut dyn FnMut(Logged)) -> Result<(), BackendError> {
        self.inner.audit_each(visit)
    }

    fn last_seq(&self) -> u64 {
        match self.flaw {
            Flaw::ForgetsNumbers => 0,
            _ => self.inner.last_seq(),
        }
    }

    fn audit_pruned_before(&self) -> Result<Option<u64>, BackendError> {
        self.inner.audit_pruned_before()
    }

    fn spend(&self, jti: &str, until_unix: u64, now_unix: u64) -> Result<(), BackendError> {
        match self.flaw {
            Flaw::ForgetsSpends => Ok(()),
            _ => self.inner.spend(jti, until_unix, now_unix),
        }
    }
}

impl Subject for Flawed {
    fn live(&self) -> Shared {
        Arc::new(Self {
            inner: self.inner.clone(),
            flaw: self.flaw,
            failing: Arc::clone(&self.failing),
        })
    }

    fn restarted(&self) -> Shared {
        self.live()
    }

    fn failing(&self, on: bool) {
        self.failing.store(on, Ordering::SeqCst);
    }
}

/// Proves: the files, the fake and Postgres each keep every clause of the contract the stores
/// rely on.
///
/// Catches: a backend that loses a part of a failed commit's state, gives a list back out of
/// order, numbers the audit anew after a restart, pages the audit wrongly, or takes a spent id
/// again.
#[test]
fn the_conformance_suite_passes_on_the_files_the_fake_and_postgres() {
    assert_eq!(run(&|_| Box::new(OnFake(Fake::new()))), []);
    assert_eq!(run(&|name| Box::new(OnFiles::new(name))), []);
    assert_eq!(
        run(&|_| Box::new(super::postgres::tests::store::OnPostgres::new())),
        []
    );
}

/// Proves: the suite catches what it claims. Against a backend wrong in one way each, the clause
/// that names the fault fails, and every other clause passes.
///
/// Catches: a clause that passes whatever the backend does, or one that fails for a fault that is
/// another clause's.
#[test]
fn the_suite_fails_against_a_backend_that_breaks_each_clause() {
    let cases = [
        (Flaw::WritesHalf, "all_or_nothing"),
        (Flaw::DropsOrder, "load_gives_back"),
        (Flaw::DropsReleases, "load_gives_back"),
        (Flaw::DropsTokens, "load_gives_back"),
        (Flaw::KeepsFailed, "failed_keeps"),
        (Flaw::ForgetsNumbers, "numbering_continues"),
        (Flaw::IgnoresNewest, "audit_pages"),
        (Flaw::ForgetsSpends, "spent_until_past"),
    ];
    for (flaw, clause) in cases {
        let failed: Vec<&str> = run(&|_| {
            Box::new(Flawed {
                inner: Fake::new(),
                flaw,
                failing: Arc::new(AtomicBool::new(false)),
            })
        })
        .into_iter()
        .map(|(name, _)| name)
        .collect();
        assert_eq!(failed, [clause], "{flaw:?}");
    }
}
