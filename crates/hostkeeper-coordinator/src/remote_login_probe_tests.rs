//! The rules a Remote Login probe keeps, and what the coordinator keeps, shows and forgets of it.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;

use super::{Probes, keeps_rules, show};
use crate::fleet::Fleet;
use crate::testutil::{id, secs};

/// A step named `name` that runs nothing and says nothing.
fn step(name: &str) -> agent::ProbeStep {
    agent::ProbeStep {
        name: name.to_owned(),
        ..agent::ProbeStep::default()
    }
}

/// A probe of the one step `step`.
fn probe_of(step: agent::ProbeStep) -> agent::RemoteLoginProbe {
    agent::RemoteLoginProbe {
        started_unix: 1_700_000_000,
        steps: vec![step],
    }
}

/// A probe of one step of every kind of text, to make a change to one of them stand out.
fn sample() -> agent::RemoteLoginProbe {
    probe_of(agent::ProbeStep {
        name: "remote_login_set_on".to_owned(),
        argv: vec!["systemsetup".to_owned(), "-getremotelogin".to_owned()],
        exit_code: Some(0),
        stdout: "Remote Login: On".to_owned(),
        stderr: "warning".to_owned(),
        error: String::new(),
        truncated: false,
    })
}

/// Proves: a probe of no step keeps the rules, as does one of exactly 24 steps; one of 25 does
/// not.
///
/// Catches: a step count off by one at the boundary, and an empty probe refused.
#[test]
fn a_probe_of_at_most_24_steps_keeps_the_rules() {
    let steps = |n: usize| agent::RemoteLoginProbe {
        started_unix: 0,
        steps: (0..n).map(|_| step("a")).collect(),
    };
    assert!(keeps_rules(&steps(0)));
    assert!(keeps_rules(&steps(24)));
    assert!(!keeps_rules(&steps(25)));
}

/// Proves: a step named by 1 to 64 bytes of lower-case letters, digits and `_` keeps the rules;
/// an empty name, one of 65 bytes, and names with an upper-case letter, `-`, a space or a
/// non-ASCII character do not.
///
/// Catches: a name length bound off by one at either end, and a character outside `[a-z0-9_]`
/// let through.
#[test]
fn a_step_name_is_1_to_64_bytes_of_lower_case_digits_and_underscore() {
    let ok = |name: &str| keeps_rules(&probe_of(step(name)));
    assert!(ok("a"));
    assert!(ok(&"a".repeat(64)));
    assert!(ok("remote_login_set_on_2"));
    assert!(ok("0_9"));
    assert!(!ok(""));
    assert!(!ok(&"a".repeat(65)));
    assert!(!ok("Remote"));
    assert!(!ok("remote-login"));
    assert!(!ok("remote login"));
    assert!(!ok("rémote"));
    // Two bytes long, not two characters: 33 of it is 66 bytes.
    assert!(!ok(&"é".repeat(33)));
}

/// Proves: a name with a bad character late in a long name is refused, so every byte of the
/// name is checked, not its first.
///
/// Catches: a name checked at its start alone.
#[test]
fn a_bad_character_at_the_end_of_a_name_breaks_the_rules() {
    assert!(!keeps_rules(&probe_of(step("remote_login_set_On"))));
}

/// Proves: a bad step among good ones breaks the rules of the whole probe, wherever it is.
///
/// Catches: only the first step, or only the last, checked.
#[test]
fn one_bad_step_among_good_ones_breaks_the_rules_of_the_probe() {
    for at in [0, 1, 2] {
        let mut probe = agent::RemoteLoginProbe {
            started_unix: 0,
            steps: vec![step("a"), step("b"), step("c")],
        };
        assert!(keeps_rules(&probe));
        probe.steps[at].name = "BAD".to_owned();
        assert!(!keeps_rules(&probe), "bad step {at}");
    }
}

/// Proves: a step of 8 arguments keeps the rules and one of 9 does not.
///
/// Catches: an argument count bound off by one.
#[test]
fn a_step_has_at_most_8_arguments() {
    let argv = |n: usize| {
        probe_of(agent::ProbeStep {
            argv: vec!["x".to_owned(); n],
            ..step("a")
        })
    };
    assert!(keeps_rules(&argv(0)));
    assert!(keeps_rules(&argv(8)));
    assert!(!keeps_rules(&argv(9)));
}

/// Proves: an argument of exactly 1024 bytes keeps the rules and one of 1025 does not, whichever
/// argument of the step it is.
///
/// Catches: argument text unbounded, a bound off by one, and only the first argument checked.
#[test]
fn an_argument_is_at_most_1024_bytes() {
    let with = |len: usize| {
        probe_of(agent::ProbeStep {
            argv: vec!["x".to_owned(), "y".repeat(len)],
            ..step("a")
        })
    };
    assert!(keeps_rules(&with(1024)));
    assert!(!keeps_rules(&with(1025)));
}

/// Proves: standard output of exactly 1024 bytes keeps the rules and one of 1025 does not.
///
/// Catches: standard output unbounded, or a bound off by one.
#[test]
fn stdout_is_at_most_1024_bytes() {
    let with = |len: usize| {
        probe_of(agent::ProbeStep {
            stdout: "o".repeat(len),
            ..step("a")
        })
    };
    assert!(keeps_rules(&with(1024)));
    assert!(!keeps_rules(&with(1025)));
}

/// Proves: standard error of exactly 1024 bytes keeps the rules and one of 1025 does not.
///
/// Catches: standard error unbounded, or a bound off by one.
#[test]
fn stderr_is_at_most_1024_bytes() {
    let with = |len: usize| {
        probe_of(agent::ProbeStep {
            stderr: "e".repeat(len),
            ..step("a")
        })
    };
    assert!(keeps_rules(&with(1024)));
    assert!(!keeps_rules(&with(1025)));
}

/// Proves: an error text of exactly 1024 bytes keeps the rules and one of 1025 does not.
///
/// Catches: the error text unbounded, or a bound off by one.
#[test]
fn an_error_text_is_at_most_1024_bytes() {
    let with = |len: usize| {
        probe_of(agent::ProbeStep {
            error: "r".repeat(len),
            ..step("a")
        })
    };
    assert!(keeps_rules(&with(1024)));
    assert!(!keeps_rules(&with(1025)));
}

/// Proves: the bounds on texts are in bytes: 513 two-byte characters (1026 bytes) break the
/// rules, 512 (1024 bytes) keep them.
///
/// Catches: text counted in characters.
#[test]
fn the_text_bound_counts_bytes_not_characters() {
    let with = |chars: usize| {
        probe_of(agent::ProbeStep {
            stdout: "é".repeat(chars),
            ..step("a")
        })
    };
    assert!(keeps_rules(&with(512)));
    assert!(!keeps_rules(&with(513)));
}

/// Proves: `show` puts the host's kept probe, as it was kept, into the host's heartbeat, making
/// the heartbeat when the host has none; and leaves a host that has no probe as it is, with no
/// heartbeat made.
///
/// Catches: a probe shown on another host, a heartbeat of the host overwritten by `show`, and a
/// heartbeat made for a host that has no probe.
#[test]
fn show_fills_in_the_probe_of_the_host_and_of_no_other() {
    let mut fleet = Fleet::new(secs(0), secs(0));
    fleet.remote_login_probes.0.insert(id("s1"), sample());

    let mut bare = operator::Host::default();
    show(&fleet, &id("s1"), &mut bare);
    assert_eq!(
        bare.heartbeat,
        Some(agent::HeartbeatRequest {
            remote_login_probe: Some(sample()),
            ..agent::HeartbeatRequest::default()
        })
    );

    let mut beating = operator::Host {
        heartbeat: Some(agent::HeartbeatRequest {
            installers: vec!["pkg".to_owned()],
            ..agent::HeartbeatRequest::default()
        }),
        ..operator::Host::default()
    };
    show(&fleet, &id("s1"), &mut beating);
    assert_eq!(
        beating.heartbeat,
        Some(agent::HeartbeatRequest {
            installers: vec!["pkg".to_owned()],
            remote_login_probe: Some(sample()),
            ..agent::HeartbeatRequest::default()
        })
    );

    let mut other = operator::Host::default();
    show(&fleet, &id("s2"), &mut other);
    assert_eq!(other, operator::Host::default());
}

/// Proves: `Probes::forget` drops the probe of the host named and no other host's.
///
/// Catches: a forgotten host's probe still shown, and another host's forgotten with it.
#[test]
fn forget_drops_the_probe_of_that_host_alone() {
    let mut fleet = Fleet::new(secs(0), secs(0));
    for name in ["s1", "s2"] {
        fleet.remote_login_probes.0.insert(id(name), sample());
    }
    let mut probes: Probes = fleet.remote_login_probes.clone();
    probes.forget(&id("s1"));
    probes.forget(&id("s3"));
    assert_eq!(probes.0.keys().cloned().collect::<Vec<_>>(), [id("s2")]);
}

/// Proves: a host the fleet releases (`Fleet::release_if_final`) has its probe forgotten, and
/// another host's stays.
///
/// Catches: a retired host's probe held in memory for good, and another host's forgotten with it.
#[test]
fn a_released_host_has_its_probe_forgotten() {
    let mut fleet = Fleet::new(secs(0), secs(0));
    for name in ["s1", "s2"] {
        fleet.remote_login_probes.0.insert(id(name), sample());
    }
    assert!(fleet.release_if_final(&id("s1"), secs(10)));
    let mut gone = operator::Host::default();
    show(&fleet, &id("s1"), &mut gone);
    assert_eq!(gone.heartbeat, None);
    let mut kept = operator::Host::default();
    show(&fleet, &id("s2"), &mut kept);
    assert_eq!(
        kept.heartbeat.and_then(|beat| beat.remote_login_probe),
        Some(sample())
    );
}
