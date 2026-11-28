//! `hk releases wait` through `run`, against a stand-in coordinator: a loopback server of port 0
//! that answers each `Releases.WaitConverged` from a script, on a simulated clock it shares with
//! the world's. A call the stand-in holds to its deadline moves the clock by the seconds it was
//! asked for, a pause of `hk` moves it by the pause, and nothing sleeps; a hang-up is a connection
//! cut in mid-call. One test runs it against the real gateway of an empty coordinator.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_hk::tailnet_wait::Tailnet;
use serde_json::{Value, json};

use crate::rig::{Opts, Ran, Reply, Request, Rig, Server, args};

/// When the stand-in answers, by its own clock: 2027-01-15T08:01:00Z.
const ANSWERED: &str = "1800000060";

/// The stuck Mac, as the gateway writes it.
fn stuck() -> Value {
    json!({
        "nodeId": "mac-2",
        "role": "mac-build",
        "state": "WAITING_STATE_UPDATING",
        "installedVersion": "0.3.211",
        "updatingTo": "0.3.212",
        "step": "verify",
        "stepAttempt": 1,
    })
}

/// The answer of `outcome` as the gateway writes it, the stuck Mac not yet when it is `DEADLINE`.
fn answer(outcome: &str) -> String {
    let mut answer = json!({
        "outcome": format!("WAIT_OUTCOME_{outcome}"),
        "convergence": {"desired": "0.3.212", "state": "CONVERGENCE_STATE_IN_PROGRESS"},
    });
    if outcome == "DEADLINE" {
        answer["notYet"] = json!({
            "hosts": [stuck()],
            "plans": [{"hostGroup": "mac-build", "waiting": "PLAN_WAIT_UPDATING"}],
            "answeredUnix": ANSWERED,
        });
    } else {
        answer["convergence"]["state"] = json!("CONVERGENCE_STATE_CONVERGED");
    }
    answer.to_string()
}

/// The `waitSecs` a request asked for.
fn wait_secs(request: &Request) -> u64 {
    serde_json::from_slice::<Value>(&request.body)
        .ok()
        .and_then(|body| body["waitSecs"].as_u64())
        .unwrap_or_default()
}

/// A stand-in coordinator and the clock it shares with the world: `script` answers the `n`-th call
/// (from 1) at the clock's second, and a `DEADLINE` it answers moves the clock by the call's
/// `waitSecs`, as a call held to its deadline would.
struct Stand {
    server: Server,
    clock: Arc<AtomicU64>,
}

impl Stand {
    fn new(rig: &Rig, script: impl Fn(u64, u64) -> Reply + Send + 'static) -> Stand {
        let clock = Arc::new(AtomicU64::new(0));
        let shared = Arc::clone(&clock);
        let calls = AtomicU64::new(0);
        let server = Server::serve(
            &rig.pki,
            || (),
            move |(), request| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let reply = script(n, shared.load(Ordering::SeqCst));
                if reply.body.windows(8).any(|w| w == b"DEADLINE") {
                    shared.fetch_add(wait_secs(request), Ordering::SeqCst);
                }
                reply
            },
        );
        Stand { server, clock }
    }

    /// `hk <globals> releases wait <words>` with the world's wait and clock on the shared clock.
    fn run(&self, rig: &Rig, words: &[&str]) -> Ran {
        let wait = |time: Duration| {
            self.clock.fetch_add(time.as_secs(), Ordering::SeqCst);
        };
        let now = || Duration::from_secs(self.clock.load(Ordering::SeqCst));
        let tailnet = Tailnet {
            tailscale: rig.path("no-tailscale"),
            limit: Duration::from_secs(1),
            wait: &wait,
            now: &now,
        };
        let mut all = rig.globals(self.server.port);
        all.extend(args(&["releases", "wait"]));
        all.extend(args(words));
        rig.run(Opts::real().tailnet(&tailnet), &all)
    }
}

/// The flags of a wait for tierstore 0.3.212 in mac-build for 1800 seconds, its answer in
/// `answer.json`.
fn waiting(rig: &Rig) -> Vec<String> {
    let mut words = args(&[
        "--service",
        "tierstore",
        "--version",
        "0.3.212",
        "--wait-secs",
        "1800",
        "--host-group",
        "mac-build",
        "--response-file",
    ]);
    words.push(rig.path("answer.json").display().to_string());
    words
}

/// The answer `hk` wrote.
fn written(rig: &Rig) -> Value {
    let text = std::fs::read_to_string(rig.path("answer.json")).expect("an answer");
    serde_json::from_str(&text).expect("JSON")
}

/// What the stuck Mac's table prints under `head`.
fn block(head: &str) -> String {
    format!(
        "hk: releases wait: tierstore 0.3.212 in mac-build: {head}\n  state: in progress\n  mac-2 \
         (mac-build): updating to 0.3.212, step verify (try 1), on 0.3.211, seen so since \
         2027-01-15T08:01:00Z\n  the plan of mac-build waits: updating\n"
    )
}

/// Proves: against a stand-in coordinator, a fleet that converges after 25 minutes of simulated
/// time is reported converged: exit 0, the converged answer in the response file, each call held
/// for at most 60 seconds with a token of its own and the request's service, version and group,
/// and the table printed once.
///
/// Catches: a wait that rides one call for the whole bound (the cut of half-hour calls), or one
/// that gives up before the fleet converges.
#[test]
fn a_fleet_that_converges_after_25_minutes_is_reported_converged() {
    let rig = Rig::new("releases-wait-converges");
    let stand = Stand::new(&rig, |_, clock| {
        let outcome = if clock < 1500 {
            "DEADLINE"
        } else {
            "CONVERGED"
        };
        Reply::new(200, answer(outcome))
    });
    let words = waiting(&rig);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let ran = stand.run(&rig, &words);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(written(&rig)["outcome"], "WAIT_OUTCOME_CONVERGED");
    assert_eq!(ran.stderr, block("not converged after 1m00s of 30m00s:"));
    let seen = stand.server.seen();
    assert_eq!(seen.len(), 26);
    for call in &seen {
        assert_eq!(
            call.target,
            "/hostkeeper.operator.v1.Releases/WaitConverged"
        );
        assert_eq!(
            call.header("authorization"),
            Some(format!("Bearer {}", crate::rig::TOKEN).as_str())
        );
        let body: Value = serde_json::from_slice(&call.body).expect("JSON");
        assert_eq!(
            body,
            json!({"service": "tierstore", "version": "0.3.212", "hostGroup": "mac-build",
                "waitSecs": 60})
        );
    }
}

/// Proves: a fleet with one host stuck updating ends at the bound with the last answer
/// (`DEADLINE`, which the workflow reads as not converged), and the message on stderr names the
/// host, its state and since when; the calls fill the 30 minutes and no more.
///
/// Catches: a wait that ends at its bound saying nothing of what it waited on.
#[test]
fn a_host_stuck_updating_fails_at_the_bound_naming_the_host_its_state_and_since_when() {
    let rig = Rig::new("releases-wait-stuck");
    let stand = Stand::new(&rig, |_, _| Reply::new(200, answer("DEADLINE")));
    let words = waiting(&rig);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let ran = stand.run(&rig, &words);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(written(&rig)["outcome"], "WAIT_OUTCOME_DEADLINE");
    assert_eq!(
        ran.stderr,
        [
            block("not converged after 1m00s of 30m00s:"),
            block("did not converge within 30m00s; it still waits on:"),
        ]
        .concat()
    );
    assert_eq!(stand.server.seen().len(), 30);
    assert_eq!(stand.clock.load(Ordering::SeqCst), 1800);
}

/// Proves: a connection cut in mid-wait (the stand-in hangs up on the 3rd call) does not fail the
/// step: it is said, the same question is asked again 10 seconds later, and the wait ends
/// converged with exit 0.
///
/// Catches: a cut connection that ends the release red with a transport error that names nothing.
#[test]
fn a_connection_cut_in_mid_wait_does_not_fail_the_step() {
    let rig = Rig::new("releases-wait-cut");
    let stand = Stand::new(&rig, |n, clock| match (n, clock) {
        (3, _) => Reply::hang_up(),
        (_, 0..300) => Reply::new(200, answer("DEADLINE")),
        _ => Reply::new(200, answer("CONVERGED")),
    });
    let words = waiting(&rig);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let ran = stand.run(&rig, &words);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(written(&rig)["outcome"], "WAIT_OUTCOME_CONVERGED");
    let lines: Vec<&str> = ran.stderr.lines().collect();
    assert_eq!(lines.len(), 5, "{}", ran.stderr);
    let (head, tail) = lines[4].split_once(" (").expect("a reason");
    assert_eq!(head, "hk: releases wait: no answer after 2m00s");
    assert!(
        tail.starts_with(&format!(
            "no answer from https://127.0.0.1:{}",
            stand.server.port
        )),
        "{tail}"
    );
    assert!(tail.ends_with("); asking again in 10s"), "{tail}");
    assert_eq!(
        stand.server.seen().len(),
        7,
        "two held, the cut, three held, the converged"
    );
}

/// Proves: when no call is answered within the bound, `hk` exits 1 with the last call's reason.
///
/// Catches: a coordinator that is down read as a fleet that did not converge.
#[test]
fn no_answer_within_the_bound_exits_1_with_the_last_reason() {
    let rig = Rig::new("releases-wait-down");
    let stand = Stand::new(&rig, |_, _| Reply::hang_up());
    let ran = stand.run(
        &rig,
        &[
            "--service",
            "tierstore",
            "--version",
            "0.3.212",
            "--wait-secs",
            "25",
            "--host-group=",
        ],
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    let last = ran.stderr.lines().last().unwrap_or_default();
    let want = format!(
        "hk: no answer from https://127.0.0.1:{} in 0m25s: the last call: no answer from",
        stand.server.port
    );
    assert!(last.starts_with(&want), "{last}");
    assert_eq!(stand.server.seen().len(), 3);
    let body: Value = serde_json::from_slice(&stand.server.seen()[0].body).expect("JSON");
    assert_eq!(
        body,
        json!({"service": "tierstore", "version": "0.3.212", "waitSecs": 25}),
        "an empty host group sends none"
    );
}

/// Proves: a call whose token the runner does not give is a call with no answer, asked again
/// inside the bound (a token request of the runner can fail for a moment), and the wait ends with
/// that reason; a call line that names no token at all is a usage error before any call, as is one
/// with no `--endpoint` or an empty `--response-file`.
///
/// Catches: a release red on one failed token request, or a wait that runs its whole bound for a
/// command line that can never call.
#[test]
fn a_token_the_runner_does_not_give_is_asked_again_and_a_call_line_with_none_is_refused() {
    let rig = Rig::new("releases-wait-token");
    let stand = Stand::new(&rig, |_, _| Reply::new(200, answer("CONVERGED")));
    let wait = |rig: &Rig, globals: Vec<String>| {
        let wait = |time: Duration| {
            stand.clock.fetch_add(time.as_secs(), Ordering::SeqCst);
        };
        let now = || Duration::from_secs(stand.clock.load(Ordering::SeqCst));
        let tailnet = Tailnet {
            tailscale: rig.path("no-tailscale"),
            limit: Duration::from_secs(1),
            wait: &wait,
            now: &now,
        };
        let mut all = globals;
        all.extend(args(&[
            "releases",
            "wait",
            "--service",
            "s",
            "--version",
            "1",
            "--wait-secs",
            "15",
        ]));
        rig.run(Opts::real().tailnet(&tailnet), &all)
    };
    let endpoint = format!("https://127.0.0.1:{}", stand.server.port);
    let ca = rig.path("ca.pem").display().to_string();
    let ran = wait(
        &rig,
        args(&[
            "--endpoint",
            &endpoint,
            "--ca-file",
            &ca,
            "--github-oidc-audience",
            "aud",
        ]),
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    let no_token = "--github-oidc-audience needs the job to have id-token: write: the runner set \
                    no ACTIONS_ID_TOKEN_REQUEST_URL and ACTIONS_ID_TOKEN_REQUEST_TOKEN";
    assert_eq!(
        ran.stderr,
        format!(
            "hk: releases wait: no answer after 0m00s ({no_token}); asking again in 10s\n\
             hk: releases wait: no answer after 0m10s ({no_token}); asking again in 5s\n\
             hk: no answer from {endpoint} in 0m15s: the last call: {no_token}\n"
        )
    );
    for (globals, want) in [
        (
            args(&["--endpoint", &endpoint, "--ca-file", &ca]),
            "hk: a call needs exactly one of --token-file, --token-stdin and \
             --github-oidc-audience\n",
        ),
        (args(&["--ca-file", &ca]), "hk: a call needs --endpoint\n"),
        (
            [rig.globals(stand.server.port), args(&["--response-file="])].concat(),
            "hk: --response-file needs a path\n",
        ),
    ] {
        let ran = wait(&rig, globals);
        assert_eq!((ran.code, ran.stderr.as_str()), (2, want));
    }
    assert!(stand.server.seen().is_empty(), "no call was made");
}

/// Proves: a refusal ends the wait at once with the generated command's lines and exit code.
///
/// Catches: a refused wait (`too_many_waits`, a bad token) retried until the bound.
#[test]
fn a_refusal_ends_the_wait_as_the_generated_commands_does() {
    let rig = Rig::new("releases-wait-refused");
    let stand = Stand::new(&rig, |_, _| {
        Reply::new(
            400,
            r#"{"code":"FAILED_PRECONDITION","reason":"too_many_waits"}"#,
        )
    });
    let ran = stand.run(
        &rig,
        &[
            "--service",
            "tierstore",
            "--version",
            "0.3.212",
            "--wait-secs",
            "1800",
        ],
    );
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            "hk: the coordinator answered FAILED_PRECONDITION\nrefused: too_many_waits\n"
        )
    );
    assert_eq!(stand.server.seen().len(), 1);
}

/// Proves: the usage errors of the flags, each with exit 2 and its own message, and `--help`; a
/// bound that is a number out of 1 to 1800 is also `refused: invalid_wait`, the token the
/// coordinator gave a held call of it, which the workflow reads as the job's reason.
///
/// Catches: a wait asked for more than the coordinator's own bound, a `max_polls` above 180 read
/// by the workflow as no answer, or a flag taken twice.
#[test]
fn the_flags_and_their_usage_errors() {
    let rig = Rig::new("releases-wait-flags");
    let stand = Stand::new(&rig, |_, _| Reply::new(200, answer("CONVERGED")));
    let all = ["--service", "s", "--version", "1"];
    for (words, want) in [
        (
            vec!["--service", "s"],
            "hk: hk releases wait needs --service, --version and --wait-secs\n",
        ),
        (
            [&all[..], &["--wait-secs", "0"]].concat(),
            "refused: invalid_wait\nhk: --wait-secs takes a whole number from 1 to 1800\n",
        ),
        (
            [&all[..], &["--wait-secs", "1801"]].concat(),
            "refused: invalid_wait\nhk: --wait-secs takes a whole number from 1 to 1800\n",
        ),
        (
            [&all[..], &["--wait-secs", "+60"]].concat(),
            "hk: --wait-secs takes a whole number from 1 to 1800\n",
        ),
        (
            [&all[..], &["--service", "t"]].concat(),
            "hk: --service is given twice\n",
        ),
        (
            vec!["--later", "1"],
            "hk: hk releases wait has no flag --later\n",
        ),
        (
            vec!["later"],
            "hk: hk releases wait takes no argument: later\n",
        ),
    ] {
        let ran = stand.run(&rig, &words);
        assert_eq!((ran.code, ran.stderr.as_str()), (2, want), "{words:?}");
    }
    assert!(stand.server.seen().is_empty(), "no usage error calls");
    let ran = stand.run(&rig, &["--help"]);
    assert_eq!(ran.code, 0);
    assert!(
        ran.stdout
            .starts_with("hk releases wait: waits for the fleet to converge on a version"),
        "{}",
        ran.stdout
    );
}

/// Proves: against the real gateway of a coordinator with no desired version of the legacy
/// service, a wait ends at once (`SUPERSEDED`), and the answer carries the `notYet` the coordinator
/// writes, read by the names `hk` reads: the field names of the two agree.
///
/// Catches: a client that reads names the gateway does not write, so that every table is empty.
#[test]
fn the_real_gateway_answers_a_wait_with_what_it_waits_on() {
    let rig = Rig::new("releases-wait-gateway");
    let server = crate::gateway::start(&rig.pki, &rig.path(""));
    let mut all = rig.globals(server.port);
    all.extend(args(&[
        "releases",
        "wait",
        "--service",
        "kbf",
        "--version",
        "2.0",
        "--wait-secs",
        "60",
    ]));
    let ran = rig.run(Opts::real(), &all);
    assert_eq!(ran.code, 0, "{ran:?}");
    let answer: Value = serde_json::from_str(&ran.stdout).expect("JSON");
    assert_eq!(answer["outcome"], "WAIT_OUTCOME_SUPERSEDED");
    assert!(answer["notYet"]["answeredUnix"].is_string(), "{answer}");
    assert_eq!(
        ran.stderr,
        "hk: releases wait: kbf 2.0: ended superseded after 0m00s:\n  state: in progress\n  no \
         host is listed\n"
    );
}
