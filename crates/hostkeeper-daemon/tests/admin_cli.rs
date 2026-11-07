//! `hostkeeper admin` (`hostkeeper_daemon::admin_client`) against an admin server on a unix
//! socket in the test's scratch directory: the daemon's own server, with the host's enrollment and
//! the heartbeat loop's link set by the test, and a server of the test's that records each call.
//! The command line parses each verb, and refuses a verb without its service.

mod support;

use std::future::Future;
use std::io::{self, Write};
use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_ctl::exit;
use hostkeeper_daemon::admin::{self, Peer};
use hostkeeper_daemon::admin_client::{self, AdminCommand, AdminVerb};
use hostkeeper_daemon::cli::{self, Cli, Command, DEFAULT_ADMIN_SOCKET};
use hostkeeper_daemon::node::LinkHealth;
use hostkeeper_enroll::{Enrollment, HostId, RequestId};
use hostkeeper_proto::hostkeeper::agent::v1::FailurePhase;
use hostkeeper_proto::hostkeeper::local::v1::admin_server::{Admin, AdminServer};
use hostkeeper_proto::hostkeeper::local::v1::{
    AbortRequest, AbortResponse, ClearRecordRequest, ClearRecordResponse,
    Enrollment as WireEnrollment, EnrollmentState, Lifecycle, LinkHealth as WireLink, Liveness,
    RecordProblem, ServiceRecord, ServiceState, StatusRequest, StatusResponse, StepOutcome,
    StepRecord, Unhealthy,
};
use support::{Rig, Scratch};
use tokio::sync::oneshot;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

/// A server on a unix socket, on a thread of its own with its own runtime, stopped and joined
/// when it is dropped.
struct Served {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the server thread");
        }
    }
}

/// Binds `path` now, so that a client may connect as soon as this returns, and serves it with
/// `serve`, given the connections and the future that ends when the test is done.
fn on_socket<F, Fut>(path: &Path, serve: F) -> Served
where
    F: FnOnce(UnixListenerStream, oneshot::Receiver<()>) -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let listener = StdUnixListener::bind(path).expect("bind the socket");
    listener.set_nonblocking(true).expect("non-blocking");
    let (stop, stopped) = oneshot::channel();
    let thread = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).expect("the listener");
            serve(UnixListenerStream::new(listener), stopped).await;
        });
    });
    Served {
        stop: Some(stop),
        thread: Some(thread),
    }
}

/// The daemon of `rig` served on `path`, every connection taken as coming from `peer`.
fn serve_daemon(daemon: Arc<hostkeeper_daemon::Daemon>, path: &Path, peer: Peer) -> Served {
    on_socket(path, move |incoming, stopped| async move {
        let incoming = incoming.map(move |accepted| accepted.map(|stream| (stream, peer)));
        let stop = async move {
            let _ = stopped.await;
        };
        admin::serve_until(daemon, incoming, stop)
            .await
            .expect("the admin server");
    })
}

/// What a verb printed, and its exit code.
#[derive(Debug, PartialEq, Eq)]
struct Ran {
    code: u8,
    out: String,
    err: String,
}

fn run(socket: &Path, verb: AdminVerb) -> Ran {
    let command = AdminCommand {
        admin_socket: socket.to_path_buf(),
        verb,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = admin_client::run(&command, &mut out, &mut err);
    Ran {
        code,
        out: String::from_utf8(out).expect("UTF-8"),
        err: String::from_utf8(err).expect("UTF-8"),
    }
}

/// `Status` through the client's connection to `socket`.
fn status(socket: &Path) -> StatusResponse {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let mut client = admin_client::connect(socket).await.expect("connect");
        client
            .status(StatusRequest {})
            .await
            .expect("the status")
            .into_inner()
    })
}

const KEY: &str = "4f2a9c0e8b7d6a5f4e3d2c1b0a99887766554433221100ffeeddccbbaa998877";
const NOT_AFTER: u64 = 1_900_604_800;
const BEAT: u64 = 1_900_000_015;

/// Catches a field of the enrollment or of the link dropped, filled from the wrong state, or
/// carrying a value of another state; and the text of a state that is not what an operator is
/// shown: for no enrollment yet and for each of the four states, with the link never answered
/// after three failures or answered at a second, `Status` over the socket carries exactly the
/// expected enrollment and link, and `hostkeeper admin status` prints exactly the expected lines.
#[test]
fn status_reports_each_enrollment_state_and_the_link() {
    let rig = Rig::new("acli-states");
    let daemon = Arc::new(rig.daemon());
    let socket = rig.scratch.path().join("a.sock");
    let _served = serve_daemon(Arc::clone(&daemon), &socket, Peer { uid: Some(0) });
    let node = &daemon.ctx().node;

    let got = status(&socket);
    assert_eq!((got.enrollment, got.link), (None, None));
    assert_eq!(
        run(&socket, AdminVerb::Status).out,
        "enrollment: unknown\nlink: unknown\ntrusted_root.age_secs: unknown\ntrusted_root.reason: none\nservices: 0\n"
    );

    let failing = LinkHealth {
        last_success_unix: None,
        consecutive_failures: 3,
    };
    let answered = LinkHealth {
        last_success_unix: Some(BEAT),
        consecutive_failures: 0,
    };
    let wire = |state: EnrollmentState| WireEnrollment {
        state: state.into(),
        ..WireEnrollment::default()
    };
    let cases = [
        (
            Enrollment::Unenrolled,
            failing,
            wire(EnrollmentState::Unenrolled),
            "enrollment: unenrolled\nlink.last_success_unix: never\n\
             link.consecutive_failures: 3\ntrusted_root.age_secs: unknown\ntrusted_root.reason: none\nservices: 0\n"
                .to_owned(),
        ),
        (
            Enrollment::Pending {
                key_sha256: RequestId::parse(KEY).expect("a request id"),
            },
            failing,
            WireEnrollment {
                key_sha256: KEY.to_owned(),
                ..wire(EnrollmentState::Pending)
            },
            format!(
                "enrollment: pending\nenrollment.key_sha256: {KEY}\n\
                 link.last_success_unix: never\nlink.consecutive_failures: 3\ntrusted_root.age_secs: unknown\ntrusted_root.reason: none\nservices: 0\n"
            ),
        ),
        (
            Enrollment::Enrolled {
                host_id: HostId::new("web-17").expect("a host id"),
                not_after: NOT_AFTER,
            },
            answered,
            WireEnrollment {
                host_id: "web-17".to_owned(),
                not_after_unix: Some(NOT_AFTER),
                ..wire(EnrollmentState::Enrolled)
            },
            format!(
                "enrollment: enrolled\nenrollment.host_id: web-17\n\
                 enrollment.not_after_unix: {NOT_AFTER}\nlink.last_success_unix: {BEAT}\n\
                 link.consecutive_failures: 0\ntrusted_root.age_secs: unknown\ntrusted_root.reason: none\nservices: 0\n"
            ),
        ),
        (
            Enrollment::Unreadable("the pki directory's generation 1 is corrupt".to_owned()),
            answered,
            wire(EnrollmentState::Unreadable),
            format!(
                "enrollment: unreadable\nlink.last_success_unix: {BEAT}\n\
                 link.consecutive_failures: 0\ntrusted_root.age_secs: unknown\ntrusted_root.reason: none\nservices: 0\n"
            ),
        ),
    ];
    for (enrollment, link, expected, text) in cases {
        node.set_enrollment(enrollment.clone());
        node.set_link(link);
        let got = status(&socket);
        assert_eq!(got.enrollment, Some(expected), "{enrollment:?}");
        assert_eq!(
            got.link,
            Some(WireLink {
                last_success_unix: link.last_success_unix,
                consecutive_failures: link.consecutive_failures,
            })
        );
        assert!(got.services.is_empty());
        let ran = run(&socket, AdminVerb::Status);
        assert_eq!(
            ran,
            Ran {
                code: exit::SUCCESS,
                out: text,
                err: String::new(),
            },
            "{enrollment:?}"
        );
    }
}

/// Catches a connection the daemon closes unread taken for an answer: a peer that is not root
/// prints nothing on standard output, one line on standard error, and exits with a failure.
#[test]
fn a_connection_the_daemon_refuses_exits_with_a_failure() {
    let rig = Rig::new("acli-peer");
    let socket = rig.scratch.path().join("a.sock");
    let _served = serve_daemon(Arc::new(rig.daemon()), &socket, Peer { uid: Some(1000) });
    let ran = run(&socket, AdminVerb::Status);
    assert_eq!(ran.code, exit::FAILURE);
    assert_eq!(ran.out, "");
    assert!(ran.err.starts_with("hostkeeper admin: "), "{}", ran.err);
    assert_eq!(ran.err.lines().count(), 1, "{}", ran.err);
}

/// What the recording server answers, and what it was asked.
#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<String>>,
    status: StatusResponse,
    abort: Option<AbortResponse>,
}

impl Recording {
    fn note(&self, call: String) {
        self.calls.lock().expect("the calls").push(call);
    }
}

/// `not_installed` for any service but `web`, as the daemon refuses one not installed.
fn installed(service: &str) -> Result<(), Status> {
    match service {
        "web" => Ok(()),
        _ => Err(Status::not_found("not_installed")),
    }
}

/// The recording server as tonic serves it.
struct Recorder(Arc<Recording>);

impl std::ops::Deref for Recorder {
    type Target = Recording;

    fn deref(&self) -> &Recording {
        &self.0
    }
}

#[tonic::async_trait]
impl Admin for Recorder {
    async fn status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        self.note("status".to_owned());
        Ok(Response::new(self.status.clone()))
    }

    async fn abort(
        &self,
        request: Request<AbortRequest>,
    ) -> Result<Response<AbortResponse>, Status> {
        let service = request.into_inner().service;
        self.note(format!("abort {service}"));
        installed(&service)?;
        Ok(Response::new(self.abort.clone().unwrap_or_default()))
    }

    async fn clear_record(
        &self,
        request: Request<ClearRecordRequest>,
    ) -> Result<Response<ClearRecordResponse>, Status> {
        let service = request.into_inner().service;
        self.note(format!("clear_record {service}"));
        installed(&service)?;
        Ok(Response::new(ClearRecordResponse {
            problem: RecordProblem::Unreadable.into(),
        }))
    }
}

/// `recording` served on a socket in a new scratch directory; the socket's path.
fn serve_recording(name: &str, recording: &Arc<Recording>) -> (Scratch, PathBuf, Served) {
    let scratch = Scratch::new(name);
    let socket = scratch.path().join("a.sock");
    let recording = Arc::clone(recording);
    let served = on_socket(&socket, move |incoming, stopped| async move {
        let stop = async move {
            let _ = stopped.await;
        };
        tonic::transport::Server::builder()
            .serve_with_incoming_shutdown(AdminServer::new(Recorder(recording)), incoming, stop)
            .await
            .expect("the recording server");
    });
    (scratch, socket, served)
}

fn calls(recording: &Recording) -> Vec<String> {
    recording.calls.lock().expect("the calls").clone()
}

fn halted() -> StepRecord {
    StepRecord {
        order_id: "o-7".to_owned(),
        order_try: 2,
        version: "2.1.0".to_owned(),
        step: "cutover".to_owned(),
        outcome: StepOutcome::Failed.into(),
        phase: FailurePhase::Halt.into(),
        reason: "aborted_by_operator".to_owned(),
        ..StepRecord::default()
    }
}

/// Catches a service's line dropped, taken from the wrong field, or printed in another order:
/// a service that runs healthy on its version with no step running and its last order
/// succeeded, and one being installed whose record does not read and that was not observed,
/// with a step under way, print exactly these lines, after the enrollment and the link.
#[test]
fn status_prints_each_service_in_its_lines() {
    let recording = Arc::new(Recording {
        status: StatusResponse {
            services: vec![
                ServiceRecord {
                    service: "api".to_owned(),
                    current: None,
                    last: Some(StepRecord {
                        order_id: String::new(),
                        order_try: 1,
                        version: "1.4.0".to_owned(),
                        step: "verify".to_owned(),
                        outcome: StepOutcome::Succeeded.into(),
                        forced: true,
                        ..StepRecord::default()
                    }),
                    state: Some(ServiceState {
                        liveness: Liveness::Live.into(),
                        version: Some("1.4.0".to_owned()),
                        healthy: true,
                        unhealthy: Unhealthy::Unspecified.into(),
                        unit: String::new(),
                    }),
                    record_problem: RecordProblem::Unspecified.into(),
                    lifecycle: Lifecycle::Running.into(),
                },
                ServiceRecord {
                    service: "db.main".to_owned(),
                    current: Some(StepRecord {
                        order_try: 3,
                        version: "9.0.0".to_owned(),
                        step: "prepare".to_owned(),
                        ..StepRecord::default()
                    }),
                    last: None,
                    state: None,
                    record_problem: RecordProblem::Unreadable.into(),
                    lifecycle: Lifecycle::Installing.into(),
                },
            ],
            enrollment: Some(WireEnrollment {
                state: EnrollmentState::Pending.into(),
                key_sha256: KEY.to_owned(),
                ..WireEnrollment::default()
            }),
            link: Some(WireLink {
                last_success_unix: Some(BEAT),
                consecutive_failures: 0,
            }),
            trusted_root_age_secs: Some(91 * 86_400),
            trusted_root_reason: "trusted_root_stale".to_owned(),
        },
        ..Recording::default()
    });
    let (_scratch, socket, _served) = serve_recording("acli-svc", &recording);
    let ran = run(&socket, AdminVerb::Status);
    let expected = format!(
        "enrollment: pending
enrollment.key_sha256: {KEY}
link.last_success_unix: {BEAT}
link.consecutive_failures: 0
trusted_root.age_secs: 7862400
trusted_root.reason: trusted_root_stale
services: 2
service[api].lifecycle: running
service[api].record_problem: none
service[api].liveness: live
service[api].version: 1.4.0
service[api].healthy: true
service[api].unhealthy: none
service[api].current: none
service[api].last.order_id: -
service[api].last.order_try: 1
service[api].last.version: 1.4.0
service[api].last.step: verify
service[api].last.outcome: succeeded
service[api].last.phase: none
service[api].last.class: none
service[api].last.reason: -
service[api].last.forced: true
service[db.main].lifecycle: installing
service[db.main].record_problem: unreadable
service[db.main].liveness: unknown
service[db.main].current.order_id: -
service[db.main].current.order_try: 3
service[db.main].current.version: 9.0.0
service[db.main].current.step: prepare
service[db.main].current.outcome: none
service[db.main].current.phase: none
service[db.main].current.class: none
service[db.main].current.reason: -
service[db.main].current.forced: false
service[db.main].last: none
"
    );
    assert_eq!(
        ran,
        Ran {
            code: exit::SUCCESS,
            out: expected,
            err: String::new(),
        }
    );
    assert_eq!(calls(&recording), ["status"]);
}

/// Catches an abort sent for another service, or its record printed from the wrong fields: the
/// server is asked to abort `web`, and the client prints exactly the record it answered; an
/// answer with no record prints `record: none`.
#[test]
fn abort_sends_the_service_and_prints_the_record() {
    let recording = Arc::new(Recording {
        abort: Some(AbortResponse {
            record: Some(halted()),
        }),
        ..Recording::default()
    });
    let (_scratch, socket, _served) = serve_recording("acli-abort", &recording);
    let ran = run(
        &socket,
        AdminVerb::Abort {
            service: "web".to_owned(),
        },
    );
    assert_eq!(
        ran,
        Ran {
            code: exit::SUCCESS,
            out: "aborted: web
record.order_id: o-7
record.order_try: 2
record.version: 2.1.0
record.step: cutover
record.outcome: failed
record.phase: halt
record.class: none
record.reason: aborted_by_operator
record.forced: false
"
            .to_owned(),
            err: String::new(),
        }
    );
    assert_eq!(calls(&recording), ["abort web"]);

    let bare = Arc::new(Recording::default());
    let (_scratch, socket, _served) = serve_recording("acli-abort-bare", &bare);
    let ran = run(
        &socket,
        AdminVerb::Abort {
            service: "web".to_owned(),
        },
    );
    assert_eq!(ran.out, "aborted: web\nrecord: none\n");
}

/// Catches a clear sent for another service, or its answer not printed: the server is asked to
/// clear the record of `web`, and the client prints what it was cleared for.
#[test]
fn clear_record_sends_the_service_and_prints_the_problem() {
    let recording = Arc::new(Recording::default());
    let (_scratch, socket, _served) = serve_recording("acli-clear", &recording);
    let ran = run(
        &socket,
        AdminVerb::ClearRecord {
            service: "web".to_owned(),
        },
    );
    assert_eq!(
        ran,
        Ran {
            code: exit::SUCCESS,
            out: "cleared: web\nproblem: unreadable\n".to_owned(),
            err: String::new(),
        }
    );
    assert_eq!(calls(&recording), ["clear_record web"]);
}

/// Catches a refusal printed as an answer, exiting 0, or without the daemon's token: `abort`
/// and `clear-record` of a service that is not installed, answered `NOT_FOUND`
/// `not_installed`, print nothing on standard output, exactly the token and the code on
/// standard error, and exit with a failure.
#[test]
fn a_service_not_installed_exits_nonzero_with_the_message_on_stderr() {
    let recording = Arc::new(Recording::default());
    let (_scratch, socket, _served) = serve_recording("acli-notfound", &recording);
    for verb in [
        AdminVerb::Abort {
            service: "gone".to_owned(),
        },
        AdminVerb::ClearRecord {
            service: "gone".to_owned(),
        },
    ] {
        assert_eq!(
            run(&socket, verb),
            Ran {
                code: exit::FAILURE,
                out: String::new(),
                err: "hostkeeper admin: not_installed (NotFound)\n".to_owned(),
            }
        );
    }
    assert_eq!(calls(&recording), ["abort gone", "clear_record gone"]);
    assert_ne!(exit::FAILURE, exit::SUCCESS);
}

/// Catches a socket that is not there reported as an answer, or without the path an operator
/// typed: the call exits with a failure, names the socket and why it cannot be reached.
#[test]
fn a_socket_that_is_not_there_exits_with_a_failure_naming_it() {
    let scratch = Scratch::new("acli-missing");
    let socket = scratch.path().join("a.sock");
    let ran = run(&socket, AdminVerb::Status);
    assert_eq!(ran.code, exit::FAILURE);
    assert_eq!(ran.out, "");
    let prefix = format!("hostkeeper admin: cannot connect to {}: ", socket.display());
    assert!(ran.err.starts_with(&prefix), "{}", ran.err);
    assert!(
        ran.err
            .ends_with("No such file or directory (os error 2)\n"),
        "{}",
        ran.err
    );
}

/// A stream that refuses every write.
struct Refusing;

impl Write for Refusing {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Catches an answer, or a failure, reported as printed when its stream refused it: an answer
/// standard output refuses, and a failure standard error refuses, each exit with a failure.
#[test]
fn a_stream_that_refuses_the_text_exits_with_a_failure() {
    let recording = Arc::new(Recording::default());
    let (scratch, socket, _served) = serve_recording("acli-refusing", &recording);
    let command = AdminCommand {
        admin_socket: socket,
        verb: AdminVerb::Status,
    };
    let mut err = Vec::new();
    assert_eq!(
        admin_client::run(&command, &mut Refusing, &mut err),
        exit::FAILURE
    );
    assert!(err.is_empty());
    assert_eq!(calls(&recording), ["status"]);

    let command = AdminCommand {
        admin_socket: scratch.path().join("missing.sock"),
        verb: AdminVerb::Status,
    };
    let mut out = Vec::new();
    assert_eq!(
        admin_client::run(&command, &mut out, &mut Refusing),
        exit::FAILURE
    );
    assert!(out.is_empty());
}

/// Catches a verb that does not parse, a socket flag that is not read on either side of the
/// verb, a default socket that is not the daemon's, and `abort` or `clear-record` taken without
/// a service: each verb parses to its variant, `--admin-socket` is read after the verb and before
/// it, the default is the path the daemon binds, and a missing service is a usage error.
#[test]
fn the_admin_command_line_parses_each_verb_and_refuses_a_missing_service() {
    let parsed = |line: &[&str]| match Cli::try_parse_from(line).expect("parses").command {
        Command::Admin(command) => command,
        other => panic!("not admin: {other:?}"),
    };
    assert_eq!(
        parsed(&["hostkeeper", "admin", "status", "--admin-socket", "X"]),
        AdminCommand {
            admin_socket: PathBuf::from("X"),
            verb: AdminVerb::Status,
        }
    );
    assert_eq!(
        parsed(&["hostkeeper", "admin", "--admin-socket", "Y", "abort", "web"]),
        AdminCommand {
            admin_socket: PathBuf::from("Y"),
            verb: AdminVerb::Abort {
                service: "web".to_owned(),
            },
        }
    );
    assert_eq!(
        parsed(&["hostkeeper", "admin", "clear-record", "web"]),
        AdminCommand {
            admin_socket: PathBuf::from(DEFAULT_ADMIN_SOCKET),
            verb: AdminVerb::ClearRecord {
                service: "web".to_owned(),
            },
        }
    );
    let Command::Daemon(flags) = Cli::parse_from(["hostkeeper", "daemon"]).command else {
        panic!("daemon");
    };
    assert_eq!(flags.admin_socket, PathBuf::from(DEFAULT_ADMIN_SOCKET));

    for line in [
        &["hostkeeper", "admin", "abort"][..],
        &["hostkeeper", "admin", "clear-record"][..],
        &["hostkeeper", "admin"][..],
    ] {
        let refused = Cli::try_parse_from(line).expect_err("a usage error");
        assert_eq!(refused.exit_code(), 2, "{line:?}");
        assert_ne!(refused.kind(), ErrorKind::DisplayHelp, "{line:?}");
    }
    let refused = Cli::try_parse_from(["hostkeeper", "admin", "abort"]).expect_err("usage");
    assert_eq!(refused.kind(), ErrorKind::MissingRequiredArgument);
}

/// Catches `hostkeeper admin` not dispatched by the binary's `run`, or its exit code lost: a
/// status of a socket that is not there exits with the failure code.
#[test]
fn run_dispatches_admin_and_returns_its_exit_code() {
    let scratch = Scratch::new("acli-run");
    let socket = scratch.path().join("a.sock");
    let socket = socket.to_str().expect("UTF-8");
    let ran = cli::run(Cli::parse_from([
        "hostkeeper",
        "admin",
        "status",
        "--admin-socket",
        socket,
    ]));
    assert_eq!(
        format!("{ran:?}"),
        format!("{:?}", ExitCode::from(exit::FAILURE))
    );
}
