//! `hostkeeper ctl releases wait-converged`, run as CI or an operator types it, against a
//! generated `hostkeeper.operator.v1.Releases` server over the in-memory gRPC route: the three
//! flags reach the request as typed, the answer is printed as JSON, and the refusals of the wait
//! end the run with their token and exit code. What the coordinator answers is tested in
//! `hostkeeper-coordinator` (`grpc/tests/waits.rs`).

use std::sync::{Arc, Mutex};

use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{Io, PeerIdentity, exit};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::{Releases, ReleasesServer};
use serde_json::{Value, json};
use tonic::{Code, Request, Response, Status};

/// A `Releases` server whose `WaitConverged` records each request and answers by its
/// `wait_secs`: 0 is `invalid_wait`, 17 is `too_many_waits`, any other is `CONVERGED` on the
/// version asked for.
#[derive(Clone, Default)]
struct Waits(Arc<Mutex<Vec<proto::WaitConvergedRequest>>>);

/// The refusal of every method of `Releases` but the wait: no other method is called here.
fn other() -> Status {
    Status::unimplemented("not in this test")
}

#[tonic::async_trait]
impl Releases for Waits {
    async fn set_desired(
        &self,
        _: Request<proto::SetDesiredRequest>,
    ) -> Result<Response<proto::SetDesiredResponse>, Status> {
        Err(other())
    }

    async fn converge(
        &self,
        _: Request<proto::ConvergeRequest>,
    ) -> Result<Response<proto::ConvergeResponse>, Status> {
        Err(other())
    }

    async fn force(
        &self,
        _: Request<proto::ForceRequest>,
    ) -> Result<Response<proto::ForceResponse>, Status> {
        Err(other())
    }

    async fn unmark_bad(
        &self,
        _: Request<proto::UnmarkBadRequest>,
    ) -> Result<Response<proto::UnmarkBadResponse>, Status> {
        Err(other())
    }

    async fn clear_halt(
        &self,
        _: Request<proto::ClearHaltRequest>,
    ) -> Result<Response<proto::ClearHaltResponse>, Status> {
        Err(other())
    }

    async fn status(
        &self,
        _: Request<proto::StatusRequest>,
    ) -> Result<Response<proto::StatusResponse>, Status> {
        Err(other())
    }

    async fn mark_bad(
        &self,
        _: Request<proto::MarkBadRequest>,
    ) -> Result<Response<proto::MarkBadResponse>, Status> {
        Err(other())
    }

    async fn halt(
        &self,
        _: Request<proto::HaltRequest>,
    ) -> Result<Response<proto::HaltResponse>, Status> {
        Err(other())
    }

    async fn wait_converged(
        &self,
        request: Request<proto::WaitConvergedRequest>,
    ) -> Result<Response<proto::WaitConvergedResponse>, Status> {
        let asked = request.into_inner();
        self.0.lock().expect("lock").push(asked.clone());
        match asked.wait_secs {
            0 => Err(Status::new(Code::InvalidArgument, "invalid_wait")),
            17 => Err(Status::new(Code::FailedPrecondition, "too_many_waits")),
            _ => Ok(Response::new(proto::WaitConvergedResponse {
                outcome: proto::WaitOutcome::Converged.into(),
                convergence: Some(proto::Convergence {
                    desired: Some(asked.version),
                    state: proto::ConvergenceState::Converged.into(),
                    coordinator_commit: "0123456789ab".to_owned(),
                    ..proto::Convergence::default()
                }),
            })),
        }
    }
}

/// What one run printed and how it ended.
struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// `hostkeeper ctl releases wait-converged` with `flags`, a token file in `dir`, against
/// `memory`, with the JSON output.
fn wait(memory: &InMemory, dir: &std::path::Path, flags: &[&str]) -> Ran {
    let token_file = dir.join("token");
    std::fs::write(&token_file, "operator-token-1\n").expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let (mut stdin, mut stdout, mut stderr): (&[u8], Vec<u8>, Vec<u8>) =
        (b"", Vec::new(), Vec::new());
    let mut words = vec!["hostkeeper-ctl", "releases", "wait-converged"];
    words.extend_from_slice(flags);
    words.extend_from_slice(&[
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--token-file",
        token_file,
        "--timeout-secs",
        "1810",
        "--output",
        "json",
    ]);
    let code = hostkeeper_ctl::run(
        words,
        memory,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    Ran {
        code,
        stdout: String::from_utf8(stdout).expect("UTF-8"),
        stderr: String::from_utf8(stderr).expect("UTF-8"),
    }
}

/// Proves: the generated command sends `--service`, `--version` and `--wait-secs` as the
/// request's three fields and prints the answer as JSON (the outcome by its enum name, the
/// convergence); `invalid_wait` ends the run with exit 2 and `too_many_waits`, a
/// `FAILED_PRECONDITION`, with exit 2 too, each with its token on stderr and nothing on stdout;
/// a `--wait-secs` that is no 32-bit unsigned integer is a usage error before any call.
///
/// Catches: a flag of the wait dropped or sent to another field, an answer printed under other
/// names, and a refusal of the wait read as success or as a transport failure (exit 1), which
/// would cost CI's workflow its reason token.
#[test]
fn releases_wait_converged_sends_its_flags_and_prints_the_outcome_or_the_refusal() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-wait-converged",
    )
    .expect("a scratch directory");
    let waits = Waits::default();
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(ReleasesServer::new(waits.clone()));
    let flags = |secs: &'static str| ["--service", "web", "--version", "2.0", "--wait-secs", secs];

    let converged = wait(&memory, dir.path(), &flags("1800"));
    assert_eq!(converged.code, exit::SUCCESS, "{}", converged.stderr);
    let printed: Value = serde_json::from_str(&converged.stdout).expect("JSON on stdout");
    assert_eq!(
        printed,
        json!({
            "outcome": "WAIT_OUTCOME_CONVERGED",
            "convergence": {
                "desired": "2.0",
                "state": "CONVERGENCE_STATE_CONVERGED",
                "coordinatorCommit": "0123456789ab"
            }
        })
    );

    for (secs, token) in [("0", "invalid_wait"), ("17", "too_many_waits")] {
        let refused = wait(&memory, dir.path(), &flags(secs));
        assert_eq!(refused.code, exit::USAGE, "{secs}: {}", refused.stderr);
        assert!(refused.stderr.contains(token), "{secs}: {}", refused.stderr);
        assert_eq!(refused.stdout, "", "{secs}");
    }

    let not_a_number = wait(&memory, dir.path(), &flags("thirty"));
    assert_eq!(not_a_number.code, exit::USAGE, "{}", not_a_number.stderr);
    assert_eq!(not_a_number.stdout, "");

    let asked = |wait_secs| proto::WaitConvergedRequest {
        service: "web".to_owned(),
        version: "2.0".to_owned(),
        wait_secs,
    };
    assert_eq!(
        *waits.0.lock().expect("lock"),
        [asked(1800), asked(0), asked(17)],
        "three calls: the bad number is refused before any"
    );
}
