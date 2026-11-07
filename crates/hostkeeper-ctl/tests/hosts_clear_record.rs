//! `hostkeeper ctl hosts clear-record`, run as an operator types it, against a generated
//! `hostkeeper.operator.v1.Hosts` server over the in-memory gRPC route: the flags reach the
//! request, the order id the coordinator answers is printed, and its refusals `daemon_too_old`
//! and `unknown_host` end the command with their exit codes and tokens. The coordinator's own
//! answer is tested in `hostkeeper-coordinator` (`clear_record_tests`).

use std::sync::{Arc, Mutex};

use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{Io, PeerIdentity, exit};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::{Hosts, HostsServer};
use serde_json::{Value, json};
use tonic::{Code, Request, Response, Status};

/// A `Hosts` server whose `ClearRecord` orders `s1`, refuses `old-1` `daemon_too_old` and any
/// other host `unknown_host`, and records every request it is sent.
#[derive(Clone, Default)]
struct Answers(Arc<Mutex<Vec<proto::ClearRecordRequest>>>);

#[tonic::async_trait]
impl Hosts for Answers {
    async fn list(
        &self,
        _: Request<proto::ListHostsRequest>,
    ) -> Result<Response<proto::ListHostsResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn get(
        &self,
        _: Request<proto::GetHostRequest>,
    ) -> Result<Response<proto::GetHostResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn exclude(
        &self,
        _: Request<proto::ExcludeRequest>,
    ) -> Result<Response<proto::ExcludeResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn include(
        &self,
        _: Request<proto::IncludeRequest>,
    ) -> Result<Response<proto::IncludeResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn set_labels(
        &self,
        _: Request<proto::SetLabelsRequest>,
    ) -> Result<Response<proto::SetLabelsResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn abort(
        &self,
        _: Request<proto::AbortRequest>,
    ) -> Result<Response<proto::AbortResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn retire(
        &self,
        _: Request<proto::RetireRequest>,
    ) -> Result<Response<proto::RetireResponse>, Status> {
        Err(Status::unimplemented("not in this test"))
    }

    async fn clear_record(
        &self,
        request: Request<proto::ClearRecordRequest>,
    ) -> Result<Response<proto::ClearRecordResponse>, Status> {
        let request = request.into_inner();
        self.0.lock().expect("lock").push(request.clone());
        match request.node_id.as_str() {
            "s1" => Ok(Response::new(proto::ClearRecordResponse {
                order_id: "clear-record-12".to_owned(),
            })),
            "old-1" => Err(Status::new(Code::FailedPrecondition, "daemon_too_old")),
            _ => Err(Status::new(Code::NotFound, "unknown_host")),
        }
    }
}

/// What one run of the command ended with: the exit code, stdout and stderr.
struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// `hostkeeper ctl hosts clear-record --node-id <node> --service <service>` with a token file in
/// `dir`, against `memory`.
fn clear(memory: &InMemory, dir: &std::path::Path, node: &str, service: &str) -> Ran {
    let token_file = dir.join("token");
    std::fs::write(&token_file, "operator-token-1\n").expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let (mut stdin, mut stdout, mut stderr): (&[u8], Vec<u8>, Vec<u8>) =
        (b"", Vec::new(), Vec::new());
    let words = [
        "hostkeeper-ctl",
        "hosts",
        "clear-record",
        "--node-id",
        node,
        "--service",
        service,
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--token-file",
        token_file,
        "--output",
        "json",
    ];
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
        stdout: String::from_utf8(stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(stderr).expect("UTF-8 stderr"),
    }
}

/// Catches: a flag of the generated command sent in another field or dropped, an order id the
/// command does not print or prints under another name, and a refusal that exits 0, exits with
/// another code, or prints another token.
#[test]
fn hosts_clear_record_sends_the_host_and_service_and_prints_the_order_or_the_refusal() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-hosts-clear-record",
    )
    .expect("a scratch directory");
    let answers = Answers::default();
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(HostsServer::new(answers.clone()));

    let ordered = clear(&memory, dir.path(), "s1", "web");
    assert_eq!(ordered.code, exit::SUCCESS, "{}", ordered.stderr);
    assert_eq!(ordered.stderr, "");
    let printed: Value = serde_json::from_str(&ordered.stdout).expect("JSON on stdout");
    assert_eq!(printed, json!({"orderId": "clear-record-12"}));

    let too_old = clear(&memory, dir.path(), "old-1", "web");
    assert_eq!(
        (
            too_old.code,
            too_old.stdout.as_str(),
            too_old.stderr.as_str()
        ),
        (exit::USAGE, "", "refused: daemon_too_old\n")
    );

    let unknown = clear(&memory, dir.path(), "ghost", "db");
    assert_eq!(
        (
            unknown.code,
            unknown.stdout.as_str(),
            unknown.stderr.as_str()
        ),
        (exit::FAILURE, "", "error: NOT_FOUND: unknown_host\n")
    );

    let sent: Vec<(String, String)> = answers
        .0
        .lock()
        .expect("lock")
        .iter()
        .map(|request| (request.node_id.clone(), request.service.clone()))
        .collect();
    assert_eq!(
        sent,
        [
            ("s1".to_owned(), "web".to_owned()),
            ("old-1".to_owned(), "web".to_owned()),
            ("ghost".to_owned(), "db".to_owned()),
        ]
    );
}
