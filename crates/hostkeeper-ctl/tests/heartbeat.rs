//! `hostkeeper ctl coordinator heartbeat`, run as a host's operator types it: parsed from the
//! generated command table, encoded, sent over the in-memory gRPC route to the generated
//! `hostkeeper.agent.v1.Coordinator` server, and the answer printed. The server here records the
//! request; the coordinator's own handling of it is tested in `hostkeeper-coordinator`.

use std::sync::{Arc, Mutex};

use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{Io, PeerIdentity, exit};
use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_proto::hostkeeper::agent::v1::coordinator_server::{Coordinator, CoordinatorServer};
use tonic::{Code, Request, Response, Status};

/// A `Coordinator` server that records each heartbeat and answers it with no order.
#[derive(Clone, Default)]
struct Heard(Arc<Mutex<Vec<proto::HeartbeatRequest>>>);

impl Heard {
    fn beats(&self) -> Vec<proto::HeartbeatRequest> {
        self.0.lock().expect("lock").clone()
    }
}

#[tonic::async_trait]
impl Coordinator for Heard {
    async fn heartbeat(
        &self,
        request: Request<proto::HeartbeatRequest>,
    ) -> Result<Response<proto::HeartbeatResponse>, Status> {
        self.0.lock().expect("lock").push(request.into_inner());
        Ok(Response::new(proto::HeartbeatResponse::default()))
    }

    async fn enroll(
        &self,
        _request: Request<proto::EnrollRequest>,
    ) -> Result<Response<proto::EnrollResponse>, Status> {
        Err(Status::new(Code::Unimplemented, "not in this test"))
    }

    async fn poll(
        &self,
        _request: Request<proto::PollRequest>,
    ) -> Result<Response<proto::PollResponse>, Status> {
        Err(Status::new(Code::Unimplemented, "not in this test"))
    }

    async fn renew(
        &self,
        _request: Request<proto::RenewRequest>,
    ) -> Result<Response<proto::RenewResponse>, Status> {
        Err(Status::new(Code::Unimplemented, "not in this test"))
    }
}

/// Runs `hostkeeper ctl coordinator heartbeat <args>` as the host `s1`, against `heard`: the exit
/// code and stderr.
fn heartbeat(heard: &Heard, args: &[&str]) -> (u8, String) {
    let memory = InMemory::new(PeerIdentity::Host("s1".to_owned()))
        .serve(CoordinatorServer::new(heard.clone()));
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let globals = [
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--client-cert-file",
        "host.pem",
        "--client-key-file",
        "host.key",
        "--output",
        "json",
    ];
    let words = ["hostkeeper-ctl", "coordinator", "heartbeat"]
        .into_iter()
        .chain(args.iter().copied())
        .chain(globals);
    let code = hostkeeper_ctl::run(
        words,
        &memory,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    (code, String::from_utf8(stderr).expect("UTF-8"))
}

/// Catches a flag of the host's clock or certificate that does not reach its field, reaches
/// another, or is sent when not given: with `--sent-unix`, `--cert-not-after-unix` and
/// `--last-renew-error` the request carries each value in its own field and the run succeeds;
/// without them the three fields are unset; and a time that is no number is refused before
/// anything is sent.
#[test]
fn the_heartbeat_flags_of_the_host_clock_and_certificate_reach_their_fields() {
    let heard = Heard::default();
    let given = heartbeat(
        &heard,
        &[
            "--platform",
            "linux_x86_64",
            "--sent-unix",
            "1800000123",
            "--cert-not-after-unix",
            "1800600000",
            "--last-renew-error",
            "renewal refused: host_excluded",
        ],
    );
    assert_eq!(given, (exit::SUCCESS, String::new()));
    let bare = heartbeat(&heard, &["--platform", "linux_x86_64"]);
    assert_eq!(bare, (exit::SUCCESS, String::new()));
    let beats = heard.beats();
    let said: Vec<_> = beats
        .iter()
        .map(|beat| {
            (
                beat.platform(),
                beat.sent_unix,
                beat.cert_not_after_unix,
                beat.last_renew_error.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            (
                proto::Platform::LinuxX8664,
                Some(1_800_000_123),
                Some(1_800_600_000),
                Some("renewal refused: host_excluded"),
            ),
            (proto::Platform::LinuxX8664, None, None, None),
        ]
    );

    let (code, _) = heartbeat(&heard, &["--sent-unix", "soon"]);
    assert_eq!(code, exit::USAGE);
    assert_eq!(heard.beats().len(), 2, "nothing more was sent");
}
