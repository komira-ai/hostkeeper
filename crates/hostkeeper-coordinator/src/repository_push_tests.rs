//! The repositories sent to a host ([`crate::repository_push`]): what a daemon's report is
//! judged by, the set the registry makes for a host and the rules that decide whether it is sent,
//! and what `Hosts.Get` shows. The answer over gRPC is in `grpc/tests/repositories.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_release::registry;
use serde_json::{Value, json};

use crate::marks_support::{DB, WEB, ask, beat_of, operator, rig, runs, who};
use crate::repositories_support::{komira, repository};
use crate::repository_push::{FEATURE, MAX_REASON, Refused, Report};
use crate::testutil::{OPERATOR, Rig, id};

/// A report of the set of `generation` and `digest`, refusing none.
fn report(generation: u64, digest: &str) -> agent::RepositoriesReport {
    agent::RepositoriesReport {
        generation,
        digest: digest.to_owned(),
        refused: None,
    }
}

fn refusal(digest: &str, reason: &str) -> agent::RepositoryRefusal {
    agent::RepositoryRefusal {
        generation: 1,
        digest: digest.to_owned(),
        reason: reason.to_owned(),
    }
}

/// A heartbeat over gRPC of a daemon that announces [`FEATURE`] and says `repositories`.
fn said(repositories: Option<agent::RepositoriesReport>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: vec![FEATURE.to_owned()],
        repositories,
        ..agent::HeartbeatRequest::default()
    }
}

/// The digest of the set of `komira`s of `seeds`.
fn digest_of(repositories: &[hostkeeper_proto::hostkeeper::release::v1::Repository]) -> String {
    registry::digest(repositories)
}

fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

/// Puts `repository` as the operator.
fn put(rig: &Rig, repository: hostkeeper_proto::hostkeeper::release::v1::Repository) {
    rig.coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(repository),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");
}

/// The host `d1` of the role `daemon` heartbeats: it announces `repositories-v1` among the
/// features of its JSON heartbeat, says `repositories` over gRPC when `says`, and runs `web` and
/// `db`.
fn beats(rig: &Rig, says: Option<Option<agent::RepositoriesReport>>) {
    if let Some(repositories) = says {
        rig.coordinator
            .hear_repositories(&who("d1", "daemon"), &said(repositories));
    }
    let b = beat_of(
        &["heartbeat-v2", FEATURE],
        vec![runs(WEB, "1.0"), runs(DB, "1.0")],
    );
    ask(rig, "d1", "daemon", b);
}

fn set_for_d1(rig: &Rig) -> Option<agent::RepositorySet> {
    rig.coordinator.repository_set(&id("d1"))
}

/// Proves: a daemon's report is read when its digest is empty with generation 0 or 64 lower-case
/// hex with a generation, its refusal when it has a 64-hex digest and a reason of 1 to 256 bytes,
/// a longer reason cut at a character boundary (a two-byte character on the limit is dropped whole)
/// and no more; each other report is none: a digest with generation 0, a generation without a
/// digest, an upper-case or short digest, a refusal with no reason or a bad digest.
///
/// Catches: a report that breaks a rule kept, or a good one dropped, or a reason that is cut in
/// the middle of a character (which would not be text).
#[test]
fn a_report_is_read_within_its_rules_and_its_reason_is_cut_at_a_boundary() {
    assert_eq!(Report::of(&report(0, "")), Some(Report::default()));
    let good = Report::of(&report(4, &hex('a'))).expect("read");
    assert_eq!(
        (good.generation, good.digest.as_str()),
        (4, hex('a').as_str())
    );
    for bad in [
        report(0, &hex('a')),
        report(3, ""),
        report(1, &"A".repeat(64)),
        report(1, &"a".repeat(63)),
        report(1, &"a".repeat(65)),
    ] {
        assert_eq!(Report::of(&bad), None, "{bad:?}");
    }

    let with = |refused: agent::RepositoryRefusal| agent::RepositoriesReport {
        refused: Some(refused),
        ..report(0, "")
    };
    let kept = Report::of(&with(refusal(&hex('b'), "the key is absent"))).expect("read");
    assert_eq!(
        kept.refused,
        Some(Refused {
            digest: hex('b'),
            reason: "the key is absent".to_owned()
        })
    );
    for bad in [
        refusal(&hex('b'), ""),
        refusal("abc", "why"),
        refusal(&"B".repeat(64), "why"),
    ] {
        assert_eq!(Report::of(&with(bad)), None);
    }
    let exact = "x".repeat(MAX_REASON);
    let reason = |text: &str| {
        Report::of(&with(refusal(&hex('b'), text)))
            .and_then(|r| r.refused)
            .map(|r| r.reason)
    };
    assert_eq!(reason(&exact), Some(exact.clone()));
    assert_eq!(reason(&format!("{exact}y")), Some(exact));
    // 255 bytes, then a two-byte character that crosses the limit.
    let straddling = format!("{}é", "x".repeat(MAX_REASON - 1));
    assert_eq!(reason(&straddling), Some("x".repeat(MAX_REASON - 1)));
    let fits = format!("{}é", "x".repeat(MAX_REASON - 2));
    assert_eq!(reason(&fits), Some(fits.clone()));
}

/// Proves: a host that announces `repositories-v1` whose services name `komira` is sent the set
/// of the registry's `komira`: the registry's generation, the digest of the set by the rule the
/// daemon checks it by, and the entry with its keyring; one that sent no report yet is sent it
/// too.
///
/// Catches: a set that is not sent, one whose digest the daemon would recompute differently, or one
/// that leaves a keyring out.
#[test]
fn a_host_that_announces_the_feature_is_sent_the_set_its_services_name() {
    let rig = rig("push-sent", "");
    put(&rig, komira(1));
    beats(&rig, Some(None));
    let set = set_for_d1(&rig).expect("a set");
    assert_eq!(set.generation, 1);
    assert_eq!(set.entries, [komira(1)]);
    assert_eq!(set.digest, digest_of(&[komira(1)]));
    assert_eq!(registry::check_set(&set.entries), Ok(()));
}

/// Proves: a host is sent no set when it did not announce the feature (its report is not read
/// either), when the registry is empty, when no service of the host's names a repository, and for
/// a host never heard.
///
/// Catches: a push to a daemon that cannot write it, or one the coordinator did not offer.
#[test]
fn no_set_goes_to_a_host_that_did_not_announce_the_feature_or_to_one_offered_none() {
    let rig = rig("push-feature", "");
    put(&rig, komira(1));
    assert_eq!(set_for_d1(&rig), None, "never heard");
    let plain = beat_of(&["heartbeat-v2"], vec![runs(WEB, "1.0"), runs(DB, "1.0")]);
    rig.coordinator
        .hear_repositories(&who("d1", "daemon"), &agent::HeartbeatRequest::default());
    ask(&rig, "d1", "daemon", plain);
    assert_eq!(set_for_d1(&rig), None, "did not announce it");

    beats(&rig, None);
    assert_eq!(
        set_for_d1(&rig),
        None,
        "announced over JSON, never reported"
    );

    let empty = crate::marks_support::rig("push-empty", "");
    beats(&empty, Some(None));
    assert_eq!(set_for_d1(&empty), None, "nothing registered");

    // A host of a role no service runs on, announcing no service, needs no repository.
    let idle = crate::marks_support::rig("push-idle", "");
    put(&idle, komira(1));
    idle.coordinator
        .hear_repositories(&who("i1", "frontend"), &said(None));
    ask(
        &idle,
        "i1",
        "frontend",
        beat_of(&["heartbeat-v2", FEATURE], Vec::new()),
    );
    assert_eq!(
        idle.coordinator.repository_set(&id("i1")),
        None,
        "needs nothing"
    );
}

/// Proves: a host whose service names a repository the registry lacks is sent none, though
/// another it names is registered; once the missing one is put the set holds both.
///
/// Catches: a partial set, which the daemon would take as the whole configuration.
#[test]
fn a_host_is_sent_nothing_while_a_repository_it_needs_is_unregistered() {
    let rig = rig("push-unregistered", "repository = \"other\"");
    put(&rig, komira(1));
    beats(&rig, Some(None));
    assert_eq!(set_for_d1(&rig), None);
    put(
        &rig,
        repository("other", "https://o.example.com/", &["stable"], 2),
    );
    let set = set_for_d1(&rig).expect("a set");
    assert_eq!(
        set.entries
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        ["komira", "other"]
    );
}

/// Proves: the set is not sent again when the host's report holds its digest in force or refused,
/// and is sent when the report holds another digest in force or refused (a set of an earlier
/// registry), when the host reports the digest of the set again after a key rotation changed it,
/// and when the same digest is put again at a higher generation (it stays unsent).
///
/// Catches: a set resent at every heartbeat, a refused set resent forever, a rotation that is
/// never sent.
#[test]
fn the_set_is_sent_until_the_report_holds_its_digest_in_force_or_refused() {
    let rig = rig("push-report", "");
    put(&rig, komira(1));
    let first = digest_of(&[komira(1)]);
    beats(&rig, Some(Some(report(0, ""))));
    assert!(set_for_d1(&rig).is_some());

    beats(&rig, Some(Some(report(1, &first))));
    assert_eq!(set_for_d1(&rig), None, "in force");
    let refused = agent::RepositoriesReport {
        refused: Some(refusal(&first, "no")),
        ..report(0, "")
    };
    beats(&rig, Some(Some(refused)));
    assert_eq!(set_for_d1(&rig), None, "refused");
    let other_refused = agent::RepositoriesReport {
        refused: Some(refusal(&hex('c'), "no")),
        ..report(0, "")
    };
    beats(&rig, Some(Some(other_refused)));
    assert!(set_for_d1(&rig).is_some(), "another set was refused");

    beats(&rig, Some(Some(report(1, &first))));
    put(&rig, komira(2));
    let rotated = set_for_d1(&rig).expect("the rotation is sent");
    assert_eq!(rotated.generation, 2);
    assert_eq!(rotated.digest, digest_of(&[komira(2)]));
    assert_ne!(rotated.digest, first);

    beats(&rig, Some(Some(report(2, &rotated.digest))));
    put(&rig, komira(2));
    assert_eq!(
        set_for_d1(&rig),
        None,
        "the same set at a higher generation"
    );
}

/// Proves: a report that breaks a rule is dropped, counted as the dropped fact `repositories`,
/// and the one read before is kept, so a set already in force is not sent again.
///
/// Catches: a bad report that resets what the coordinator knows of the host.
#[test]
fn a_bad_report_is_dropped_and_the_one_before_is_kept() {
    let rig = rig("push-bad-report", "");
    put(&rig, komira(1));
    let first = digest_of(&[komira(1)]);
    beats(&rig, Some(Some(report(1, &first))));
    assert_eq!(set_for_d1(&rig), None);
    beats(&rig, Some(Some(report(0, &first))));
    assert_eq!(set_for_d1(&rig), None, "the good report stands");
    let counts: Value =
        serde_json::to_value(rig.coordinator.status(Some(OPERATOR)).expect("status"))
            .expect("JSON")["extras_dropped"]
            .clone();
    assert_eq!(counts, json!({"repositories": 1}));
}

/// Proves: the services that need a repository are those assigned to the host and not excluded
/// from it, and those it announced: with `web` excluded and no longer run by the host it needs
/// `db`'s repository alone, while an excluded service it still runs stays needed;
/// a service of another role that the host runs is needed too; the same repository named by two
/// services is one entry.
///
/// Catches: a set that names a repository no service of the host uses, or leaves one it uses out.
#[test]
fn a_host_needs_the_repositories_of_its_assigned_and_announced_services() {
    let rig = rig(
        "push-needed",
        "repository = \"web-repo\"\n\n[[service]]\nname = \"solo\"\nroles = [\"frontend\"]\nrepository = \"solo-repo\"",
    );
    put(&rig, komira(1));
    put(
        &rig,
        repository("web-repo", "https://w.example.com/", &["stable"], 2),
    );
    put(
        &rig,
        repository("solo-repo", "https://s.example.com/", &["stable"], 3),
    );
    beats(&rig, Some(None));
    let names = |set: Option<agent::RepositorySet>| -> Vec<String> {
        set.expect("a set")
            .entries
            .into_iter()
            .map(|r| r.name)
            .collect()
    };
    assert_eq!(names(set_for_d1(&rig)), ["komira", "web-repo"]);

    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: WEB.to_owned(),
                node_id: "d1".to_owned(),
                reason: "gone".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");
    assert_eq!(
        names(set_for_d1(&rig)),
        ["komira", "web-repo"],
        "still run by the host, so still needed"
    );
    let without_web = beat_of(&["heartbeat-v2", FEATURE], vec![runs(DB, "1.0")]);
    ask(&rig, "d1", "daemon", without_web);
    assert_eq!(names(set_for_d1(&rig)), ["komira"]);

    // `solo` runs on frontends: a daemon that announces it is sent its repository.
    let b = beat_of(
        &["heartbeat-v2", FEATURE],
        vec![runs(DB, "1.0"), runs("solo", "1.0")],
    );
    ask(&rig, "d1", "daemon", b);
    assert_eq!(names(set_for_d1(&rig)), ["komira", "solo-repo"]);
}

/// Proves: a host needing 8 repositories is sent them, and one needing 9 is sent none: the set is
/// at most what a daemon takes.
///
/// Catches: a set the daemon must refuse for its size, sent at every heartbeat.
#[test]
fn a_set_of_eight_repositories_is_sent_and_a_ninth_stops_it() {
    let services = |n: usize| {
        (0..n)
            .map(|i| {
                format!(
                    "\n\n[[service]]\nname = \"s{i}\"\nroles = [\"daemon\"]\nrepository = \"r{i}\""
                )
            })
            .collect::<String>()
    };
    for (extra, sent) in [(7, true), (8, false)] {
        let rig = rig(&format!("push-eight-{extra}"), &services(extra));
        put(&rig, komira(1));
        for i in 0..extra {
            put(
                &rig,
                repository(&format!("r{i}"), "https://r.example.com/", &["stable"], 5),
            );
        }
        beats(&rig, Some(None));
        assert_eq!(
            set_for_d1(&rig).map(|set| set.entries.len()),
            sent.then_some(8)
        );
    }
}

/// Proves: `Hosts.Get` shows a host that announced the feature the repositories it names, those
/// the registry lacks, the digest of the set it is sent, the generation the daemon reported, whether
/// the set is in force, and the reason of a refusal of that set; a host that did not announce it
/// shows none.
///
/// Catches: a host page that does not say why a host has no repositories.
#[test]
fn hosts_show_what_the_coordinator_sends_and_what_the_daemon_reported() {
    let rig = rig("push-shown", "repository = \"other\"");
    put(&rig, komira(1));
    let get = |rig: &Rig, node: &str| {
        rig.coordinator
            .get_host(
                &operator(),
                &proto::GetHostRequest {
                    node_id: node.to_owned(),
                },
            )
            .expect("the host")
            .host
            .expect("a host")
            .repositories
    };
    beats(&rig, Some(None));
    assert_eq!(
        get(&rig, "d1"),
        Some(proto::HostRepositories {
            named: vec!["komira".to_owned(), "other".to_owned()],
            unregistered: vec!["other".to_owned()],
            ..proto::HostRepositories::default()
        })
    );
    put(
        &rig,
        repository("other", "https://o.example.com/", &["stable"], 2),
    );
    let both = digest_of(&[
        komira(1),
        repository("other", "https://o.example.com/", &["stable"], 2),
    ]);
    assert_eq!(
        get(&rig, "d1"),
        Some(proto::HostRepositories {
            named: vec!["komira".to_owned(), "other".to_owned()],
            digest: both.clone(),
            ..proto::HostRepositories::default()
        })
    );
    beats(&rig, Some(Some(report(2, &both))));
    let current = get(&rig, "d1").expect("shown");
    assert_eq!(
        (
            current.current,
            current.generation,
            current.refused.as_str()
        ),
        (true, 2, "")
    );

    let refused = agent::RepositoriesReport {
        refused: Some(refusal(&both, "the key is absent")),
        ..report(1, &hex('d'))
    };
    beats(&rig, Some(Some(refused)));
    let shown = get(&rig, "d1").expect("shown");
    assert_eq!(
        (shown.current, shown.generation, shown.refused.as_str()),
        (false, 1, "the key is absent")
    );

    let plain = beat_of(&["heartbeat-v2"], vec![runs(WEB, "1.0")]);
    ask(&rig, "d2", "daemon", plain.clone());
    assert_eq!(get(&rig, "d2"), None);
    // A host that stops announcing the feature is shown none, whatever it reported before.
    ask(&rig, "d1", "daemon", plain);
    assert_eq!(get(&rig, "d1"), None);
}

/// Catches: `repositories-v1` advertised and not acted on: a host that announces it is sent the set
/// its services name, and the same host that does not announce it is sent none.
pub(crate) async fn repositories_v1_is_acted_on() {
    let rig = rig("push-feature-acted-on", "");
    put(&rig, komira(1));
    beats(&rig, Some(None));
    assert!(set_for_d1(&rig).is_some());
    let silent = beat_of(&["heartbeat-v2"], vec![runs(WEB, "1.0"), runs(DB, "1.0")]);
    ask(&rig, "d1", "daemon", silent);
    assert_eq!(set_for_d1(&rig), None);
}
