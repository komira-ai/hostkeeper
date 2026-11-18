//! The operator keys file and what the coordinator keeps and sends of it
//! ([`crate::operator_keys`]): the file's rules and refusals, its reload, the reports a heartbeat
//! carries and the set a host is sent. The answer over gRPC and the operator's view are in
//! `grpc/tests/operator_keys.rs`.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::{ApproveRequest, RetireRequest};
use hostkeeper_wire::operator_keys::{KEY_TYPES, digest as wire_digest};

use super::{FEATURE, MAX_FILE_BYTES, OperatorKeys, OperatorKeysError, Reload, Set, kept, parse};
use crate::enrollment::UNGROUPED_ROLE;
use crate::enrollment::tests::support::{
    HostKey, Rig as EnrollmentRig, approval, coordinator_over, enroll_request,
};
use crate::first_serving_tests::{APPROVED, FIRST, Fixture, WEB, daemon};
use crate::marks_support::{operator, rig, runs, start, who};
use crate::testutil::{Log, OPERATOR, Rig, id, scratch};

/// The SHA-256 of no bytes: the digest of the empty set.
const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// A blob of `kind` as `ssh-keygen` would write it, with `fill` after the type: made at run
/// time, so no key material is kept in the repository.
pub(crate) fn blob(kind: &str, fill: u8) -> String {
    let mut bytes = u32::try_from(kind.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend_from_slice(&[fill; 32]);
    STANDARD.encode(bytes)
}

/// An `ssh-ed25519` key line of the blob of `fill`, with `comment` after it (none when empty).
pub(crate) fn key(fill: u8, comment: &str) -> String {
    let blob = blob("ssh-ed25519", fill);
    match comment {
        "" => format!("ssh-ed25519 {blob}"),
        _ => format!("ssh-ed25519 {blob} {comment}"),
    }
}

/// The TOML of a group `name` holding `keys`, each as given.
pub(crate) fn group(name: &str, keys: &[String]) -> String {
    let listed: Vec<String> = keys.iter().map(|key| format!("\"{key}\"")).collect();
    format!("[host-groups.{name}]\nkeys = [{}]\n", listed.join(", "))
}

/// Writes `text` to `operator-keys.toml` in `dir` with mode 0644 and answers its path.
pub(crate) fn file(dir: &Path, text: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("operator-keys.toml");
    std::fs::write(&path, text).expect("written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    path
}

/// The handle over a file of `text`, in a scratch directory named `name`.
fn handle(name: &str, text: &str) -> (OperatorKeys, PathBuf, hostkeeper_testkit::Scratch) {
    let dir = scratch(name);
    let path = file(dir.path(), text);
    let keys = OperatorKeys::from_file(&path).expect("the file is read");
    (keys, path, dir)
}

/// A report holding `digest` in force.
fn holding(digest: &str) -> agent::OperatorKeysReport {
    agent::OperatorKeysReport {
        digest: digest.to_owned(),
        ..agent::OperatorKeysReport::default()
    }
}

/// A heartbeat over gRPC that announces `features` and carries `report`.
fn said(features: &[&str], report: Option<agent::OperatorKeysReport>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: features.iter().map(|f| (*f).to_owned()).collect(),
        operator_keys: report,
        ..agent::HeartbeatRequest::default()
    }
}

/// A heartbeat of a daemon that announces the feature and carries `report`.
fn announcing(report: Option<agent::OperatorKeysReport>) -> agent::HeartbeatRequest {
    said(&["heartbeat-v2", FEATURE], report)
}

/// A rig whose coordinator holds `keys`.
fn rig_with(name: &str, keys: OperatorKeys) -> Rig {
    let mut rig = rig(name, "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_operator_keys(keys);
    rig
}

/// The keys the coordinator of `rig` sends the host `host` of the group `group` that sends
/// `request`.
fn sent(rig: &Rig, host: &str, group: &str, request: &agent::HeartbeatRequest) -> Option<Set> {
    rig.coordinator
        .operator_keys_for(&who(host, group), request)
        .map(|keys| Set {
            keys: keys.keys,
            digest: keys.digest,
        })
}

/// What the status counts of dropped operator keys reports.
fn dropped(rig: &Rig) -> Option<u64> {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("operator_keys").copied()
}

// ---- the file -------------------------------------------------------------------------------

/// Proves: a file of two groups, one with two keys and one with none, is read as the sets of
/// those groups, each key as it is written and each digest the one the daemon computes
/// (`hostkeeper_wire::operator_keys::digest`); a group the file does not name has no set.
///
/// Catches: a key lost or reordered, a digest of another rule than the daemon's.
#[test]
fn a_file_of_groups_is_read_as_their_sets() {
    let keys = vec![key(1, "ops laptop"), key(2, "")];
    let text = format!("{}{}", group("farm-linux", &keys), group("farm-mac", &[]));
    let sets = parse(&text).expect("the file is read");
    assert_eq!(sets.len(), 2);
    assert_eq!(
        sets["farm-linux"],
        Set {
            digest: wire_digest(&keys),
            keys
        }
    );
    assert_eq!(
        sets["farm-mac"],
        Set {
            keys: Vec::new(),
            digest: EMPTY_DIGEST.to_owned()
        }
    );
    assert_eq!(sets.get("other"), None);
}

/// Proves: every key type the wire rule lists is read from a file, and a key written with extra
/// white space and a comment of several words is kept with single spaces, so that its digest is
/// that of the normalised set.
///
/// Catches: a type of the list refused, and a digest over the key as written rather than as the
/// daemon will write it.
#[test]
fn keys_are_normalised_before_their_digest() {
    for kind in KEY_TYPES {
        let blob = blob(kind, 9);
        let text = group(
            "farm-linux",
            &[format!("  {kind}   {blob}   ops\t laptop  ")],
        );
        let sets = parse(&text).expect(kind);
        let normal = vec![format!("{kind} {blob} ops laptop")];
        assert_eq!(sets["farm-linux"].keys, normal, "{kind}");
        assert_eq!(sets["farm-linux"].digest, wire_digest(&normal), "{kind}");
    }
}

/// Proves: a file with no `host-groups` table, an empty one and an empty `[host-groups]` table
/// all name no group: they are read, and a handle over them is given a file but lists nothing.
///
/// Catches: an empty file refused, so that an operator who has not set up keys yet cannot start
/// the coordinator with the flag.
#[test]
fn a_file_with_no_groups_is_read_and_lists_none() {
    for text in ["", "# no keys yet\n", "[host-groups]\n"] {
        assert_eq!(parse(text).expect(text), Default::default(), "{text:?}");
    }
    let (keys, _, _dir) = handle("operator-keys-empty", "");
    assert!(keys.is_given());
    assert_eq!(keys.set_of("farm-linux"), None);
}

/// Proves: the file's refusals say, exactly, which rule is broken, name the flag, the group and
/// the key's place, and never hold a key's text: not TOML, a top-level key that is not
/// `host-groups`, `host-groups` that is not a table, a group name that is not one, an entry that
/// is not a table, a key of an entry other than `keys`, no `keys`, `keys` that is not an array,
/// an element that is not a string, an element that is not a key (by each rule of a line), more
/// than 64 keys, a key twice, a key over 4096 bytes.
///
/// Catches: a refusal that gives the reader nothing to find the line by, one that prints a key,
/// and a rule of the file that is not checked.
#[test]
fn a_file_that_breaks_a_rule_is_refused_naming_the_flag_the_group_and_the_place() {
    let first = key(1, "ops laptop");
    let second = key(2, "");
    let ed = blob("ssh-ed25519", 3);
    let flag = "--operator-keys-file: ";
    let in_build = |why: &str| format!("{flag}host group farm-linux: {why}");
    let many: Vec<String> = (0..65).map(|n| key(n, "")).collect();
    let long = key(4, &"c".repeat(4100));
    let cases: Vec<(String, String)> = vec![
        (
            format!("[tokens]\nname = \"{first}\"\n"),
            format!("{flag}the key \"tokens\" is not known; the file holds only host-groups"),
        ),
        (
            "host-groups = [\"build\"]\n".to_owned(),
            format!("{flag}host-groups is not a table of host groups"),
        ),
        (
            format!("[host-groups.Farm-linux]\nkeys = [\"{first}\"]\n"),
            format!(
                "{flag}\"Farm-linux\" is not a host group name (1 to 32 bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
            ),
        ),
        (
            format!("[host-groups]\nfarm-linux = [\"{first}\"]\n"),
            in_build("it is not a table holding keys"),
        ),
        (
            format!("[host-groups.farm-linux]\nkeys = []\nuser = \"{first}\"\n"),
            in_build("the key \"user\" is not known; it holds only keys"),
        ),
        (
            "[host-groups.farm-linux]\n".to_owned(),
            in_build("keys is missing (write keys = [] for none)"),
        ),
        (
            format!("[host-groups.farm-linux]\nkeys = \"{first}\"\n"),
            in_build("keys is not an array of strings"),
        ),
        (
            format!("[host-groups.farm-linux]\nkeys = [\"{first}\", 7]\n"),
            in_build("key 2 is not a string"),
        ),
        (
            format!("[host-groups.farm-linux]\nkeys = [\"{first}\", \"not a key\"]\n"),
            in_build("key 2: not <type> <base64> [comment] with a known key type first"),
        ),
        (
            "[host-groups.farm-linux]\nkeys = [\"ssh-ed25519 AAAA\"]\n".to_owned(),
            in_build("key 1: a key blob that is not base64 of its key type"),
        ),
        (
            format!("[host-groups.farm-linux]\nkeys = [\"ssh-ed25519 {ed} caf\u{e9}\"]\n"),
            in_build("key 1: a comment that is not printable ASCII"),
        ),
        (
            group("farm-linux", &many),
            in_build("65 keys, more than the 64 a set may hold"),
        ),
        (
            group(
                "farm-linux",
                &[first.clone(), second.clone(), format!("{first} again")],
            ),
            in_build("key 3 has the type and blob of key 1"),
        ),
        (
            group("farm-linux", std::slice::from_ref(&long)),
            in_build("key 1 is longer than 4096 bytes"),
        ),
    ];
    for (text, said) in cases {
        let why = parse(&text).expect_err(&text);
        assert_eq!(why.to_string(), said, "{text}");
        for secret in [&first, &second, &long] {
            assert!(!why.to_string().contains(secret.as_str()), "{why}");
        }
        for part in [&ed, &blob("ssh-ed25519", 1)] {
            assert!(!why.to_string().contains(part.as_str()), "{why}");
        }
    }
}

/// Proves: text that is not TOML is refused naming the flag and that it is not TOML, with none of
/// the file's text in the message.
///
/// Catches: a file of another format read as empty, and a parser message that quotes the line.
#[test]
fn text_that_is_not_toml_is_refused() {
    let first = key(1, "ops laptop");
    let why = parse(&format!("host-groups.farm-linux = [{first}\n")).expect_err("not TOML");
    assert!(matches!(why, OperatorKeysError::NotToml(_)), "{why}");
    let said = why.to_string();
    assert!(
        said.starts_with("--operator-keys-file: not TOML: "),
        "{said}"
    );
    assert!(!said.contains(&first), "{said}");
}

/// Proves: the file is a plain file: any owner's mode is read (0644 and 0666), and one of exactly
/// the largest size, 256 KiB, is read while one byte more is refused naming its size; neither
/// message names the path.
///
/// Catches: a mode rule of a secret file applied to public keys, and a size bound off by one.
#[test]
fn any_mode_is_read_and_the_size_is_bounded() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("operator-keys-size");
    let (comment, rest) = ("#", usize::try_from(MAX_FILE_BYTES).expect("a size") - 1);
    let path = file(dir.path(), &format!("{comment}{}", "x".repeat(rest)));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("mode");
    assert!(OperatorKeys::from_file(&path).is_ok(), "256 KiB exactly");

    std::fs::write(&path, format!("{comment}{}", "x".repeat(rest + 1))).expect("written");
    let why = OperatorKeys::from_file(&path).expect_err("over the bound");
    assert_eq!(
        why.to_string(),
        "--operator-keys-file: 262145 bytes, more than the 262144 the file may hold"
    );
}

/// Proves: a path that is a directory is refused as not a regular file, a path that does not
/// exist and a file that is not UTF-8 are refused with what reading them said, each naming the
/// flag and never the path.
///
/// Catches: a directory read as a file, and a refusal that leaves out the flag or shows the path.
#[test]
fn a_file_that_cannot_be_read_is_refused_without_its_path() {
    let dir = scratch("operator-keys-unreadable");
    let missing = dir.path().join("absent.toml");
    let bytes = dir.path().join("bytes.toml");
    std::fs::write(&bytes, b"\xff\xfe").expect("written");
    for (path, said) in [
        (dir.path(), "--operator-keys-file: not a regular file"),
        (
            missing.as_path(),
            "--operator-keys-file: No such file or directory (os error 2)",
        ),
        (
            bytes.as_path(),
            "--operator-keys-file: stream did not contain valid UTF-8",
        ),
    ] {
        let why = OperatorKeys::from_file(path).expect_err(said);
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
    let keys = OperatorKeys::default();
    assert!(!keys.is_given());
    assert_eq!(keys.reload(), Reload::Unchanged);
    assert_eq!(keys.set_of("farm-linux"), None);
}

/// Proves: a reload of a file that holds the sets in use is unchanged and logs nothing; a file
/// whose keys changed is applied, logged at info with the number of groups, and the new set is
/// the one listed from then on.
///
/// Catches: a change that is not picked up, and a reload that logs a change when there is none.
#[test]
fn a_changed_file_is_applied_and_an_unchanged_one_is_not() {
    let log = Log::default();
    let _guard = log.install();
    let first = vec![key(1, "ops laptop")];
    let (keys, path, _dir) = handle("operator-keys-reload", &group("farm-linux", &first));
    assert_eq!(keys.reload(), Reload::Unchanged);
    assert_eq!(log.text(), "");

    let second = vec![key(2, "ops laptop")];
    let text = format!("{}{}", group("farm-linux", &second), group("farm-mac", &[]));
    std::fs::write(&path, text).expect("written");
    assert_eq!(keys.reload(), Reload::Applied);
    assert_eq!(keys.set_of("farm-linux").expect("a set").keys, second);
    assert!(keys.set_of("farm-mac").is_some());
    let logged = log.text();
    assert!(logged.contains("INFO"), "{logged}");
    assert!(
        logged.contains("the operator keys file changed: its keys are in use"),
        "{logged}"
    );
    assert!(logged.contains("groups=2"), "{logged}");
    assert!(!logged.contains(&second[0]), "{logged}");
}

/// Proves: a file that is refused after a change, a file that is gone and a file that is not
/// TOML each reload as rejected, log a warning that names the rule and not the key, and leave the
/// sets in use; the file made right again is applied.
///
/// Catches: a bad edit that takes every host's keys away, and a warning that prints a key.
#[test]
fn a_file_that_is_refused_on_reload_keeps_the_sets_in_use() {
    let log = Log::default();
    let _guard = log.install();
    let held = vec![key(1, "ops laptop")];
    let (keys, path, _dir) = handle("operator-keys-rejected", &group("farm-linux", &held));
    let before = keys.set_of("farm-linux");

    let twice = group(
        "farm-linux",
        &[held[0].clone(), format!("{} again", held[0])],
    );
    std::fs::write(&path, twice).expect("written");
    assert_eq!(keys.reload(), Reload::Rejected);
    assert_eq!(keys.set_of("farm-linux"), before);
    let logged = log.text();
    assert!(logged.contains("WARN"), "{logged}");
    assert!(
        logged.contains("the operator keys file was not read again: the keys in use stay"),
        "{logged}"
    );
    assert!(
        logged.contains("key 2 has the type and blob of key 1"),
        "{logged}"
    );
    assert!(!logged.contains(&held[0]), "{logged}");

    std::fs::write(&path, "[host-groups").expect("written");
    assert_eq!(keys.reload(), Reload::Rejected);
    std::fs::remove_file(&path).expect("removed");
    assert_eq!(keys.reload(), Reload::Rejected);
    assert_eq!(keys.set_of("farm-linux"), before);

    let again = vec![key(5, "ops laptop")];
    file(
        path.parent().expect("a directory"),
        &group("farm-linux", &again),
    );
    assert_eq!(keys.reload(), Reload::Applied);
    assert_eq!(keys.set_of("farm-linux").expect("a set").keys, again);
}

// ---- what a heartbeat reports ---------------------------------------------------------------

/// Proves: the report of a daemon that announces the feature is kept as it says, the last one
/// replacing the one before; a heartbeat of such a daemon that carries none counts as the empty
/// report, which replaces the one before; and the report is the host's own: another host has none.
///
/// Catches: a report lost, one that does not replace the older, and a daemon that announced the
/// feature and never reported held as one that sent nothing.
#[test]
fn the_last_report_of_a_daemon_that_announces_the_feature_is_kept() {
    let rig = rig_with("operator-keys-hear", OperatorKeys::default());
    let keys = rig.coordinator.operator_keys();
    let host = id("d1");
    let digest = "a".repeat(64);
    let refused = agent::OperatorKeysReport {
        digest: digest.clone(),
        refused: Some(agent::OperatorKeysRefusal {
            digest: "b".repeat(64),
            reason: "key 2 has the type and blob of key 1".to_owned(),
        }),
        failed: "the file could not be written".to_owned(),
    };

    let d1 = who("d1", "daemon");
    rig.coordinator
        .hear_operator_keys(&d1, &announcing(Some(refused.clone())));
    assert_eq!(keys.report_of(&host), Some(refused));
    rig.coordinator
        .hear_operator_keys(&d1, &announcing(Some(holding(&"c".repeat(64)))));
    assert_eq!(keys.report_of(&host), Some(holding(&"c".repeat(64))));
    rig.coordinator.hear_operator_keys(&d1, &announcing(None));
    assert_eq!(
        keys.report_of(&host),
        Some(agent::OperatorKeysReport::default())
    );
    assert_eq!(keys.report_of(&id("d2")), None);
    assert_eq!(dropped(&rig), None);
}

/// Proves: a daemon that does not announce the feature has its report ignored, however it is
/// written, and a host that never announced it has none kept.
///
/// Catches: a report of an older daemon (which has no such field) or of a different protocol
/// kept and used to hold a set back.
#[test]
fn a_report_of_a_daemon_that_did_not_announce_the_feature_is_ignored() {
    let rig = rig_with("operator-keys-silent", OperatorKeys::default());
    let d1 = who("d1", "daemon");
    for features in [&["heartbeat-v2"][..], &[]] {
        let request = said(features, Some(holding(&"a".repeat(64))));
        rig.coordinator.hear_operator_keys(&d1, &request);
    }
    assert_eq!(rig.coordinator.operator_keys().report_of(&id("d1")), None);
    assert_eq!(dropped(&rig), None);
}

/// Proves: a report that breaks a rule is dropped, counted under `operator_keys` and logged as a
/// dropped fact, and the report read before is kept: a digest that is not 64 lower-case hex
/// characters (short, upper case), a refusal whose digest is no digest, and a refusal with no
/// reason; a valid report after them is kept again.
///
/// Catches: a malformed report that replaces a good one, so that a host is sent a set it holds
/// again and again, or one that is not counted.
#[test]
fn a_report_that_breaks_a_rule_is_dropped_counted_and_the_last_one_kept() {
    let rig = rig_with("operator-keys-bad", OperatorKeys::default());
    // After the rig, which installs a subscriber of its own.
    let log = Log::default();
    let _guard = log.install();
    let keys = rig.coordinator.operator_keys();
    let d1 = who("d1", "daemon");
    let good = holding(&"a".repeat(64));
    rig.coordinator
        .hear_operator_keys(&d1, &announcing(Some(good.clone())));

    let refusal = |digest: &str, reason: &str| agent::OperatorKeysRefusal {
        digest: digest.to_owned(),
        reason: reason.to_owned(),
    };
    let bad = [
        holding("abc"),
        holding(&"A".repeat(64)),
        agent::OperatorKeysReport {
            refused: Some(refusal("short", "a reason")),
            ..holding(&"a".repeat(64))
        },
        agent::OperatorKeysReport {
            refused: Some(refusal(&"b".repeat(64), "")),
            ..holding(&"a".repeat(64))
        },
    ];
    for (n, report) in bad.into_iter().enumerate() {
        rig.coordinator
            .hear_operator_keys(&d1, &announcing(Some(report)));
        assert_eq!(keys.report_of(&id("d1")), Some(good.clone()), "{n}");
        assert_eq!(dropped(&rig), Some(u64::try_from(n).expect("a count") + 1));
    }
    let logged = log.text();
    assert!(
        logged.contains("the operator keys report breaks a rule"),
        "{logged}"
    );

    let next = holding(&"c".repeat(64));
    rig.coordinator
        .hear_operator_keys(&d1, &announcing(Some(next.clone())));
    assert_eq!(keys.report_of(&id("d1")), Some(next));
}

/// Proves: the reason of a refusal and the failure of a report are cut to 256 bytes at a
/// character boundary (a two-byte character that would straddle the cut is dropped whole), and
/// shorter ones are kept whole.
///
/// Catches: a reader that keeps a long text a daemon wrote, and a cut that splits a character.
#[test]
fn the_texts_of_a_report_are_cut_at_256_bytes_on_a_character_boundary() {
    let long = format!("{}\u{e9}\u{e9}", "x".repeat(255));
    let said = agent::OperatorKeysReport {
        digest: "a".repeat(64),
        refused: Some(agent::OperatorKeysRefusal {
            digest: "b".repeat(64),
            reason: long.clone(),
        }),
        failed: long,
    };
    let kept_report = kept(&said).expect("a report");
    assert_eq!(kept_report.failed, "x".repeat(255));
    assert_eq!(
        kept_report.refused.expect("a refusal").reason,
        "x".repeat(255)
    );

    let exact = "y".repeat(256);
    let said = agent::OperatorKeysReport {
        failed: exact.clone(),
        ..said
    };
    assert_eq!(kept(&said).expect("a report").failed, exact);
}

/// Proves: a host's report is forgotten when the host is, and a report of another host is not.
///
/// Catches: the reports of retired hosts kept for as long as the coordinator runs.
#[test]
fn a_forgotten_host_has_no_report() {
    let keys = OperatorKeys::default();
    let rig = rig_with("operator-keys-forget", keys.clone());
    for host in ["d1", "d2"] {
        rig.coordinator
            .hear_operator_keys(&who(host, "daemon"), &announcing(None));
    }
    keys.forget(&id("d1"));
    assert_eq!(keys.report_of(&id("d1")), None);
    assert!(keys.report_of(&id("d2")).is_some());
}

// ---- what a host is sent --------------------------------------------------------------------

/// Proves: a daemon that announces the feature, of a group the file lists, is sent that group's
/// keys and their digest, whole.
///
/// Catches: a set that is not sent, or sent with the digest of another set.
#[test]
fn a_host_of_a_listed_group_is_sent_its_groups_keys() {
    let keys = vec![key(1, "ops laptop"), key(2, "")];
    let other = vec![key(3, "")];
    let text = format!("{}{}", group("farm-linux", &keys), group("web", &other));
    let (handle, _, _dir) = handle("operator-keys-send", &text);
    let rig = rig_with("operator-keys-send-rig", handle);
    assert_eq!(
        sent(&rig, "d1", "farm-linux", &announcing(None)),
        Some(Set {
            digest: wire_digest(&keys),
            keys
        })
    );
    let web = sent(&rig, "d2", "web", &announcing(None)).expect("the other group's set");
    assert_eq!(web.keys, other);
}

/// Proves: nothing is sent to a daemon that does not announce the feature (the same daemon that
/// does is sent the set), to any daemon when this coordinator does not advertise the feature, and
/// to any daemon when no file was given.
///
/// Catches: a field an older daemon refuses, a feature acted on without being offered, and keys
/// sent by a coordinator no one gave a file.
#[test]
fn nothing_is_sent_without_the_feature_on_both_sides_and_a_file() {
    let keys = vec![key(1, "ops laptop")];
    let (handle, _, _dir) = handle("operator-keys-gated", &group("farm-linux", &keys));
    let mut rig = rig_with("operator-keys-gated-rig", handle.clone());
    assert!(sent(&rig, "d1", "farm-linux", &announcing(None)).is_some());
    for features in [&["heartbeat-v2"][..], &[]] {
        assert_eq!(sent(&rig, "d1", "farm-linux", &said(features, None)), None);
    }

    rig.coordinator = start(&rig.dir, &rig.clock, "")
        .with_operator_keys(handle)
        .with_features(&["heartbeat-v2"]);
    assert_eq!(sent(&rig, "d1", "farm-linux", &announcing(None)), None);

    let none = rig_with("operator-keys-no-file", OperatorKeys::default());
    assert_eq!(sent(&none, "d1", "farm-linux", &announcing(None)), None);
}

/// Proves: a host whose group the file does not list is sent the empty set, with the digest of no
/// keys, and so is a host approved in no group; a group listed with no keys is sent the same.
///
/// Catches: a group taken out of the file that keeps its keys on its hosts, and a host in no
/// group sent another group's keys.
#[tokio::test]
async fn a_host_of_an_unlisted_group_or_of_none_is_sent_the_empty_set() {
    let keys = vec![key(1, "ops laptop")];
    let text = format!("{}{}", group("farm-linux", &keys), group("farm-mac", &[]));
    let (handle, _, _dir) = handle("operator-keys-empty-set", &text);
    let enrollment = EnrollmentRig::new("operator-keys-empty-set-enrollment");
    let mut rig = coordinator_over("operator-keys-empty-set-coordinator", &enrollment).await;
    rig.coordinator = rig.coordinator.with_operator_keys(handle);
    let empty = Set {
        keys: Vec::new(),
        digest: EMPTY_DIGEST.to_owned(),
    };
    assert_eq!(
        sent(&rig, "d1", "storage", &announcing(None)),
        Some(empty.clone()),
        "a group the file does not list"
    );
    assert_eq!(
        sent(&rig, "d1", "farm-mac", &announcing(None)),
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
    let caller = crate::operator::Caller::Operator("operator".parse().expect("a token name"));
    rig.coordinator
        .enrollment_approve(&caller, request)
        .expect("approved");
    assert_eq!(
        sent(&rig, "s1", "farm-linux", &announcing(None)),
        Some(empty),
        "a host approved in no group, whatever its certificate says"
    );
}

/// Proves: once a host's report holds the set's digest in force it is sent nothing; a report that
/// holds another digest, the empty one, or a refusal of another set is sent the set again; a
/// report that refused this set is sent nothing, and so is one that holds it in force while it
/// also refused another.
///
/// Catches: a set sent at every heartbeat, one never sent to a host that holds another, and a
/// refused set sent again and again.
#[test]
fn a_set_is_sent_until_the_host_reports_it_in_force_or_refused() {
    let keys = vec![key(1, "ops laptop")];
    let (handle, _, _dir) = handle("operator-keys-held", &group("farm-linux", &keys));
    let rig = rig_with("operator-keys-held-rig", handle);
    let set = wire_digest(&keys);
    let refusal = |digest: &str| {
        Some(agent::OperatorKeysRefusal {
            digest: digest.to_owned(),
            reason: "key 1 breaks a rule".to_owned(),
        })
    };
    let reports = [
        (holding(""), true),
        (holding(&"a".repeat(64)), true),
        (holding(&set), false),
        (
            agent::OperatorKeysReport {
                refused: refusal(&"a".repeat(64)),
                ..holding("")
            },
            true,
        ),
        (
            agent::OperatorKeysReport {
                refused: refusal(&set),
                ..holding("")
            },
            false,
        ),
        (
            agent::OperatorKeysReport {
                refused: refusal(&"a".repeat(64)),
                ..holding(&set)
            },
            false,
        ),
    ];
    for (n, (report, resent)) in reports.into_iter().enumerate() {
        rig.coordinator
            .hear_operator_keys(&who("d1", "farm-linux"), &announcing(Some(report)));
        let answer = sent(&rig, "d1", "farm-linux", &announcing(None));
        assert_eq!(answer.is_some(), resent, "{n}");
    }
}

/// Proves: after a reload that changes a group's keys, a host that reported the old set in force
/// is sent the new one at its next heartbeat, and nothing again once it reports the new digest; a
/// group taken out of the file is sent the empty set.
///
/// Catches: a change of the file that no host hears of until the coordinator restarts.
#[test]
fn a_changed_file_is_sent_at_the_next_heartbeat() {
    let first = vec![key(1, "ops laptop")];
    let (handle, path, dir) = handle("operator-keys-change", &group("farm-linux", &first));
    let rig = rig_with("operator-keys-change-rig", handle.clone());
    let d1 = who("d1", "farm-linux");
    rig.coordinator
        .hear_operator_keys(&d1, &announcing(Some(holding(&wire_digest(&first)))));
    assert_eq!(sent(&rig, "d1", "farm-linux", &announcing(None)), None);

    let second = vec![key(1, "ops laptop"), key(2, "ci")];
    std::fs::write(&path, group("farm-linux", &second)).expect("written");
    assert_eq!(handle.reload(), Reload::Applied);
    let answer = sent(&rig, "d1", "farm-linux", &announcing(None)).expect("the new set");
    assert_eq!(answer.keys, second);
    rig.coordinator
        .hear_operator_keys(&d1, &announcing(Some(holding(&answer.digest))));
    assert_eq!(sent(&rig, "d1", "farm-linux", &announcing(None)), None);

    file(dir.path(), "");
    assert_eq!(handle.reload(), Reload::Applied);
    let taken = sent(&rig, "d1", "farm-linux", &announcing(None)).expect("the empty set");
    assert_eq!(
        (taken.keys, taken.digest),
        (Vec::new(), EMPTY_DIGEST.to_owned())
    );
}

/// Proves: a host whose record the enrollment store cannot read is sent nothing at that
/// heartbeat, and the set once it reads again.
///
/// Catches: keys sent to a group guessed from the certificate, or a heartbeat failed, when the
/// record cannot be read.
#[tokio::test]
async fn a_host_whose_record_cannot_be_read_is_sent_nothing_for_that_heartbeat() {
    let keys = vec![key(1, "ops laptop")];
    let (handle, _, _dir) = handle("operator-keys-unreadable-record", &group("storage", &keys));
    let enrollment = EnrollmentRig::new("operator-keys-record-enrollment");
    let mut rig = coordinator_over("operator-keys-record-coordinator", &enrollment).await;
    rig.coordinator = rig.coordinator.with_operator_keys(handle);
    let host_key = HostKey::new();
    enrollment
        .enrollment
        .enroll(enroll_request(&host_key, "machine-1"))
        .expect("enrolled");
    let caller = crate::operator::Caller::Operator("operator".parse().expect("a token name"));
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
    assert_eq!(FEATURE, "operator-keys-v1");
    assert!(crate::features::FEATURES.contains(&FEATURE));
}

/// Catches: `operator-keys-v1` advertised and not acted on: a daemon that announces it is sent
/// its group's keys, and the same daemon that does not announce it is sent none.
pub(crate) fn operator_keys_v1_is_acted_on() {
    let keys = vec![key(1, "ops laptop")];
    let (handle, _, _dir) = handle("operator-keys-acted-on", &group("farm-linux", &keys));
    let rig = rig_with("operator-keys-acted-on-rig", handle);
    assert!(sent(&rig, "d1", "farm-linux", &announcing(None)).is_some());
    assert_eq!(
        sent(&rig, "d1", "farm-linux", &said(&["heartbeat-v2"], None)),
        None
    );
}

// ---- the report's shape ---------------------------------------------------------------------

/// Proves: a report is kept as it is written when it keeps every rule: the empty digest, a digest
/// of 64 lower-case hex characters, no refusal or one with a digest and a reason, and a failure.
///
/// Catches: a valid report changed or refused on its way in.
#[test]
fn a_report_that_keeps_the_rules_is_kept_as_it_is() {
    let reports = [
        agent::OperatorKeysReport::default(),
        holding(&"0123456789abcdef".repeat(4)),
        agent::OperatorKeysReport {
            failed: "Remote Login did not start".to_owned(),
            ..holding("")
        },
        agent::OperatorKeysReport {
            digest: "a".repeat(64),
            refused: Some(agent::OperatorKeysRefusal {
                digest: "b".repeat(64),
                reason: "no single local administrator account".to_owned(),
            }),
            failed: String::new(),
        },
    ];
    for report in reports {
        assert_eq!(kept(&report), Some(report.clone()), "{report:?}");
    }
}

// ---- a retired host -------------------------------------------------------------------------

/// Proves: the report of a host an operator retires is forgotten with the host, and so is that of
/// a retiring host whose final report frees its slot; the report of a host that stays is kept.
///
/// Catches: the reports of retired hosts kept for as long as the coordinator runs, and a
/// retirement that forgets another host's report.
#[test]
fn a_retired_host_has_no_report() {
    let f = Fixture::new("operator-keys-retire");
    for (n, host) in ["db-1", "db-2", "db-3"].into_iter().enumerate() {
        f.approve(&format!("m-{n}"), host, APPROVED, false);
        f.beat(host, "storage", FIRST, vec![runs(WEB, "1.0")]);
        f.rig
            .coordinator
            .hear_operator_keys(&who(host, "storage"), &announcing(None));
    }
    let keys = f.rig.coordinator.operator_keys();
    assert!(
        ["db-1", "db-2", "db-3"]
            .iter()
            .all(|h| keys.report_of(&id(h)).is_some())
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
    assert_eq!(keys.report_of(&id("db-1")), None);

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
    assert_eq!(keys.report_of(&id("db-2")), None);
    assert!(keys.report_of(&id("db-3")).is_some(), "a host that stays");
}
