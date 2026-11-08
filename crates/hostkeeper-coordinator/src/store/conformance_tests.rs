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
//! - `failed_keeps`: a commit of one part that fails leaves that part as it was.
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
use crate::audit_file::{Actor, AuditLine, DEFAULT_ROTATION, Filter, Logged, Query};
use crate::desired::{self, Desired, StoreError};
use crate::jti_file::jti_path;
use crate::ops::{AuditEntry, OpsError, OpsState};
use crate::registry::{Digest, Release};
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

fn loaded(backend: &Shared) -> Result<Loaded, String> {
    backend
        .load()
        .map_err(|why| format!("the load failed: {why}"))
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

fn load_gives_back(subject: &dyn Subject) -> Outcome {
    let live = subject.live();
    let (desired, mut ops, verdicts) = (
        desired_with_release("2.0", 'c'),
        rich_ops(),
        rich_verdicts(),
    );
    let audit = lines(1, 3);
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
    let back = loaded(&subject.restarted())?;
    check(back.desired == desired, || {
        format!("the desired state came back as {:?}", back.desired)
    })?;
    check(back.desired.releases == desired.releases, || {
        format!("the releases came back as {:?}", back.desired.releases)
    })?;
    check(normal(back.ops.clone()) == normal(ops), || {
        format!("the operations state came back as {:?}", back.ops)
    })?;
    check(back.verdicts == verdicts, || {
        format!("the verdicts came back as {:?}", back.verdicts)
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
    let back = loaded(&subject.restarted())?;
    check(back.verdicts == first, || {
        format!("a failed commit changed the verdicts: {:?}", back.verdicts)
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
    let held = lines(1, 12);
    live.commit(&Commit {
        ops: Some((&state, &state)),
        audit: &held,
        ..Commit::default()
    })
    .map_err(|why| format!("the commit failed: {why}"))?;
    let filters: [(&str, Filter<'_>); 8] = [
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
/// file system that fails the ops file's write, and a temporary file of the verdicts' that cannot
/// be made.
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
            .with_verdicts(&verdicts_path(&state));
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

    /// Where the verdicts' file is written before its rename.
    fn verdicts_temp(&self) -> PathBuf {
        let mut path = verdicts_path(&Self::state_file(&self.dir)).into_os_string();
        path.push(".new");
        PathBuf::from(path)
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
        if on {
            fs::create_dir(self.verdicts_temp()).expect("a directory in the way");
        } else {
            fs::remove_dir(self.verdicts_temp()).expect("the way cleared");
        }
    }
}

/// The ways [`Flawed`] is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flaw {
    /// A failing commit of the desired state and more writes the desired state.
    WritesHalf,
    /// A failing commit of the verdicts writes them.
    KeepsFailed,
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
            Flaw::KeepsFailed => change.verdicts.map(|part| Commit {
                verdicts: Some(part),
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
