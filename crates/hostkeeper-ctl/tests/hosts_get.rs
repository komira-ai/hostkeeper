//! `hostkeeper ctl hosts get`, run as an operator types it, against a generated
//! `hostkeeper.operator.v1.Hosts` server over the in-memory gRPC route: the host's first
//! heartbeat time is printed as the coordinator answers it, and left out when it answers none.
//! The coordinator's own answer is tested in `hostkeeper-coordinator` (`first_heartbeat_tests`).

use std::sync::{Arc, Mutex};

use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{Io, PeerIdentity, exit};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::{Hosts, HostsServer};
use serde_json::{Value, json};
use tonic::{Request, Response, Status};

/// A `Hosts` server that answers `Get` of `db-1` (enrolled, heard) and of `db-2` (enrolled, not
/// heard since its approval), and records the node ids asked for.
#[derive(Clone, Default)]
struct Answers(Arc<Mutex<Vec<String>>>);

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
        request: Request<proto::GetHostRequest>,
    ) -> Result<Response<proto::GetHostResponse>, Status> {
        let node_id = request.into_inner().node_id;
        self.0.lock().expect("lock").push(node_id.clone());
        let first_heartbeat_unix = (node_id == "db-1").then_some(1_800_000_090);
        Ok(Response::new(proto::GetHostResponse {
            host: Some(proto::Host {
                node_id,
                role: "storage".to_owned(),
                enrolled_unix: 1_800_000_000,
                first_heartbeat_unix,
                ..proto::Host::default()
            }),
        }))
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
}

/// `hostkeeper ctl hosts get --node-id <node>` with a token file in `dir`, against `memory`:
/// the exit code and stdout as JSON.
fn get(memory: &InMemory, dir: &std::path::Path, node: &str) -> Value {
    let token_file = dir.join("token");
    std::fs::write(&token_file, "operator-token-1\n").expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let (mut stdin, mut stdout, mut stderr): (&[u8], Vec<u8>, Vec<u8>) =
        (b"", Vec::new(), Vec::new());
    let words = [
        "hostkeeper-ctl",
        "hosts",
        "get",
        "--node-id",
        node,
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
    assert_eq!(code, exit::SUCCESS, "{}", String::from_utf8_lossy(&stderr));
    serde_json::from_slice(&stdout).expect("JSON on stdout")
}

/// Catches: a first heartbeat time the generated command drops or prints under another name, a
/// host with none printed with a time (0 or the approval's), and the call sent for another host.
#[test]
fn hosts_get_prints_the_first_heartbeat_time_the_coordinator_answers() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-hosts-get",
    )
    .expect("a scratch directory");
    let answers = Answers::default();
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(HostsServer::new(answers.clone()));

    assert_eq!(
        get(&memory, dir.path(), "db-1")["host"],
        json!({"nodeId": "db-1", "role": "storage", "enrolledUnix": "1800000000",
               "firstHeartbeatUnix": "1800000090"})
    );
    assert_eq!(
        get(&memory, dir.path(), "db-2")["host"],
        json!({"nodeId": "db-2", "role": "storage", "enrolledUnix": "1800000000"})
    );
    assert_eq!(*answers.0.lock().expect("lock"), ["db-1", "db-2"]);
}
