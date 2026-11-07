//! The daemon's checks as functions: the version order, the tokens, the try rules on records the
//! runner would not write in these tests, and the reply for each way the runner stops.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_config::manifest::ManifestRefusal;
use hostkeeper_config::names::Version;
use hostkeeper_daemon::checks::{OrderKey, TryRule, compare_versions, not_below_running, try_rule};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::verbs::update::interrupted;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_steps::record::{HookSel, Progress, Record, State};
use hostkeeper_steps::{
    ArtifactError, BootId, Finished, LaunchError, RecordError, RunError, Stamp,
};

/// Catches versions compared as text, or parts of digits compared as text: each pair is in the
/// order given, and each version equals itself.
#[test]
fn versions_are_ordered_part_by_part_with_whole_numbers() {
    let ascending = [
        ("1.9.0", "1.10.0"),
        ("1.0", "1.0.0"),
        ("0.9", "1"),
        ("1.0.0", "1.0.0-rc1"),
        ("1.2.03", "1.2.4"),
        ("2", "10"),
        ("1.a", "1.b"),
        ("9", "a"),
        ("99999999999999999999998", "99999999999999999999999"),
    ];
    for (low, high) in ascending {
        assert_eq!(
            compare_versions(low, high),
            Ordering::Less,
            "{low} < {high}"
        );
        assert_eq!(
            compare_versions(high, low),
            Ordering::Greater,
            "{high} > {low}"
        );
        assert_eq!(compare_versions(low, low), Ordering::Equal, "{low}");
    }
    assert_eq!(compare_versions("1.02", "1.2"), Ordering::Equal);
}

/// Catches the downgrade rule applied to the wrong verb or to a service that runs nothing.
#[test]
fn only_an_update_below_the_running_version_is_refused() {
    let v = |text: &str| Version::new(text).expect("a version");
    let rows = [
        (
            HookedVerb::Update,
            "1.0.0",
            Some("2.0.0"),
            Err(Refusal::BelowRunning),
        ),
        (HookedVerb::Update, "2.0.0", Some("2.0.0"), Ok(())),
        (HookedVerb::Update, "3.0.0", Some("2.0.0"), Ok(())),
        (HookedVerb::Update, "1.0.0", None, Ok(())),
        (HookedVerb::ForceRollback, "1.0.0", Some("2.0.0"), Ok(())),
    ];
    for (verb, target, running, expected) in rows {
        let running = running.map(v);
        assert_eq!(
            not_below_running(verb, &v(target), running.as_ref()),
            expected,
            "{verb:?} {target} over {running:?}"
        );
    }
    assert_eq!(HookedVerb::ForceRollback.name(), "force_rollback");
    assert_eq!(HookedVerb::Install.name(), "install");
}

/// Catches a token outside the protobuf files' grammar, or two refusals sharing a token.
#[test]
fn every_refusal_token_keeps_the_token_grammar() {
    let mut seen = std::collections::BTreeSet::new();
    for refusal in Refusal::ALL {
        let token = refusal.token();
        let grammar = (1..=64).contains(&token.len())
            & token
                .bytes()
                .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'_'));
        assert!(grammar, "{refusal:?}: {token}");
        assert!(seen.insert(token), "{token} twice");
    }
    let too_old = ManifestRefusal::DaemonTooOld {
        needs: hostkeeper_config::manifest::HostkeeperVersion::parse("9.0.0").expect("v"),
        running: hostkeeper_config::manifest::HostkeeperVersion::parse("0.1.0").expect("v"),
    };
    assert_eq!(Refusal::from(&too_old), Refusal::DaemonTooOld);
    assert_eq!(
        Refusal::from(&ManifestRefusal::TooLarge),
        Refusal::ManifestMismatch
    );
}

/// Catches a digest of the wrong length or case accepted, and a good one shown otherwise.
#[test]
fn a_package_digest_is_64_lower_case_hex_digits() {
    let good = "0123456789abcdef".repeat(4);
    assert_eq!(PackageDigest::new(&good).expect("good").to_string(), good);
    let bad = [
        String::new(),
        good[1..].to_owned(),
        good.to_uppercase(),
        format!("{good}0"),
        "g".repeat(64),
    ];
    for bad in &bad {
        assert_eq!(PackageDigest::new(bad), None, "{bad}");
    }
}

fn key(version: &str, order_try: u64) -> OrderKey {
    OrderKey {
        verb: HookedVerb::Update,
        version: Version::new(version).expect("a version"),
        digest: PackageDigest::new(&"ab".repeat(32)).expect("a digest"),
        order_try,
    }
}

fn record(version: &str, order_try: Option<u64>, state: State) -> Record {
    Record {
        schema: 1,
        service: "web".to_owned(),
        target_version: version.to_owned(),
        artifact_digest: "cd".repeat(32),
        tarball_digest: "ab".repeat(32),
        previous_digest: None,
        generation: 1,
        params: PathBuf::from("/state/web/params.toml"),
        step: Step::Verify,
        attempt: 1,
        state,
        hook: HookSel::New,
        progress: Progress::default(),
        order_try,
        forced: false,
        force: None,
    }
}

fn finished(verdict: Verdict) -> State {
    State::Finished {
        verdict,
        reason: None,
    }
}

/// Catches a row of the try rules answered by its neighbour's rule, including records written
/// before the try was kept (try 0) and an unfinished order that is only a post-success watcher.
#[test]
fn the_try_rules_in_their_order() {
    let ok = Verdict::Succeeded {
        promotion_pending: false,
    };
    let done = || finished(ok.clone());
    let answered = TryRule::Answer(Finished {
        verdict: ok.clone(),
        reason: None,
    });
    let watching = Record {
        progress: Progress {
            watching: Some(Stamp {
                boot_id: BootId::new("a test", "b").ok(),
                mono_secs: 1,
            }),
            ..Progress::default()
        },
        ..record("2.0.0", Some(3), State::Pending)
    };
    let rows: Vec<(Option<Record>, OrderKey, TryRule)> = vec![
        (None, key("2.0.0", 1), TryRule::Run),
        (
            Some(record("2.0.0", Some(3), done())),
            key("2.0.0", 3),
            answered,
        ),
        (
            Some(record("2.0.0", Some(3), State::Pending)),
            key("2.0.0", 3),
            TryRule::Run,
        ),
        (
            Some(record("2.0.0", Some(3), done())),
            key("2.1.0", 3),
            TryRule::Refuse(Refusal::TryConflict),
        ),
        (
            Some(record("2.0.0", Some(3), done())),
            key("2.1.0", 2),
            TryRule::Refuse(Refusal::StaleTry),
        ),
        (
            Some(record("2.0.0", Some(3), done())),
            key("2.1.0", 4),
            TryRule::Run,
        ),
        (
            Some(record("2.0.0", Some(3), State::Pending)),
            key("2.1.0", 4),
            TryRule::Refuse(Refusal::Busy),
        ),
        (
            Some(record("2.0.0", Some(3), State::Pending)),
            key("2.0.0", 4),
            TryRule::Run,
        ),
        (Some(watching), key("2.1.0", 4), TryRule::Run),
        (
            Some(record("2.0.0", Some(3), finished(Verdict::Halt))),
            key("2.1.0", 9),
            TryRule::Refuse(Refusal::Halted),
        ),
        (
            Some(record("2.0.0", Some(3), finished(Verdict::Halt))),
            key("2.1.0", 3),
            TryRule::Refuse(Refusal::Halted),
        ),
        (
            Some(record("2.0.0", None, done())),
            key("2.1.0", 1),
            TryRule::Run,
        ),
        (
            Some(record("2.0.0", None, State::Pending)),
            key("2.1.0", 1),
            TryRule::Refuse(Refusal::Busy),
        ),
    ];
    for (n, (last, key, expected)) in rows.into_iter().enumerate() {
        assert_eq!(try_rule(last.as_ref(), &key), expected, "row {n}");
    }
}

/// Catches a way the runner stops reported with another token: each error of the runner is an
/// interrupted reply with its token and its text.
#[test]
fn each_way_the_runner_stops_has_its_token() {
    let launch = || LaunchError::Launcher("gone".to_owned());
    let rows = [
        (
            RunError::Unstopped {
                unit: "u".to_owned(),
                error: Arc::new(launch()),
            },
            Refusal::Unstopped,
        ),
        (RunError::Launch(launch()), Refusal::LauncherFailed),
        (
            RunError::Record(RecordError::Corrupt("x".to_owned())),
            Refusal::StoreFailed,
        ),
        (
            RunError::Unreadable(ArtifactError::Io(std::io::Error::other("eio"))),
            Refusal::ReleaseUnreadable,
        ),
        (
            RunError::Busy {
                service: "web".to_owned(),
                target: "2.0.0".to_owned(),
            },
            Refusal::Busy,
        ),
        (RunError::RecordNewer(2), Refusal::RecordNewer),
        (
            RunError::Switch("cannot link /x".to_owned()),
            Refusal::LinkFailed,
        ),
        (
            RunError::BadOrder("the service name"),
            Refusal::InvalidOrder,
        ),
        (
            RunError::UnitNameTooLong {
                service: "a".repeat(210),
            },
            Refusal::InvalidOrder,
        ),
    ];
    for (error, refusal) in rows {
        let detail = error.to_string();
        assert_eq!(interrupted(error), Reply::Interrupted { refusal, detail });
    }
}

/// Catches the coordinator's list of refusals for the try alone, which it orders again past and
/// never counts as the host failing the version (`hostkeeper_wire::wire::TRY_REFUSALS`), drifting
/// from the tokens of the two try rules that refuse an order for its try: it is exactly
/// `try_conflict` and `stale_try`, and no other refusal's token.
#[test]
fn the_try_refusals_the_coordinator_passes_over_are_the_try_rules_tokens() {
    let try_rules = [Refusal::TryConflict.token(), Refusal::StaleTry.token()];
    assert_eq!(hostkeeper_wire::wire::TRY_REFUSALS, try_rules);
    let listed = Refusal::ALL
        .iter()
        .filter(|refusal| hostkeeper_wire::wire::TRY_REFUSALS.contains(&refusal.token()))
        .count();
    assert_eq!(listed, 2);
}
