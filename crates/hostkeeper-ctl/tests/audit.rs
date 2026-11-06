//! `hostkeeper ctl audit list`, run as an operator types it: parsed from the generated command
//! table, encoded, sent with the operator's token over the in-memory gRPC route to the generated
//! `hostkeeper.operator.v1.Audit` server, and the answer printed. The server here answers what
//! it is told to and records each request; the coordinator's own `Audit.List` behind it is
//! tested in `hostkeeper-coordinator` (`audit_list_tests`).

use std::sync::{Arc, Mutex};

use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{Io, PeerIdentity, exit};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::audit_server::{Audit, AuditServer};
use serde_json::{Value, json};
use tonic::{Code, Request, Response, Status};

/// The token the operator's file holds.
const TOKEN: &str = "operator-token-1";

/// What the server saw.
#[derive(Default)]
struct Seen {
    requests: Vec<proto::ListAuditRequest>,
    bearers: Vec<String>,
}

/// An `Audit` server that answers two entries, one with a time and a name, and refuses a
/// `by_name` of `Bad` as the coordinator does (`invalid_request`).
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Seen>>);

impl Log {
    fn seen<T>(&self, read: impl FnOnce(&mut Seen) -> T) -> T {
        read(&mut self.0.lock().expect("lock"))
    }
}

#[tonic::async_trait]
impl Audit for Log {
    async fn list(
        &self,
        request: Request<proto::ListAuditRequest>,
    ) -> Result<Response<proto::ListAuditResponse>, Status> {
        let bearer = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        let asked = request.into_inner();
        let refused = asked.by_name.as_deref() == Some("Bad");
        self.seen(|s| {
            s.bearers.push(bearer);
            s.requests.push(asked);
        });
        if refused {
            return Err(Status::new(Code::InvalidArgument, "invalid_request"));
        }
        let entry = |seq, by_name: &str, at_unix| proto::AuditEntry {
            seq,
            by: proto::Actor::Operator.into(),
            action: "hold".to_owned(),
            service: Some("web".to_owned()),
            node_id: None,
            reason: Some("kernel patch".to_owned()),
            at_unix,
            by_name: Some(by_name.to_owned()),
        };
        Ok(Response::new(proto::ListAuditResponse {
            entries: vec![
                entry(41, "alice", Some(1_800_000_000)),
                entry(42, "alice", None),
            ],
            next_seq: 42,
        }))
    }
}

/// One run of `hostkeeper ctl`: its exit code and streams.
struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// Runs `hostkeeper ctl <args>` with the operator's token file, against `memory`.
fn ctl(memory: &InMemory, token_file: &str, args: &[&str]) -> Ran {
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let globals = [
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--token-file",
        token_file,
        "--output",
        "json",
    ];
    let words = std::iter::once("hostkeeper-ctl")
        .chain(args.iter().copied())
        .chain(globals);
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

/// Catches: `--by-name` or `--newest` missing from the generated table or not reaching its
/// field, a flag that sets another field, the call routed elsewhere or sent without the
/// operator's token, the entries' `atUnix` and `byName` not printed, a value of `--newest` that
/// is no bool taken as false rather than refused before any call, and a refusal not ending the
/// run with its token on stderr. Without the two flags the request leaves them unset.
#[test]
fn audit_list_sends_by_name_and_newest_and_prints_time_and_name() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-audit",
    )
    .expect("a scratch directory");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let log = Log::default();
    let memory =
        InMemory::new(PeerIdentity::Operator("op".to_owned())).serve(AuditServer::new(log.clone()));

    let listed = ctl(
        &memory,
        token_file,
        &[
            "audit",
            "list",
            "--by-name",
            "alice",
            "--newest",
            "true",
            "--since-seq",
            "40",
            "--limit",
            "5",
        ],
    );
    assert_eq!(listed.code, exit::SUCCESS, "{}", listed.stderr);
    let printed: Value = serde_json::from_str(&listed.stdout).expect("JSON on stdout");
    assert_eq!(
        printed,
        json!({"entries": [
                   {"seq": "41", "by": "ACTOR_OPERATOR", "action": "hold", "service": "web",
                    "reason": "kernel patch", "atUnix": "1800000000", "byName": "alice"},
                   {"seq": "42", "by": "ACTOR_OPERATOR", "action": "hold", "service": "web",
                    "reason": "kernel patch", "byName": "alice"}],
               "nextSeq": "42"})
    );

    let plain = ctl(&memory, token_file, &["audit", "list"]);
    assert_eq!(plain.code, exit::SUCCESS, "{}", plain.stderr);

    let not_a_bool = ctl(&memory, token_file, &["audit", "list", "--newest", "maybe"]);
    assert_eq!(not_a_bool.code, exit::USAGE, "{}", not_a_bool.stderr);
    assert_eq!(not_a_bool.stdout, "");

    let refused = ctl(&memory, token_file, &["audit", "list", "--by-name", "Bad"]);
    assert_eq!(refused.code, exit::USAGE, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("invalid_request"),
        "{}",
        refused.stderr
    );
    assert_eq!(refused.stdout, "");

    assert_eq!(
        log.seen(|s| s.requests.clone()),
        vec![
            proto::ListAuditRequest {
                since_seq: 40,
                limit: 5,
                by_name: Some("alice".to_owned()),
                newest: true,
                ..proto::ListAuditRequest::default()
            },
            proto::ListAuditRequest::default(),
            proto::ListAuditRequest {
                by_name: Some("Bad".to_owned()),
                ..proto::ListAuditRequest::default()
            },
        ],
        "three calls: the bad bool is refused before any"
    );
    let bearers = log.seen(|s| s.bearers.clone());
    assert!(
        bearers.iter().all(|b| *b == format!("Bearer {TOKEN}")),
        "{bearers:?}"
    );
}
