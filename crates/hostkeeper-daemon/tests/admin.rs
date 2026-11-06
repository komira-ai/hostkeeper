//! The admin service over in-process pipes that carry an explicit peer: who is served, what
//! `Status` reports, which calls the socket has, and how `Abort` and `ClearRecord` refuse.

mod support;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use hostkeeper_config::names::ServiceName;
use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{CtlTransport, PeerIdentity};
use hostkeeper_daemon::admin::{self, Peer, step_record, verdict_into};
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::halt::HaltStore;
use hostkeeper_daemon::locks::Package;
use hostkeeper_daemon::packages::{PackageError, Packages, VersionDir};
use hostkeeper_daemon::testing::{Counted, spawn};
use hostkeeper_daemon::trusted_root::{RootProblem, RootReport};
use hostkeeper_daemon::verb::{HookedVerb, PackageDigest};
use hostkeeper_daemon::{Daemon, Dirs};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_observe::{HealthView, Observation, ServiceState as Observed, VersionView};
use hostkeeper_proto::FILE_DESCRIPTOR_SET;
use hostkeeper_proto::hostkeeper::agent::v1::{FailureClass, FailurePhase};
use hostkeeper_proto::hostkeeper::local::v1::{
    AbortRequest, ClearRecordRequest, Liveness, RecordProblem, ServiceState, StatusRequest,
    StepOutcome, StepRecord, Unhealthy,
};
use hostkeeper_steps::record::RECORD_FILE;
use prost::Message;
use prost_types::FileDescriptorSet;
use support::{OWN, Rig, client, config, d, snapshot, update};
use tokio::io::duplex;
use tokio_stream::StreamExt;
use tonic::Code;
use tonic::metadata::MetadataMap;

/// Serves `daemon` on one in-process connection from `peer`; the client's end, and the count
/// of the bytes the server read.
fn connect(
    daemon: Arc<Daemon>,
    peer: Peer,
) -> (
    hostkeeper_proto::hostkeeper::local::v1::admin_client::AdminClient<tonic::transport::Channel>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let (near, far) = duplex(64 * 1024);
    let (counted, read) = Counted::new(far);
    let incoming = tokio_stream::iter([Ok((counted, peer))]).chain(tokio_stream::pending());
    tokio::spawn(admin::serve(daemon, incoming));
    (client(near), read)
}

/// Catches a peer check made after the request is decoded, or not at all: a connection from
/// uid 1000 and one whose credentials are unknown are closed with nothing read from them, and
/// uid 0 is served.
#[tokio::test]
async fn admin_refuses_every_peer_but_uid_zero_before_decoding() {
    let rig = Rig::new("admin-peer");
    let daemon = Arc::new(rig.daemon());
    for peer in [Peer { uid: Some(1000) }, Peer { uid: None }] {
        let (mut client, read) = connect(Arc::clone(&daemon), peer);
        let refused = client.status(StatusRequest {}).await;
        assert!(refused.is_err(), "{peer:?} was served");
        assert_eq!(read.load(Ordering::SeqCst), 0, "{peer:?}");
    }
    let (mut client, read) = connect(daemon, Peer { uid: Some(0) });
    let served = client
        .status(StatusRequest {})
        .await
        .expect("root is served");
    assert!(served.into_inner().services.is_empty());
    assert!(read.load(Ordering::SeqCst) > 0);
}

/// Catches a field of `Status` dropped or read from the wrong service: a service whose order
/// ended, one with a step running, one halted with a held generation, one whose record does not
/// read and one a newer daemon wrote, each with what it holds, in the library's report and on
/// the wire.
#[tokio::test(flavor = "multi_thread")]
async fn status_reports_running_version_last_result_and_held_halt() {
    let rig = Rig::new("admin-status");
    rig.running("done", "2.0.0");
    rig.add("done", "2.1.0");
    rig.add("busy", "2.0.0");
    rig.add("halted", "2.0.0");
    rig.add("idle", "1.0.0");
    rig.store.link("idle", "current", ".hidden");
    rig.launcher.exits("halted", Step::Prepare, &[76]);
    let gate = rig.launcher.hold("busy", Step::Cutover);
    let daemon = Arc::new(rig.daemon());
    assert!(matches!(
        daemon.order(&update("done", "2.1.0", &d(), 3)),
        hostkeeper_daemon::Reply::Finished { .. }
    ));
    daemon.order(&update("halted", "2.0.0", &d(), 1));
    HaltStore::new(&rig.state)
        .raise(&ServiceName::new("halted").expect("a name"), 3)
        .expect("raise");
    for (service, text) in [("broken", "{"), ("newer", r#"{"schema": 9}"#)] {
        fs::create_dir_all(rig.state.join(service)).expect("dir");
        fs::write(rig.state.join(service).join(RECORD_FILE), text).expect("record");
    }
    let running = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || {
            daemon.order(&update("busy", "2.0.0", &d(), 1))
        })
    };
    gate.entered(&running).expect("the held step is entered");

    let status = daemon.status().expect("status");
    assert_eq!(status.version, OWN);
    let names: Vec<_> = status
        .services
        .iter()
        .map(|s| s.service.to_string())
        .collect();
    assert_eq!(names, ["broken", "busy", "done", "halted", "idle", "newer"]);
    let [broken, busy, done, halted, idle, newer] = &status.services[..] else {
        panic!("six services");
    };
    assert_eq!(idle.record, RecordState::Absent);
    assert_eq!(
        (idle.running_version.clone(), idle.step_running()),
        (None, false)
    );
    assert_eq!(
        done.running_version.as_ref().map(|v| v.as_str()),
        Some("2.1.0")
    );
    assert_eq!(
        done.last_result().map(|f| f.verdict),
        Some(Verdict::Succeeded {
            promotion_pending: false
        })
    );
    assert_eq!((done.held_halt, done.step_running()), (Some(0), false));
    assert!(busy.step_running() & busy.locked);
    assert_eq!(busy.last_result(), None);
    assert_eq!(
        busy.running_version.as_ref().map(|v| v.as_str()),
        Some("2.0.0"),
        "the switch to the target comes before the cutover that is held"
    );
    assert_eq!(halted.last_result().map(|f| f.verdict), Some(Verdict::Halt));
    assert_eq!(halted.held_halt, Some(3));
    assert!(matches!(broken.record, RecordState::Unreadable(_)));
    assert!(!broken.step_running());
    assert_eq!(newer.record, RecordState::Newer(9));
    assert_eq!(
        done.observation,
        Some(Observation {
            service: "done".to_owned(),
            state: Observed::Unmonitored,
            version: VersionView::NotLive,
            health: HealthView::NotProbed,
            recorded: Some("2.1.0".to_owned()),
        })
    );
    for unobserved in [idle, broken, newer] {
        assert_eq!(unobserved.observation, None, "{}", unobserved.service);
    }

    let (mut client, _) = connect(Arc::clone(&daemon), Peer { uid: Some(0) });
    let wire = client
        .status(StatusRequest {})
        .await
        .expect("status")
        .into_inner();
    let by_name: Vec<_> = wire.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(by_name, names);
    let [broken, busy, done, halted, idle, newer] = &wire.services[..] else {
        panic!("six services");
    };
    assert!(idle.current.is_none() & idle.last.is_none());
    assert_eq!(idle.record_problem, RecordProblem::Unspecified as i32);
    assert_eq!(broken.record_problem, RecordProblem::Unreadable as i32);
    assert_eq!(newer.record_problem, RecordProblem::NewerSchema as i32);
    let current = busy.current.as_ref().expect("the running order");
    assert_eq!(
        (
            current.step.as_str(),
            current.attempt,
            current.order_try,
            current.version.as_str()
        ),
        ("cutover", 1, 1, "2.0.0")
    );
    assert_eq!(current.outcome, StepOutcome::Unspecified as i32);
    assert!(busy.last.is_none());
    let last = done.last.as_ref().expect("the last order");
    assert_eq!(
        (
            last.outcome,
            last.order_try,
            last.version.as_str(),
            last.package_digest.clone()
        ),
        (StepOutcome::Succeeded as i32, 3, "2.1.0", d())
    );
    let halt = halted.last.as_ref().expect("the halt");
    assert_eq!(
        (halt.outcome, halt.phase),
        (StepOutcome::Failed as i32, FailurePhase::Halt as i32)
    );
    assert_eq!(done.record_problem, RecordProblem::Unspecified as i32);
    assert_eq!(
        done.state,
        Some(ServiceState {
            liveness: Liveness::Unmonitored as i32,
            version: None,
            healthy: false,
            unhealthy: Unhealthy::Unspecified as i32,
            unit: String::new(),
        })
    );
    for unobserved in [idle, broken, newer] {
        assert_eq!(unobserved.state, None, "{}", unobserved.service);
    }

    gate.release();
    running.join().expect("the running order");
}

/// Catches a verb reachable that design section 4.1 keeps off the local socket: the admin
/// service's calls in the protobuf descriptors are exactly `Status`, `Abort` and `ClearRecord`,
/// the server answers `Status` by its path, and `ClearHalt` and `Update` by that service's path
/// answer `UNIMPLEMENTED`, with nothing read or written.
#[test]
fn the_admin_socket_serves_exactly_status_abort_and_clear_record() {
    let set = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).expect("the descriptors");
    let methods: Vec<&str> = set
        .file
        .iter()
        .filter(|file| file.package() == "hostkeeper.local.v1")
        .flat_map(|file| &file.service)
        .filter(|service| service.name() == "Admin")
        .flat_map(|service| &service.method)
        .map(|method| method.name())
        .collect();
    assert_eq!(methods, ["Status", "Abort", "ClearRecord"]);

    let rig = Rig::new("admin-table");
    rig.add("web", "2.0.0");
    let daemon = Arc::new(rig.daemon());
    let transport = InMemory::new(PeerIdentity::Anonymous).serve(admin::server(daemon));
    let metadata = MetadataMap::new();
    let status = transport.call("/hostkeeper.local.v1.Admin/Status", Bytes::new(), &metadata);
    assert!(status.is_ok(), "{status:?}");
    let before = snapshot(rig.scratch.path());
    for method in ["ClearHalt", "Update"] {
        let path = format!("/hostkeeper.local.v1.Admin/{method}");
        let refused = transport
            .call(&path, Bytes::new(), &metadata)
            .expect_err(method);
        assert_eq!(refused.code(), Code::Unimplemented, "{method}");
    }
    assert_eq!(snapshot(rig.scratch.path()), before);
    assert!(rig.launcher.runs().is_empty());
}

/// Catches a peer check made after the request is decoded for the calls that change things:
/// `Abort` and `ClearRecord` from uid 1000 and from a peer whose credentials are unknown are
/// refused with nothing read from the connection, and nothing written.
#[tokio::test]
async fn admin_abort_and_clear_record_refuse_every_peer_but_uid_zero_before_decoding() {
    let rig = Rig::new("admin-peer-calls");
    let daemon = Arc::new(rig.daemon());
    fs::create_dir_all(rig.state.join("web")).expect("the service's directory");
    fs::write(rig.state.join("web").join(RECORD_FILE), "{").expect("a corrupt record");
    let before = snapshot(&rig.state);
    for peer in [Peer { uid: Some(1000) }, Peer { uid: None }] {
        let (mut client, read) = connect(Arc::clone(&daemon), peer);
        let abort = client
            .abort(AbortRequest {
                service: "web".to_owned(),
            })
            .await;
        assert!(abort.is_err(), "{peer:?} was served an abort");
        assert_eq!(read.load(Ordering::SeqCst), 0, "{peer:?}");
        let (mut client, read) = connect(Arc::clone(&daemon), peer);
        let clear = client
            .clear_record(ClearRecordRequest {
                service: "web".to_owned(),
            })
            .await;
        assert!(clear.is_err(), "{peer:?} was served a clear_record");
        assert_eq!(read.load(Ordering::SeqCst), 0, "{peer:?}");
    }
    assert_eq!(snapshot(&rig.state), before);
}

/// Catches a refusal on the admin socket that is not one token with its code: a service name
/// that breaks its rule is `INVALID_ARGUMENT` `invalid_request`; an abort with nothing running is
/// `FAILED_PRECONDITION` `nothing_running`; a record that reads is `FAILED_PRECONDITION`
/// `record_reads`; a held lock is `FAILED_PRECONDITION` `busy`; a record that cannot be read is
/// `INTERNAL` `store_failed`; and a corrupt record is cleared, answered `RECORD_PROBLEM_UNREADABLE`.
#[tokio::test(flavor = "multi_thread")]
async fn admin_abort_and_clear_record_answer_each_refusal_with_its_token() {
    let rig = Rig::new("admin-tokens");
    rig.add("web", "2.0.0");
    let daemon = Arc::new(rig.daemon());
    let (mut client, _) = connect(Arc::clone(&daemon), Peer { uid: Some(0) });
    let abort = |service: &str| AbortRequest {
        service: service.to_owned(),
    };
    let clear = |service: &str| ClearRecordRequest {
        service: service.to_owned(),
    };
    let code_of = |status: tonic::Status| (status.code(), status.message().to_owned());
    let token = |code: Code, token: &str| (code, token.to_owned());

    let bad = client.abort(abort("a b")).await.expect_err("a bad name");
    assert_eq!(
        code_of(bad),
        token(Code::InvalidArgument, "invalid_request")
    );
    let bad = client
        .clear_record(clear("a b"))
        .await
        .expect_err("a bad name");
    assert_eq!(
        code_of(bad),
        token(Code::InvalidArgument, "invalid_request")
    );
    let idle = client.abort(abort("web")).await.expect_err("idle");
    assert_eq!(
        code_of(idle),
        token(Code::FailedPrecondition, "nothing_running")
    );
    let absent = client.clear_record(clear("web")).await.expect_err("absent");
    assert_eq!(
        code_of(absent),
        token(Code::FailedPrecondition, "record_reads")
    );

    let dir = rig.state.join("web");
    fs::create_dir_all(&dir).expect("the service's directory");
    fs::write(dir.join(RECORD_FILE), "{").expect("a corrupt record");
    let package = Package {
        verb: HookedVerb::Update,
        version: hostkeeper_config::names::Version::new("2.0.0").expect("a version"),
        digest: PackageDigest::new(&d()).expect("a digest"),
    };
    let web = ServiceName::new("web").expect("a name");
    let guard = daemon.ctx().locks.take(&web, package).expect("free");
    let held = client.clear_record(clear("web")).await.expect_err("held");
    assert_eq!(code_of(held), token(Code::FailedPrecondition, "busy"));
    drop(guard);
    let cleared = client
        .clear_record(clear("web"))
        .await
        .expect("cleared")
        .into_inner();
    assert_eq!(cleared.problem, RecordProblem::Unreadable as i32);

    fs::create_dir(dir.join(RECORD_FILE)).expect("a directory in the record's place");
    let unread = client.clear_record(clear("web")).await.expect_err("unread");
    assert_eq!(code_of(unread), token(Code::Internal, "store_failed"));
}

/// Catches a status that cannot list the services reported as an empty host: it is `INTERNAL`
/// with the token `store_failed`.
#[tokio::test]
async fn a_status_that_cannot_list_the_services_is_store_failed() {
    let rig = Rig::new("admin-fail");
    let file = rig.scratch.path().join("not-a-dir");
    fs::write(&file, "x").expect("a file");
    let dirs = Dirs {
        state: rig.state.clone(),
        packages: file,
    };
    let daemon = Daemon::new(config(), dirs, rig.ports(), OWN);
    assert!(daemon.status().is_err());
    let (mut client, _) = connect(Arc::new(daemon), Peer { uid: Some(0) });
    let status = client.status(StatusRequest {}).await.expect_err("failed");
    assert_eq!(
        (status.code(), status.message()),
        (Code::Internal, "store_failed")
    );
}

/// Catches a verdict written into the wrong fields of a step record: every verdict, phase and
/// class.
#[test]
fn every_verdict_reaches_the_wire_in_its_fields() {
    let fields = |verdict: Verdict| {
        let mut out = StepRecord::default();
        verdict_into(&verdict, &mut out);
        (
            out.outcome,
            out.phase,
            out.class,
            out.retry_after_secs,
            out.promotion_pending,
        )
    };
    let (none_phase, none_class) = (
        FailurePhase::Unspecified as i32,
        FailureClass::Unspecified as i32,
    );
    assert_eq!(
        fields(Verdict::Succeeded {
            promotion_pending: true
        }),
        (
            StepOutcome::Succeeded as i32,
            none_phase,
            none_class,
            0,
            true
        )
    );
    assert_eq!(
        fields(Verdict::Deferred {
            retry_after_secs: Some(40)
        }),
        (
            StepOutcome::Deferred as i32,
            none_phase,
            none_class,
            40,
            false
        )
    );
    assert_eq!(
        fields(Verdict::Deferred {
            retry_after_secs: None
        }),
        (
            StepOutcome::Deferred as i32,
            none_phase,
            none_class,
            0,
            false
        )
    );
    assert_eq!(
        fields(Verdict::Halt),
        (
            StepOutcome::Failed as i32,
            FailurePhase::Halt as i32,
            none_class,
            0,
            false
        )
    );
    let phases = [
        (Phase::BeforeCommit, FailurePhase::BeforeCommit),
        (Phase::GateRelease, FailurePhase::GateRelease),
        (Phase::GateEnvironment, FailurePhase::GateEnvironment),
        (Phase::AfterCommit, FailurePhase::AfterCommit),
        (Phase::Halt, FailurePhase::Halt),
    ];
    let classes = [
        (Class::Release, FailureClass::Release),
        (Class::Environment, FailureClass::Environment),
    ];
    for (phase, wire_phase) in phases {
        for (class, wire_class) in classes {
            assert_eq!(
                fields(Verdict::Failed { phase, class }),
                (
                    StepOutcome::Failed as i32,
                    wire_phase as i32,
                    wire_class as i32,
                    0,
                    false
                )
            );
        }
    }
}

/// Catches a finished record whose reason is lost on the wire, or an unfinished one reported
/// with an outcome.
#[test]
fn a_step_record_carries_its_reason_and_no_outcome_while_it_runs() {
    let rig = Rig::new("admin-record");
    rig.add("web", "2.0.0");
    rig.add("web", "2.1.0");
    rig.launcher.lose("web", Step::Verify);
    let daemon = rig.daemon();
    daemon.order(&update("web", "2.0.0", &d(), 2));
    let status = daemon.status().expect("status");
    let RecordState::Unfinished(rec) = &status.services[0].record else {
        panic!("an unfinished record");
    };
    let wire = step_record(rec);
    assert_eq!(
        (wire.step.as_str(), wire.outcome, wire.reason.as_str()),
        ("verify", 0, "")
    );
    let mut finished = (**rec).clone();
    finished.state = hostkeeper_steps::record::State::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(5),
        },
        reason: Some("drain_bound".to_owned()),
    };
    let wire = step_record(&finished);
    assert_eq!(
        (wire.outcome, wire.reason.as_str(), wire.retry_after_secs),
        (StepOutcome::Deferred as i32, "drain_bound", 5)
    );
}

/// Catches an accept error that stops the server, or is swallowed by the peer check: it is
/// passed on to the server, which goes on to serve the next connection.
#[tokio::test]
async fn an_accept_error_is_passed_on_and_the_next_connection_served() {
    let rig = Rig::new("admin-accept");
    let (near, far) = duplex(64 * 1024);
    let (counted, _) = Counted::new(far);
    let failed = std::io::Error::from(std::io::ErrorKind::ConnectionAborted);
    let incoming = tokio_stream::iter([Err(failed), Ok((counted, Peer { uid: Some(0) }))])
        .chain(tokio_stream::pending());
    tokio::spawn(admin::serve(Arc::new(rig.daemon()), incoming));
    let served = client(near).status(StatusRequest {}).await.expect("served");
    assert!(served.into_inner().services.is_empty());
}

/// Catches a status error that does not name the directory it could not list.
#[test]
fn a_status_error_names_the_directory() {
    let rig = Rig::new("admin-err");
    let file = rig.scratch.path().join("plain");
    fs::write(&file, "x").expect("a file");
    let dirs = Dirs {
        state: file.clone(),
        packages: rig.packages.clone(),
    };
    let error = Daemon::new(config(), dirs, rig.ports(), OWN)
        .status()
        .expect_err("a file");
    let text = error.to_string();
    assert!(
        text.starts_with(&format!(
            "cannot list the services under {}: ",
            file.display()
        )),
        "{text}"
    );
}

/// A package store that reports `report` as its trusted root, and otherwise is `inner`.
struct ReportingRoot {
    inner: Arc<hostkeeper_daemon::testing::FakePackages>,
    report: Option<RootReport>,
}

impl Packages for ReportingRoot {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &hostkeeper_config::names::Version,
        digest: &PackageDigest,
        index_digest: Option<&PackageDigest>,
    ) -> Result<VersionDir, PackageError> {
        self.inner
            .version_dir(service, version, digest, index_digest)
    }

    fn trusted_root(&self) -> Option<RootReport> {
        self.report.clone()
    }
}

/// Catches the trusted root's age or reason dropped between the store and the wire, a reason
/// sent under another token, or one sent for a root that needs no attention: the library's
/// status holds the store's report, and `Status` carries its age and its reason token, the age
/// absent for a missing root and the reason empty for a fine one or a store that reports none.
#[tokio::test]
async fn status_carries_the_trusted_roots_age_and_reason() {
    let rig = Rig::new("admin-trusted-root");
    let path = rig.scratch.path().join("trusted_root.json");
    let cases = [
        (None, None, ""),
        (Some(3600), None, ""),
        (
            Some(91 * 86_400),
            Some(RootProblem::Stale),
            "trusted_root_stale",
        ),
        (None, Some(RootProblem::Missing), "trusted_root_missing"),
        (
            Some(60),
            Some(RootProblem::Unparseable),
            "trusted_root_unparseable",
        ),
    ];
    for (age_secs, problem, token) in cases {
        let report = RootReport {
            path: path.clone(),
            age_secs,
            problem,
        };
        let packages = ReportingRoot {
            inner: Arc::clone(&rig.store),
            report: Some(report.clone()),
        };
        let ports = hostkeeper_daemon::Ports {
            packages: Arc::new(packages),
            ..rig.ports()
        };
        let daemon = Arc::new(Daemon::new(config(), rig.dirs(), ports, OWN));
        assert_eq!(
            daemon.status().expect("the status").trusted_root,
            Some(report)
        );
        let (mut client, _) = connect(daemon, Peer { uid: Some(0) });
        let status = client
            .status(StatusRequest {})
            .await
            .expect("the status")
            .into_inner();
        assert_eq!(
            (
                status.trusted_root_age_secs,
                status.trusted_root_reason.as_str()
            ),
            (age_secs, token),
            "{problem:?}"
        );
    }
    let daemon = Arc::new(rig.daemon());
    assert_eq!(daemon.status().expect("the status").trusted_root, None);
    let (mut client, _) = connect(daemon, Peer { uid: Some(0) });
    let status = client
        .status(StatusRequest {})
        .await
        .expect("the status")
        .into_inner();
    assert_eq!(
        (status.trusted_root_age_secs, status.trusted_root_reason),
        (None, String::new())
    );
}
