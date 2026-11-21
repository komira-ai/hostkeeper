//! [`wait_for`] against a scripted [`Net`]: each probe's answer, the rejoin's outcome and every
//! wait are the script's, so no test waits, joins a network or reads a clock.

use std::collections::VecDeque;
use std::time::Duration;

use super::{Net, Plan, last_line, number, wait_for};

/// One thing the wait asked of the network, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Probe,
    Wait(Duration),
    Rejoin,
}

/// A network whose probes answer from `answers` in order (no answer, `no route to host`, once
/// they run out), and whose rejoin answers `rejoin`.
struct Script {
    answers: VecDeque<Result<u16, String>>,
    rejoin: Result<(), String>,
    events: Vec<Event>,
}

impl Script {
    fn new(answers: Vec<Result<u16, String>>) -> Script {
        Script {
            answers: answers.into(),
            rejoin: Ok(()),
            events: Vec::new(),
        }
    }
}

impl Net for Script {
    fn probe(&mut self) -> Result<u16, String> {
        self.events.push(Event::Probe);
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err("no route to host".to_owned()))
    }

    fn wait(&mut self, time: Duration) {
        self.events.push(Event::Wait(time));
    }

    fn rejoin(&mut self) -> Result<(), String> {
        self.events.push(Event::Rejoin);
        self.rejoin.clone()
    }
}

/// No answer, for `why`.
fn silent(why: &str) -> Result<u16, String> {
    Err(why.to_owned())
}

/// Six attempts ten seconds apart, the defaults.
const PLAN: Plan = Plan {
    attempts: 6,
    interval: Duration::from_secs(10),
};

const TEN: Event = Event::Wait(Duration::from_secs(10));

/// The wait of `plan` against `net`: its result and what it wrote to stderr.
fn run(net: &mut Script, plan: Plan) -> (Result<String, String>, String) {
    let mut stderr = Vec::new();
    let got = wait_for(net, plan, &mut stderr);
    (got, String::from_utf8(stderr).expect("text"))
}

/// Proves: the coordinator answering on the third attempt ends the wait green, with one line that
/// names that attempt of round 1, after two lines that say why the first two had no answer and
/// two waits of the interval; it does not rejoin.
#[test]
fn an_answer_on_the_third_attempt_is_green_and_says_so() {
    let mut net = Script::new(vec![
        silent("connection refused"),
        silent("timed out"),
        Ok(200),
    ]);
    let (got, stderr) = run(&mut net, PLAN);
    assert_eq!(
        got.as_deref(),
        Ok("the coordinator answered (HTTP 200) on attempt 3 of 6 of round 1, after the join")
    );
    assert_eq!(
        stderr,
        "hk: tailnet-wait: round 1, attempt 1 of 6: no answer: connection refused\n\
         hk: tailnet-wait: round 1, attempt 2 of 6: no answer: timed out\n"
    );
    assert_eq!(
        net.events,
        [Event::Probe, TEN, Event::Probe, TEN, Event::Probe]
    );
}

/// Proves: a round 1 with no answer rejoins once, and an answer in round 2 ends the wait green,
/// with one line that names the attempt of round 2 and says the rejoin came first.
#[test]
fn no_answer_then_a_rejoin_then_an_answer_is_green_and_says_so() {
    let mut answers = vec![silent("timed out"); 6];
    answers.extend([silent("connection refused"), Ok(200)]);
    let mut net = Script::new(answers);
    let (got, stderr) = run(&mut net, PLAN);
    assert_eq!(
        got.as_deref(),
        Ok(
            "the coordinator answered (HTTP 200) on attempt 2 of 6 of round 2, after the rejoin (round 1 had no answer in 6 attempts)"
        )
    );
    let mut want: String = (1..=6)
        .map(|n| format!("hk: tailnet-wait: round 1, attempt {n} of 6: no answer: timed out\n"))
        .collect();
    want.push_str("hk: tailnet-wait: no answer in the 6 attempts of round 1; rejoining the tailnet (tailscale down, then tailscale up)\n");
    want.push_str("hk: tailnet-wait: round 2, attempt 1 of 6: no answer: connection refused\n");
    assert_eq!(stderr, want);
    let mut events = vec![Event::Probe];
    for _ in 1..6 {
        events.extend([TEN, Event::Probe]);
    }
    events.extend([Event::Rejoin, Event::Probe, TEN, Event::Probe]);
    assert_eq!(net.events, events);
}

/// Proves: no answer in either round is red, with one failure that names both rounds and the last
/// reason of each; the rejoin ran exactly once, between them, and each round made all its
/// attempts.
#[test]
fn no_answer_in_either_round_is_red_and_names_both_rounds() {
    // Round 2's answers are the script's default: no route to host.
    let mut net = Script::new(vec![silent("timed out"); 6]);
    let (got, stderr) = run(&mut net, PLAN);
    assert_eq!(
        got,
        Err(
            "the coordinator did not answer: round 1, after the join: no answer in 6 attempts, the last: timed out; round 2, after the rejoin: no answer in 6 attempts, the last: no route to host"
                .to_owned()
        )
    );
    assert_eq!(
        net.events.iter().filter(|e| **e == Event::Rejoin).count(),
        1
    );
    assert_eq!(
        net.events.iter().filter(|e| **e == Event::Probe).count(),
        12
    );
    assert_eq!(
        net.events.iter().position(|e| *e == Event::Rejoin),
        Some(11)
    );
    assert_eq!(stderr.lines().count(), 13);
    assert!(
        stderr
            .ends_with("hk: tailnet-wait: round 2, attempt 6 of 6: no answer: no route to host\n")
    );
}

/// Proves: a rejoin that fails is red at once, naming round 1 and how the rejoin failed, and
/// round 2 does not run.
#[test]
fn a_rejoin_that_fails_is_red_and_round_two_does_not_run() {
    let mut net = Script::new(vec![silent("timed out"); 6]);
    net.rejoin = Err("tailscale up ended with exit status: 1: access denied".to_owned());
    let (got, _) = run(&mut net, PLAN);
    assert_eq!(
        got,
        Err(
            "the coordinator did not answer: round 1, after the join: no answer in 6 attempts, the last: timed out; the rejoin failed: tailscale up ended with exit status: 1: access denied; round 2 did not run"
                .to_owned()
        )
    );
    assert_eq!(net.events.last(), Some(&Event::Rejoin));
    assert_eq!(net.events.iter().filter(|e| **e == Event::Probe).count(), 6);
}

/// Proves: an answer on the first attempt waits for nothing and says nothing on stderr; any HTTP
/// status is an answer (a coordinator from before the route answers 404, one that is starting
/// 503).
#[test]
fn any_status_on_the_first_attempt_ends_the_wait_at_once() {
    for status in [200, 404, 503] {
        let mut net = Script::new(vec![Ok(status)]);
        let (got, stderr) = run(&mut net, PLAN);
        assert_eq!(
            got,
            Ok(format!(
                "the coordinator answered (HTTP {status}) on attempt 1 of 6 of round 1, after the join"
            ))
        );
        assert_eq!(stderr, "");
        assert_eq!(net.events, [Event::Probe]);
    }
}

/// Proves: one attempt a round waits for nothing, rejoins after it and probes once more; an
/// answer on the last attempt of round 2 is green; the interval is the plan's, zero included.
#[test]
fn the_bounds_of_a_round_are_the_plans() {
    let one = Plan {
        attempts: 1,
        interval: Duration::from_secs(10),
    };
    let mut net = Script::new(vec![silent("timed out"), Ok(200)]);
    let (got, _) = run(&mut net, one);
    assert_eq!(
        got.as_deref(),
        Ok(
            "the coordinator answered (HTTP 200) on attempt 1 of 1 of round 2, after the rejoin (round 1 had no answer in 1 attempts)"
        )
    );
    assert_eq!(net.events, [Event::Probe, Event::Rejoin, Event::Probe]);

    let two = Plan {
        attempts: 2,
        interval: Duration::ZERO,
    };
    let mut net = Script::new(vec![silent("a"), silent("b"), silent("c"), Ok(204)]);
    let (got, _) = run(&mut net, two);
    assert_eq!(
        got.as_deref(),
        Ok(
            "the coordinator answered (HTTP 204) on attempt 2 of 2 of round 2, after the rejoin (round 1 had no answer in 2 attempts)"
        )
    );
    let zero = Event::Wait(Duration::ZERO);
    assert_eq!(
        net.events,
        [
            Event::Probe,
            zero.clone(),
            Event::Probe,
            Event::Rejoin,
            Event::Probe,
            zero,
            Event::Probe
        ]
    );
}

/// Proves: a stderr that cannot be written to does not stop the wait or change its answer.
#[test]
fn a_stderr_that_cannot_be_written_does_not_stop_the_wait() {
    struct Closed;
    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut net = Script::new(vec![silent("timed out"); 6]);
    net.answers.push_back(Ok(200));
    let got = wait_for(&mut net, PLAN, &mut Closed);
    assert_eq!(
        got.as_deref(),
        Ok(
            "the coordinator answered (HTTP 200) on attempt 1 of 6 of round 2, after the rejoin (round 1 had no answer in 6 attempts)"
        )
    );
    // The double is a whole stream: its flush, which the wait does not call, succeeds.
    assert!(std::io::Write::flush(&mut Closed).is_ok());
}

/// Proves: a number flag takes decimal digits from its minimum to its maximum and nothing else.
#[test]
fn a_number_is_digits_within_its_bounds() {
    assert_eq!(number(b"1", 1, 100), Some(1));
    assert_eq!(number(b"100", 1, 100), Some(100));
    assert_eq!(number(b"007", 1, 100), Some(7));
    assert_eq!(number(b"0", 0, 600), Some(0));
    for refused in [
        b"0".as_slice(),
        b"101",
        b"",
        b"-1",
        b"+1",
        b"1.5",
        b" 1",
        b"ten",
        b"1000000",
    ] {
        assert_eq!(number(refused, 1, 100), None, "{refused:?}");
    }
}

/// Proves: what a `tailscale` said is quoted as its last line that is not blank, its words
/// joined by one space, cut at 300 bytes on a character's boundary.
#[test]
fn what_a_program_said_is_its_last_line_cut_short() {
    assert_eq!(last_line(b""), "");
    assert_eq!(last_line(b"\n  \n"), "");
    assert_eq!(last_line(b"first\n  access   denied \n\n"), "access denied");
    let long = "a".repeat(299) + "\u{e9}tail";
    assert_eq!(last_line(long.as_bytes()), "a".repeat(299));
    assert_eq!(last_line("b".repeat(400).as_bytes()), "b".repeat(300));
}
