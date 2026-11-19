//! The host prerequisites file and what the coordinator keeps and sends of it
//! ([`crate::prerequisites`]): the file's rules and refusals, its reload, the reports a heartbeat
//! carries and the set a host is sent. The answer over gRPC and the operator's view are in
//! `grpc/tests/prerequisites.rs`.

use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::{ApproveRequest, RetireRequest};
use hostkeeper_wire::prerequisites::digest as wire_digest;

use super::{
    FEATURE, HostPrerequisites, MAX_FILE_BYTES, PrerequisitesError, Reload, Set, kept, parse,
};
use crate::enrollment::UNGROUPED_ROLE;
use crate::enrollment::tests::support::{
    HostKey, Rig as EnrollmentRig, approval, coordinator_over, enroll_request,
};
use crate::first_serving_tests::{APPROVED, FIRST, Fixture, WEB, daemon};
use crate::marks_support::{operator, rig, runs, start, who};
use crate::testutil::{Log, OPERATOR, Rig, id, scratch};

/// The SHA-256 of no bytes: the digest of the empty set.
const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The one prerequisite this release knows.
const XCODE: &str = "xcode-command-line-tools";

/// The names as owned strings.
pub(crate) fn names(of: &[&str]) -> Vec<String> {
    of.iter().map(|name| (*name).to_owned()).collect()
}

/// The TOML of a group `name` holding `prerequisites`, each as given.
pub(crate) fn group(name: &str, prerequisites: &[&str]) -> String {
    let listed: Vec<String> = prerequisites.iter().map(|p| format!("\"{p}\"")).collect();
    format!(
        "[host-groups.{name}]\nprerequisites = [{}]\n",
        listed.join(", ")
    )
}

/// Writes `text` to `host-prerequisites.toml` in `dir` with mode 0644 and answers its path.
pub(crate) fn file(dir: &Path, text: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("host-prerequisites.toml");
    std::fs::write(&path, text).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    path
}

/// The handle over a file of `text`, in a scratch directory named `name`.
fn handle(name: &str, text: &str) -> (HostPrerequisites, PathBuf, hostkeeper_testkit::Scratch) {
    let dir = scratch(name);
    let path = file(dir.path(), text);
    let held = HostPrerequisites::from_file(&path).expect("the file is read");
    (held, path, dir)
}

/// A report holding `digest`.
fn holding(digest: &str) -> agent::PrerequisitesReport {
    agent::PrerequisitesReport {
        digest: digest.to_owned(),
        ..agent::PrerequisitesReport::default()
    }
}

/// A state of `name` with `status`.
fn state(name: &str, status: agent::PrerequisiteStatus) -> agent::PrerequisiteState {
    agent::PrerequisiteState {
        name: name.to_owned(),
        status: status as i32,
        ..agent::PrerequisiteState::default()
    }
}

/// A heartbeat over gRPC that announces `features` and carries `report`.
fn said(features: &[&str], report: Option<agent::PrerequisitesReport>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: features.iter().map(|f| (*f).to_owned()).collect(),
        prerequisites: report,
        ..agent::HeartbeatRequest::default()
    }
}

/// A heartbeat of a daemon that announces the feature and carries `report`.
fn announcing(report: Option<agent::PrerequisitesReport>) -> agent::HeartbeatRequest {
    said(&["heartbeat-v2", FEATURE], report)
}

/// A rig whose coordinator holds `prerequisites`.
fn rig_with(name: &str, prerequisites: HostPrerequisites) -> Rig {
    let mut rig = rig(name, "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_host_prerequisites(prerequisites);
    rig
}

/// The set the coordinator of `rig` sends the host `host` of the group `group` that sends
/// `request`.
fn sent(rig: &Rig, host: &str, group: &str, request: &agent::HeartbeatRequest) -> Option<Set> {
    rig.coordinator
        .prerequisites_for(&who(host, group), request)
        .map(|set| Set {
            names: set.names,
            digest: set.digest,
        })
}

/// What the status counts of dropped prerequisites reports.
fn dropped(rig: &Rig) -> Option<u64> {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("prerequisites").copied()
}

// ---- the file -------------------------------------------------------------------------------

/// Proves: a file of two groups, one with the known prerequisite and one with none, is read as
/// the sets of those groups, each digest the one the daemon computes
/// (`hostkeeper_wire::prerequisites::digest`); a group the file does not name has no set.
///
/// Catches: a name lost, a digest of another rule than the daemon's.
#[test]
fn a_file_of_groups_is_read_as_their_sets() {
    let text = format!("{}{}", group("mac-build", &[XCODE]), group("web", &[]));
    let sets = parse(&text).expect("the file is read");
    assert_eq!(sets.len(), 2);
    assert_eq!(
        sets["mac-build"],
        Set {
            names: names(&[XCODE]),
            digest: wire_digest(&names(&[XCODE])),
        }
    );
    assert_eq!(
        sets["web"],
        Set {
            names: Vec::new(),
            digest: EMPTY_DIGEST.to_owned(),
        }
    );
    assert_eq!(sets.get("other"), None);
}

/// Proves: a file with no `host-groups` table, an empty one and an empty `[host-groups]` table
/// all name no group: they are read, and a handle over them is given a file but lists nothing.
///
/// Catches: an empty file refused, so that an operator who has not chosen a prerequisite yet
/// cannot start the coordinator with the flag.
#[test]
fn a_file_with_no_groups_is_read_and_lists_none() {
    for text in ["", "# nothing yet\n", "[host-groups]\n"] {
        assert_eq!(parse(text).expect(text), Default::default(), "{text:?}");
    }
    let (held, _, _dir) = handle("prerequisites-empty", "");
    assert!(held.is_given());
    assert_eq!(held.set_of("mac-build"), None);
}

/// Proves: the file's refusals say, exactly, which rule is broken, name the flag, the group and
/// the name's place, and for a name this release does not know the names it knows: a top-level
/// key that is not `host-groups`, `host-groups` that is not a table, a group name that is not
/// one, an entry that is not a table, a key of an entry other than `prerequisites`, no
/// `prerequisites`, `prerequisites` that is not an array, an element that is not a string, an
/// element that is not a known name, more than 16 names, a name twice.
///
/// Catches: a refusal that gives the reader nothing to find the line by, and a rule of the file
/// that is not checked.
#[test]
fn a_file_that_breaks_a_rule_is_refused_naming_the_flag_the_group_and_the_place() {
    let flag = "--host-prerequisites-file: ";
    let in_group = |why: &str| format!("{flag}host group mac-build: {why}");
    let many: Vec<&str> = vec![XCODE; 17];
    let cases: Vec<(String, String)> = vec![
        (
            format!("[tokens]\nname = \"{XCODE}\"\n"),
            format!("{flag}the key \"tokens\" is not known; the file holds only host-groups"),
        ),
        (
            "host-groups = [\"mac-build\"]\n".to_owned(),
            format!("{flag}host-groups is not a table of host groups"),
        ),
        (
            group("Mac-build", &[XCODE]),
            format!(
                "{flag}\"Mac-build\" is not a host group name (1 to 32 bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
            ),
        ),
        (
            format!("[host-groups]\nmac-build = [\"{XCODE}\"]\n"),
            in_group("it is not a table holding prerequisites"),
        ),
        (
            format!("[host-groups.mac-build]\nprerequisites = []\nkeys = [\"{XCODE}\"]\n"),
            in_group("the key \"keys\" is not known; it holds only prerequisites"),
        ),
        (
            "[host-groups.mac-build]\n".to_owned(),
            in_group("prerequisites is missing (write prerequisites = [] for none)"),
        ),
        (
            format!("[host-groups.mac-build]\nprerequisites = \"{XCODE}\"\n"),
            in_group("prerequisites is not an array of strings"),
        ),
        (
            format!("[host-groups.mac-build]\nprerequisites = [\"{XCODE}\", 7]\n"),
            in_group("prerequisite 2 is not a string"),
        ),
        (
            group("mac-build", &[XCODE, "homebrew"]),
            in_group(
                "prerequisite 2 is not a prerequisite this release knows (it knows xcode-command-line-tools)",
            ),
        ),
        (
            group("mac-build", &many),
            in_group("17 prerequisites, more than the 16 a set may hold"),
        ),
        (
            group("mac-build", &[XCODE, XCODE]),
            in_group("prerequisite 2 is prerequisite 1 again"),
        ),
    ];
    for (text, said) in cases {
        let why = parse(&text).expect_err(&text);
        assert_eq!(why.to_string(), said, "{text}");
    }
}

/// Proves: text that is not TOML is refused naming the flag and that it is not TOML.
///
/// Catches: a file of another format read as empty.
#[test]
fn text_that_is_not_toml_is_refused() {
    let why = parse("host-groups.mac-build = [\n").expect_err("not TOML");
    assert!(matches!(why, PrerequisitesError::NotToml(_)), "{why}");
    let said = why.to_string();
    assert!(
        said.starts_with("--host-prerequisites-file: not TOML: "),
        "{said}"
    );
}

/// Proves: the file is a plain file: any mode is read (0644 and 0666), and one of exactly the
/// largest size, 64 KiB, is read while one byte more is refused naming its size; neither message
/// names the path.
///
/// Catches: a mode rule of a secret file applied to a public file, and a size bound off by one.
#[test]
fn any_mode_is_read_and_the_size_is_bounded() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("prerequisites-size");
    let rest = usize::try_from(MAX_FILE_BYTES).expect("a size") - 1;
    let path = file(dir.path(), &format!("#{}", "x".repeat(rest)));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("mode");
    assert!(
        HostPrerequisites::from_file(&path).is_ok(),
        "64 KiB exactly"
    );

    std::fs::write(&path, format!("#{}", "x".repeat(rest + 1))).expect("written");
    let why = HostPrerequisites::from_file(&path).expect_err("over the bound");
    assert_eq!(
        why.to_string(),
        "--host-prerequisites-file: 65537 bytes, more than the 65536 the file may hold"
    );
}

/// Proves: a path that is a directory is refused as not a regular file, a path that does not
/// exist and a file that is not UTF-8 are refused with what reading them said, each naming the
/// flag and never the path.
///
/// Catches: a directory read as a file, and a refusal that leaves out the flag or shows the path.
#[test]
fn a_file_that_cannot_be_read_is_refused_without_its_path() {
    let dir = scratch("prerequisites-unreadable");
    let missing = dir.path().join("absent.toml");
    let bytes = dir.path().join("bytes.toml");
    std::fs::write(&bytes, b"\xff\xfe").expect("written");
    for (path, said) in [
        (dir.path(), "--host-prerequisites-file: not a regular file"),
        (
            missing.as_path(),
            "--host-prerequisites-file: No such file or directory (os error 2)",
        ),
        (
            bytes.as_path(),
            "--host-prerequisites-file: stream did not contain valid UTF-8",
        ),
    ] {
        let why = HostPrerequisites::from_file(path).expect_err(said);
        assert_eq!(why.to_string(), said);
        assert!(
            !why.to_string().contains(&*dir.path().to_string_lossy()),
            "{why}"
        );
    }
}

// ---- the reload -----------------------------------------------------------------------------

/// Proves: a handle with no file reloads as unchanged, lists no set for any group, and is not
/// given a file.
///
/// Catches: a reload of a coordinator started without the flag that reads or logs anything.
#[test]
fn a_handle_without_a_file_has_nothing_to_reload() {
    let held = HostPrerequisites::default();
    assert!(!held.is_given());
    assert_eq!(held.reload(), Reload::Unchanged);
    assert_eq!(held.set_of("mac-build"), None);
}

/// Proves: a reload of a file that holds the sets in use is unchanged and logs nothing; a file
/// whose sets changed is applied, logged at info with the number of groups, and the new set is
/// the one listed from then on.
///
/// Catches: a change that is not picked up, and a reload that logs a change when there is none.
#[test]
fn a_changed_file_is_applied_and_an_unchanged_one_is_not() {
    let log = Log::default();
    let _guard = log.install();
    let (held, path, _dir) = handle("prerequisites-reload", &group("mac-build", &[]));
    assert_eq!(held.reload(), Reload::Unchanged);
    assert_eq!(log.text(), "");

    let text = format!("{}{}", group("mac-build", &[XCODE]), group("web", &[]));
    std::fs::write(&path, text).expect("written");
    assert_eq!(held.reload(), Reload::Applied);
    assert_eq!(held.set_of("mac-build").expect("a set").names, [XCODE]);
    assert!(held.set_of("web").is_some());
    let logged = log.text();
    assert!(logged.contains("INFO"), "{logged}");
    assert!(
        logged.contains("the host prerequisites file changed: its sets are in use"),
        "{logged}"
    );
    assert!(logged.contains("groups=2"), "{logged}");
}

/// Proves: a file that is refused after a change, a file that is gone and a file that is not
/// TOML each reload as rejected, log a warning that names the rule, and leave the sets in use;
/// the file made right again is applied.
///
/// Catches: a bad edit that takes every host group's prerequisites away.
#[test]
fn a_file_that_is_refused_on_reload_keeps_the_sets_in_use() {
    let log = Log::default();
    let _guard = log.install();
    let (held, path, _dir) = handle("prerequisites-rejected", &group("mac-build", &[XCODE]));
    let before = held.set_of("mac-build");

    std::fs::write(&path, group("mac-build", &[XCODE, XCODE])).expect("written");
    assert_eq!(held.reload(), Reload::Rejected);
    assert_eq!(held.set_of("mac-build"), before);
    let logged = log.text();
    assert!(logged.contains("WARN"), "{logged}");
    assert!(
        logged.contains("the host prerequisites file was not read again: the sets in use stay"),
        "{logged}"
    );
    assert!(
        logged.contains("prerequisite 2 is prerequisite 1 again"),
        "{logged}"
    );

    std::fs::write(&path, "[host-groups").expect("written");
    assert_eq!(held.reload(), Reload::Rejected);
    std::fs::remove_file(&path).expect("removed");
    assert_eq!(held.reload(), Reload::Rejected);
    assert_eq!(held.set_of("mac-build"), before);

    file(
        path.parent().expect("a directory"),
        &group("mac-build", &[]),
    );
    assert_eq!(held.reload(), Reload::Applied);
    assert_eq!(held.set_of("mac-build").expect("a set").names.len(), 0);
}

// ---- what a heartbeat reports ---------------------------------------------------------------

/// Proves: the report of a daemon that announces the feature is kept as it says, the last one
/// replacing the one before; a heartbeat of such a daemon that carries none counts as the empty
/// report; and the report is the host's own: another host has none.
///
/// Catches: a report lost, one that does not replace the older, and a daemon that announced the
/// feature and never reported held as one that sent nothing.
#[test]
fn the_last_report_of_a_daemon_that_announces_the_feature_is_kept() {
    let rig = rig_with("prerequisites-hear", HostPrerequisites::default());
    let held = rig.coordinator.host_prerequisites();
    let host = id("d1");
    let failed = agent::PrerequisiteState {
        reason: "softwareupdate --list exited 1".to_owned(),
        failures: 2,
        next_try_unix: 1_800_000_000,
        seen_unix: 1_799_999_000,
        ..state(XCODE, agent::PrerequisiteStatus::Failed)
    };
    let report = agent::PrerequisitesReport {
        digest: "a".repeat(64),
        refused: Some(agent::PrerequisitesRefusal {
            digest: "b".repeat(64),
            reason: "prerequisite 2 is prerequisite 1 again".to_owned(),
        }),
        states: vec![failed],
    };

    let d1 = who("d1", "daemon");
    rig.coordinator
        .hear_prerequisites(&d1, &announcing(Some(report.clone())));
    assert_eq!(held.report_of(&host), Some(report));
    rig.coordinator
        .hear_prerequisites(&d1, &announcing(Some(holding(&"c".repeat(64)))));
    assert_eq!(held.report_of(&host), Some(holding(&"c".repeat(64))));
    rig.coordinator.hear_prerequisites(&d1, &announcing(None));
    assert_eq!(
        held.report_of(&host),
        Some(agent::PrerequisitesReport::default())
    );
    assert_eq!(held.report_of(&id("d2")), None);
    assert_eq!(held.reports().len(), 1);
    assert_eq!(dropped(&rig), None);
}

/// Proves: a daemon that does not announce the feature has its report ignored, however it is
/// written, and a host that never announced it has none kept.
///
/// Catches: a report of an older daemon or of a different protocol kept and used to hold a set
/// back.
#[test]
fn a_report_of_a_daemon_that_did_not_announce_the_feature_is_ignored() {
    let rig = rig_with("prerequisites-silent", HostPrerequisites::default());
    let d1 = who("d1", "daemon");
    for features in [&["heartbeat-v2"][..], &[]] {
        let request = said(features, Some(holding(&"a".repeat(64))));
        rig.coordinator.hear_prerequisites(&d1, &request);
    }
    assert_eq!(
        rig.coordinator.host_prerequisites().report_of(&id("d1")),
        None
    );
    assert_eq!(dropped(&rig), None);
}

/// Proves: a report that breaks a rule is dropped, counted under `prerequisites` and logged as a
/// dropped fact, and the report read before is kept: a digest that is not 64 lower-case hex
/// characters (short, upper case), a refusal whose digest is no digest, a refusal with no reason,
/// 17 states, a state whose name is not a prerequisite name, an empty name and a name twice; a
/// valid report after them is kept again.
///
/// Catches: a malformed report that replaces a good one, so that a host is sent a set it holds
/// again and again, or one that is not counted.
#[test]
fn a_report_that_breaks_a_rule_is_dropped_counted_and_the_last_one_kept() {
    let rig = rig_with("prerequisites-bad", HostPrerequisites::default());
    // After the rig, which installs a subscriber of its own.
    let log = Log::default();
    let _guard = log.install();
    let held = rig.coordinator.host_prerequisites();
    let d1 = who("d1", "daemon");
    let good = holding(&"a".repeat(64));
    rig.coordinator
        .hear_prerequisites(&d1, &announcing(Some(good.clone())));

    let refusal = |digest: &str, reason: &str| agent::PrerequisitesRefusal {
        digest: digest.to_owned(),
        reason: reason.to_owned(),
    };
    let with_states = |names: &[String]| agent::PrerequisitesReport {
        states: names
            .iter()
            .map(|name| state(name, agent::PrerequisiteStatus::Present))
            .collect(),
        ..holding(&"a".repeat(64))
    };
    let seventeen: Vec<String> = (0..17).map(|n| format!("p{n}")).collect();
    let bad = [
        holding("abc"),
        holding(&"A".repeat(64)),
        agent::PrerequisitesReport {
            refused: Some(refusal("short", "a reason")),
            ..holding(&"a".repeat(64))
        },
        agent::PrerequisitesReport {
            refused: Some(refusal(&"b".repeat(64), "")),
            ..holding(&"a".repeat(64))
        },
        with_states(&seventeen),
        with_states(&names(&["Xcode"])),
        with_states(&names(&[""])),
        with_states(&names(&[XCODE, "other", XCODE])),
    ];
    for (n, report) in bad.into_iter().enumerate() {
        rig.coordinator
            .hear_prerequisites(&d1, &announcing(Some(report)));
        assert_eq!(held.report_of(&id("d1")), Some(good.clone()), "{n}");
        assert_eq!(dropped(&rig), Some(u64::try_from(n).expect("a count") + 1));
    }
    let logged = log.text();
    assert!(
        logged.contains("the prerequisites report breaks a rule"),
        "{logged}"
    );

    let sixteen: Vec<String> = (0..16).map(|n| format!("p{n}")).collect();
    let full = with_states(&sixteen);
    rig.coordinator
        .hear_prerequisites(&d1, &announcing(Some(full.clone())));
    assert_eq!(held.report_of(&id("d1")), Some(full), "16 states are kept");
}

/// Proves: the version of a state is cut to 64 bytes and its reason and a refusal's reason to 256
/// bytes, each at a character boundary (a two-byte character that would straddle the cut is
/// dropped whole), and shorter texts and exact-length ones are kept whole.
///
/// Catches: a reader that keeps a long text a daemon wrote, and a cut that splits a character.
#[test]
fn the_texts_of_a_report_are_cut_on_a_character_boundary() {
    let long_reason = format!("{}\u{e9}\u{e9}", "x".repeat(255));
    let long_version = format!("{}\u{e9}\u{e9}", "9".repeat(63));
    let said = agent::PrerequisitesReport {
        digest: "a".repeat(64),
        refused: Some(agent::PrerequisitesRefusal {
            digest: "b".repeat(64),
            reason: long_reason.clone(),
        }),
        states: vec![agent::PrerequisiteState {
            version: long_version,
            reason: long_reason,
            failures: 4,
            next_try_unix: 77,
            seen_unix: 55,
            ..state(XCODE, agent::PrerequisiteStatus::Present)
        }],
    };
    let report = kept(&said).expect("a report");
    assert_eq!(report.refused.expect("a refusal").reason, "x".repeat(255));
    let cut = &report.states[0];
    assert_eq!(cut.version, "9".repeat(63));
    assert_eq!(cut.reason, "x".repeat(255));
    assert_eq!(
        (cut.failures, cut.next_try_unix, cut.seen_unix),
        (4, 77, 55),
        "the numbers are kept"
    );

    let exact = agent::PrerequisitesReport {
        states: vec![agent::PrerequisiteState {
            version: "v".repeat(64),
            reason: "y".repeat(256),
            ..state(XCODE, agent::PrerequisiteStatus::Failed)
        }],
        ..said
    };
    let report = kept(&exact).expect("a report");
    assert_eq!(report.states[0].version, "v".repeat(64));
    assert_eq!(report.states[0].reason, "y".repeat(256));
}

/// Proves: a status value this release does not know is kept as it came, and so is the one it
/// knows; a report is kept as it is written when it keeps every rule: the empty report, a digest
/// of 64 lower-case hex characters, a refusal with a digest and a reason, and states of every
/// status.
///
/// Catches: unknown data refused (a newer daemon's report dropped), and a valid report changed on
/// its way in.
#[test]
fn a_report_that_keeps_the_rules_is_kept_as_it_is_and_an_unknown_status_with_it() {
    let every: Vec<agent::PrerequisiteState> = [
        agent::PrerequisiteStatus::Unspecified,
        agent::PrerequisiteStatus::Pending,
        agent::PrerequisiteStatus::Present,
        agent::PrerequisiteStatus::Installing,
        agent::PrerequisiteStatus::Failed,
        agent::PrerequisiteStatus::NotApplicable,
        agent::PrerequisiteStatus::Unknown,
    ]
    .into_iter()
    .enumerate()
    .map(|(n, status)| state(&format!("p{n}"), status))
    .collect();
    let newer = agent::PrerequisiteState {
        status: 4242,
        ..state("newer", agent::PrerequisiteStatus::Unspecified)
    };
    let reports = [
        agent::PrerequisitesReport::default(),
        holding(&"0123456789abcdef".repeat(4)),
        agent::PrerequisitesReport {
            digest: "a".repeat(64),
            refused: Some(agent::PrerequisitesRefusal {
                digest: "b".repeat(64),
                reason: "prerequisite 1 is not a prerequisite name".to_owned(),
            }),
            states: every,
        },
        agent::PrerequisitesReport {
            states: vec![newer],
            ..holding("")
        },
    ];
    for report in reports {
        assert_eq!(kept(&report), Some(report.clone()), "{report:?}");
    }
}

/// Proves: a host's report is forgotten when the host is, and a report of another host is not.
///
/// Catches: the reports of retired hosts kept for as long as the coordinator runs.
#[test]
fn a_forgotten_host_has_no_report() {
    let held = HostPrerequisites::default();
    let rig = rig_with("prerequisites-forget", held.clone());
    for host in ["d1", "d2"] {
        rig.coordinator
            .hear_prerequisites(&who(host, "daemon"), &announcing(None));
    }
    held.forget(&id("d1"));
    assert_eq!(held.report_of(&id("d1")), None);
    assert!(held.report_of(&id("d2")).is_some());
}

// ---- what a host is sent --------------------------------------------------------------------

/// Proves: a daemon that announces the feature, of a group the file lists, is sent that group's
/// names and their digest, whole.
///
/// Catches: a set that is not sent, or sent with the digest of another set.
#[test]
fn a_host_of_a_listed_group_is_sent_its_groups_set() {
    let text = format!("{}{}", group("mac-build", &[XCODE]), group("web", &[]));
    let (held, _, _dir) = handle("prerequisites-send", &text);
    let rig = rig_with("prerequisites-send-rig", held);
    assert_eq!(
        sent(&rig, "d1", "mac-build", &announcing(None)),
        Some(Set {
            digest: wire_digest(&names(&[XCODE])),
            names: names(&[XCODE]),
        })
    );
    let web = sent(&rig, "d2", "web", &announcing(None)).expect("the other group's set");
    assert_eq!(web.names, Vec::<String>::new());
}

/// Proves: nothing is sent to a daemon that does not announce the feature (the same daemon that
/// does is sent the set), to any daemon when this coordinator does not advertise the feature, and
/// to any daemon when no file was given.
///
/// Catches: a field an older daemon refuses, a feature acted on without being offered, and a set
/// sent by a coordinator no one gave a file.
#[test]
fn nothing_is_sent_without_the_feature_on_both_sides_and_a_file() {
    let (held, _, _dir) = handle("prerequisites-gated", &group("mac-build", &[XCODE]));
    let mut rig = rig_with("prerequisites-gated-rig", held.clone());
    assert!(sent(&rig, "d1", "mac-build", &announcing(None)).is_some());
    for features in [&["heartbeat-v2"][..], &[]] {
        assert_eq!(sent(&rig, "d1", "mac-build", &said(features, None)), None);
    }

    rig.coordinator = start(&rig.dir, &rig.clock, "")
        .with_host_prerequisites(held)
        .with_features(&["heartbeat-v2"]);
    assert_eq!(sent(&rig, "d1", "mac-build", &announcing(None)), None);

    let none = rig_with("prerequisites-no-file", HostPrerequisites::default());
    assert_eq!(sent(&none, "d1", "mac-build", &announcing(None)), None);
}

/// Proves: a host whose group the file does not list is sent the empty set, with the digest of no
/// names, and so is a host approved in no group; a group listed with no names is sent the same.
///
/// Catches: a group taken out of the file that keeps its set on its hosts, and a host in no group
/// sent another group's set.
#[tokio::test]
async fn a_host_of_an_unlisted_group_or_of_none_is_sent_the_empty_set() {
    let text = format!("{}{}", group("mac-build", &[XCODE]), group("web", &[]));
    let (held, _, _dir) = handle("prerequisites-empty-set", &text);
    let enrollment = EnrollmentRig::new("prerequisites-empty-set-enrollment");
    let mut rig = coordinator_over("prerequisites-empty-set-coordinator", &enrollment).await;
    rig.coordinator = rig.coordinator.with_host_prerequisites(held);
    let empty = Set {
        names: Vec::new(),
        digest: EMPTY_DIGEST.to_owned(),
    };
    assert_eq!(
        sent(&rig, "d1", "storage", &announcing(None)),
        Some(empty.clone()),
        "a group the file does not list"
    );
    assert_eq!(
        sent(&rig, "d1", "web", &announcing(None)),
        Some(empty.clone()),
        "a group listed with none"
    );

    let key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&key, "machine-1"))
        .expect("enrolled");
    let request = ApproveRequest {
        role: UNGROUPED_ROLE.to_owned(),
        services: Vec::new(),
        ..approval(&key, "s1")
    };
    let caller = crate::operator::Caller::Operator(crate::tokens::Operator::named(
        "operator".parse().expect("a token name"),
    ));
    rig.coordinator
        .enrollment_approve(&caller, request)
        .expect("approved");
    assert_eq!(
        sent(&rig, "s1", "mac-build", &announcing(None)),
        Some(empty),
        "a host approved in no group, whatever its certificate says"
    );
}

/// Proves: once a host's report holds the set's digest it is sent nothing; a report that holds
/// another digest, the empty one, or a refusal of another set is sent the set again; a report that
/// refused this set is sent nothing, and so is one that holds it while it also refused another.
///
/// Catches: a set sent at every heartbeat, one never sent to a host that holds another, and a
/// refused set sent again and again.
#[test]
fn a_set_is_sent_until_the_host_reports_it_held_or_refused() {
    let (held, _, _dir) = handle("prerequisites-held", &group("mac-build", &[XCODE]));
    let rig = rig_with("prerequisites-held-rig", held);
    let set = wire_digest(&names(&[XCODE]));
    let refusal = |digest: &str| {
        Some(agent::PrerequisitesRefusal {
            digest: digest.to_owned(),
            reason: "prerequisite 1 breaks a rule".to_owned(),
        })
    };
    let reports = [
        (holding(""), true),
        (holding(&"a".repeat(64)), true),
        (holding(&set), false),
        (
            agent::PrerequisitesReport {
                refused: refusal(&"a".repeat(64)),
                ..holding("")
            },
            true,
        ),
        (
            agent::PrerequisitesReport {
                refused: refusal(&set),
                ..holding("")
            },
            false,
        ),
        (
            agent::PrerequisitesReport {
                refused: refusal(&"a".repeat(64)),
                ..holding(&set)
            },
            false,
        ),
    ];
    for (n, (report, resent)) in reports.into_iter().enumerate() {
        rig.coordinator
            .hear_prerequisites(&who("d1", "mac-build"), &announcing(Some(report)));
        let answer = sent(&rig, "d1", "mac-build", &announcing(None));
        assert_eq!(answer.is_some(), resent, "{n}");
    }
}

/// Proves: after a reload that changes a group's set, a host that reported the old set is sent
/// the new one at its next heartbeat, and nothing again once it reports the new digest; a group
/// taken out of the file is sent the empty set.
///
/// Catches: a change of the file that no host hears of until the coordinator restarts.
#[test]
fn a_changed_file_is_sent_at_the_next_heartbeat() {
    let (held, path, dir) = handle("prerequisites-change", &group("mac-build", &[]));
    let rig = rig_with("prerequisites-change-rig", held.clone());
    let d1 = who("d1", "mac-build");
    rig.coordinator
        .hear_prerequisites(&d1, &announcing(Some(holding(EMPTY_DIGEST))));
    assert_eq!(sent(&rig, "d1", "mac-build", &announcing(None)), None);

    std::fs::write(&path, group("mac-build", &[XCODE])).expect("written");
    assert_eq!(held.reload(), Reload::Applied);
    let answer = sent(&rig, "d1", "mac-build", &announcing(None)).expect("the new set");
    assert_eq!(answer.names, [XCODE]);
    rig.coordinator
        .hear_prerequisites(&d1, &announcing(Some(holding(&answer.digest))));
    assert_eq!(sent(&rig, "d1", "mac-build", &announcing(None)), None);

    file(dir.path(), "");
    assert_eq!(held.reload(), Reload::Applied);
    let taken = sent(&rig, "d1", "mac-build", &announcing(None)).expect("the empty set");
    assert_eq!(
        (taken.names, taken.digest),
        (Vec::new(), EMPTY_DIGEST.to_owned())
    );
}

/// Proves: a host whose record the enrollment store cannot read is sent nothing at that
/// heartbeat, and the set once it reads again.
///
/// Catches: a set sent to a group guessed from the certificate, or a heartbeat failed, when the
/// record cannot be read.
#[tokio::test]
async fn a_host_whose_record_cannot_be_read_is_sent_nothing_for_that_heartbeat() {
    let (held, _, _dir) = handle(
        "prerequisites-unreadable-record",
        &group("storage", &[XCODE]),
    );
    let enrollment = EnrollmentRig::new("prerequisites-record-enrollment");
    let mut rig = coordinator_over("prerequisites-record-coordinator", &enrollment).await;
    rig.coordinator = rig.coordinator.with_host_prerequisites(held);
    let host_key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&host_key, "machine-1"))
        .expect("enrolled");
    let caller = crate::operator::Caller::Operator(crate::tokens::Operator::named(
        "operator".parse().expect("a token name"),
    ));
    rig.coordinator
        .enrollment_approve(&caller, approval(&host_key, "s1"))
        .expect("approved");
    assert!(sent(&rig, "s1", "storage", &announcing(None)).is_some());

    let record = enrollment.files.store.join("hosts").join("s1.pb");
    let good = std::fs::read(&record).expect("the host record");
    std::fs::write(&record, b"\xff\xff\xff").expect("damaged");
    assert_eq!(sent(&rig, "s1", "storage", &announcing(None)), None);
    std::fs::write(&record, good).expect("restored");
    assert!(sent(&rig, "s1", "storage", &announcing(None)).is_some());
}

/// Proves: the feature is advertised by name, so a daemon's announcement is agreed.
///
/// Catches: a feature acted on and not advertised, which no daemon would then announce.
#[test]
fn the_feature_is_advertised() {
    assert_eq!(FEATURE, "prerequisites-v1");
    assert!(crate::features::FEATURES.contains(&FEATURE));
}

/// Catches: `prerequisites-v1` advertised and not acted on: a daemon that announces it is sent its
/// group's set, and the same daemon that does not announce it is sent none.
pub(crate) fn prerequisites_v1_is_acted_on() {
    let (held, _, _dir) = handle("prerequisites-acted-on", &group("mac-build", &[XCODE]));
    let rig = rig_with("prerequisites-acted-on-rig", held);
    assert!(sent(&rig, "d1", "mac-build", &announcing(None)).is_some());
    assert_eq!(
        sent(&rig, "d1", "mac-build", &said(&["heartbeat-v2"], None)),
        None
    );
}

// ---- a retired host -------------------------------------------------------------------------

/// Proves: the report of a host an operator retires is forgotten with the host, and so is that of
/// a retiring host whose final report frees its slot; the report of a host that stays is kept.
///
/// Catches: the reports of retired hosts kept for as long as the coordinator runs, and a
/// retirement that forgets another host's report.
#[test]
fn a_retired_host_has_no_report() {
    let f = Fixture::new("prerequisites-retire");
    for (n, host) in ["db-1", "db-2", "db-3"].into_iter().enumerate() {
        f.approve(&format!("m-{n}"), host, APPROVED, false);
        f.beat(host, "storage", FIRST, vec![runs(WEB, "1.0")]);
        f.rig
            .coordinator
            .hear_prerequisites(&who(host, "storage"), &announcing(None));
    }
    let held = f.rig.coordinator.host_prerequisites();
    assert!(
        ["db-1", "db-2", "db-3"]
            .iter()
            .all(|h| held.report_of(&id(h)).is_some())
    );

    f.rig
        .coordinator
        .retire_host(
            &operator(),
            &RetireRequest {
                node_id: "db-1".to_owned(),
                reason: "gone".to_owned(),
            },
        )
        .expect("retired");
    assert_eq!(held.report_of(&id("db-1")), None);

    f.enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("db-2").expect("a host id"))
        .expect("denied");
    assert!(
        f.rig
            .coordinator
            .final_report(who("db-2", "storage"), daemon(vec![runs(WEB, "1.0")]))
    );
    assert_eq!(held.report_of(&id("db-2")), None);
    assert!(held.report_of(&id("db-3")).is_some(), "a host that stays");
}
