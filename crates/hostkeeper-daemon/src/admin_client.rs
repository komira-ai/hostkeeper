//! `hostkeeper admin`: the client of the daemon's root-only admin socket on the host
//! (`hostkeeper.local.v1.Admin`, [`crate::admin`]), one verb per call (docs/admin.md):
//!
//! - `status`: `Status`, what the daemon holds: the host's enrollment, the heartbeat loop's link
//!   to the coordinator, the trusted root's age, and each installed service;
//! - `abort <service>`: `Abort`, the service's running step stopped;
//! - `clear-record <service>`: `ClearRecord`, the service's unreadable step record cleared.
//!
//! The socket is `--admin-socket`, by default the path the daemon binds
//! ([`crate::cli::DEFAULT_ADMIN_SOCKET`]); only root may connect. A verb's answer goes to standard
//! output as lines of `key: value` ([`status_text`], [`abort_text`], [`clear_record_text`]), and
//! the exit status is 0. A call the daemon refused, or that could not be made, writes one line to
//! standard error, `hostkeeper admin: <message> (<code>)` for a gRPC status (its message is the
//! refusal's token) and `hostkeeper admin: cannot connect to <socket>: <why>` for a socket that
//! could not be reached, and exits with the code [`exit::code`] gives the gRPC code
//! ([`exit::FAILURE`] for a socket that could not be reached).
//!
//! In the text, an enum value is its name without its prefix, in lower case (`running`); a value
//! this client does not know is `unknown_<number>`. A string field that is empty is `-`. A
//! service's keys are under `service[<name>]`, which no service name can be mistaken for: a
//! name has no `[`, `]`, `:` or space.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use hostkeeper_proto::hostkeeper::agent::v1::{FailureClass, FailurePhase};
use hostkeeper_proto::hostkeeper::local::v1::admin_client::AdminClient;
use hostkeeper_proto::hostkeeper::local::v1::{
    AbortRequest, AbortResponse, ClearRecordRequest, ClearRecordResponse, EnrollmentState,
    Lifecycle, Liveness, RecordProblem, ServiceRecord, StatusRequest, StatusResponse, StepOutcome,
    StepRecord, Unhealthy,
};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::Status;
use tonic::transport::{Channel, Endpoint, Uri};

use crate::cli::DEFAULT_ADMIN_SOCKET;

/// The exit codes of `hostkeeper admin` (docs/admin.md).
pub mod exit {
    use tonic::Code;

    /// The call succeeded.
    pub const SUCCESS: u8 = 0;
    /// The call failed for any reason the codes below do not name: the socket could not be
    /// reached, or the daemon answered another status.
    pub const FAILURE: u8 = 1;
    /// The daemon refused the request as it was made: `INVALID_ARGUMENT`, `FAILED_PRECONDITION`.
    pub const USAGE: u8 = 2;
    /// The daemon refused the caller: `UNAUTHENTICATED`, `PERMISSION_DENIED`.
    pub const DENIED: u8 = 3;

    /// The exit code of a call that ended with the gRPC status code `code`. Every code is named,
    /// so a code tonic adds is a build error here, not a silent [`FAILURE`].
    pub fn code(code: Code) -> u8 {
        match code {
            Code::Ok => SUCCESS,
            Code::InvalidArgument | Code::FailedPrecondition => USAGE,
            Code::Unauthenticated | Code::PermissionDenied => DENIED,
            Code::Cancelled
            | Code::Unknown
            | Code::DeadlineExceeded
            | Code::NotFound
            | Code::AlreadyExists
            | Code::ResourceExhausted
            | Code::Aborted
            | Code::OutOfRange
            | Code::Unimplemented
            | Code::Internal
            | Code::Unavailable
            | Code::DataLoss => FAILURE,
        }
    }
}

/// The program name the lines on standard error start with.
pub const ADMIN_PROGRAM: &str = "hostkeeper admin";

/// The flags and verb of `hostkeeper admin`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct AdminCommand {
    /// The daemon's root-only admin socket.
    #[arg(long, global = true, default_value = DEFAULT_ADMIN_SOCKET)]
    pub admin_socket: PathBuf,
    #[command(subcommand)]
    pub verb: AdminVerb,
}

/// One call of the admin socket.
#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum AdminVerb {
    /// What the daemon holds: the host's enrollment, its link to the coordinator, and each
    /// installed service. Changes nothing.
    Status,
    /// Stop the service's running step, and end its order in the halt phase.
    Abort {
        /// The service whose step is stopped.
        service: String,
    },
    /// Clear the service's step record when it cannot be read, so that orders run for it again.
    ClearRecord {
        /// The service whose record is cleared.
        service: String,
    },
}

/// Why a verb has no answer.
#[derive(Debug)]
enum Failed {
    /// The async runtime could not be made.
    Runtime(io::Error),
    /// The socket could not be reached.
    Connect(tonic::transport::Error),
    /// The daemon answered with a gRPC status, or the call broke off.
    Call(Status),
}

impl Failed {
    /// The line on standard error, without its newline.
    fn line(&self, socket: &Path) -> String {
        match self {
            Failed::Runtime(error) => {
                format!("{ADMIN_PROGRAM}: the async runtime could not be made: {error}")
            }
            Failed::Connect(error) => format!(
                "{ADMIN_PROGRAM}: cannot connect to {}: {}",
                socket.display(),
                chain(error)
            ),
            Failed::Call(status) => {
                format!(
                    "{ADMIN_PROGRAM}: {} ({:?})",
                    status.message(),
                    status.code()
                )
            }
        }
    }

    fn exit_code(&self) -> u8 {
        match self {
            Failed::Runtime(_) | Failed::Connect(_) => exit::FAILURE,
            Failed::Call(status) => exit::code(status.code()),
        }
    }
}

/// `error` and each error under it, joined by `: `.
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Runs `command`: its answer to `out`, or one line to `err`; the exit code.
pub fn run(command: &AdminCommand, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let answered = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Failed::Runtime)
        .and_then(|runtime| runtime.block_on(call(command)));
    match answered {
        Ok(text) => write(out, &text, exit::SUCCESS),
        Err(failed) => {
            let line = format!("{}\n", failed.line(&command.admin_socket));
            write(err, &line, failed.exit_code())
        }
    }
}

/// `code`, once `text` is written to `stream`; [`exit::FAILURE`] when it could not be.
fn write(stream: &mut dyn Write, text: &str, code: u8) -> u8 {
    stream
        .write_all(text.as_bytes())
        .and_then(|()| stream.flush())
        .map_or(exit::FAILURE, |()| code)
}

/// The verb's call over the socket, and its answer as text.
async fn call(command: &AdminCommand) -> Result<String, Failed> {
    let mut client = connect(&command.admin_socket)
        .await
        .map_err(Failed::Connect)?;
    match &command.verb {
        AdminVerb::Status => client
            .status(StatusRequest {})
            .await
            .map(|answer| status_text(answer.get_ref())),
        AdminVerb::Abort { service } => client
            .abort(AbortRequest {
                service: service.clone(),
            })
            .await
            .map(|answer| abort_text(service, answer.get_ref())),
        AdminVerb::ClearRecord { service } => client
            .clear_record(ClearRecordRequest {
                service: service.clone(),
            })
            .await
            .map(|answer| clear_record_text(service, answer.get_ref())),
    }
    .map_err(Failed::Call)
}

/// A client of the admin socket at `socket`, connected: one connection, made now.
pub async fn connect(socket: &Path) -> Result<AdminClient<Channel>, tonic::transport::Error> {
    let socket = socket.to_path_buf();
    let connector = tower::service_fn(move |_: Uri| {
        let socket = socket.clone();
        async move { UnixStream::connect(socket).await.map(TokioIo::new) }
    });
    // The URI is not dialled: the connector reaches the socket whatever it names.
    let channel = Endpoint::from_static("http://admin.invalid")
        .connect_with_connector(connector)
        .await?;
    Ok(AdminClient::new(channel))
}

/// Lines of `key: value`.
#[derive(Default)]
struct Lines(String);

impl Lines {
    fn push(&mut self, key: &str, value: impl std::fmt::Display) {
        self.0.push_str(&format!("{key}: {value}\n"));
    }

    /// A string field: `-` when it is empty.
    fn text(&mut self, key: &str, value: &str) {
        self.push(key, if value.is_empty() { "-" } else { value });
    }
}

/// An enum value as text: its `name` without `prefix`, in lower case; `unknown_<value>` for a
/// value with no name. `zero`, when given, is the text of the value 0 in place of
/// `unspecified`.
fn token(name: Option<&str>, prefix: &str, value: i32, zero: Option<&str>) -> String {
    match (name, zero) {
        (Some(_), Some(zero)) if value == 0 => zero.to_owned(),
        (Some(name), _) => name
            .strip_prefix(prefix)
            .unwrap_or(name)
            .to_ascii_lowercase(),
        (None, _) => format!("unknown_{value}"),
    }
}

macro_rules! token_of {
    ($enum:ty, $prefix:literal, $value:expr) => {
        token_of!($enum, $prefix, $value, None)
    };
    ($enum:ty, $prefix:literal, $value:expr, $zero:expr) => {{
        let value: i32 = $value;
        let name = <$enum>::try_from(value)
            .ok()
            .map(|known| known.as_str_name());
        token(name, $prefix, value, $zero)
    }};
}

/// The text of `hostkeeper admin status`, in this order:
///
/// - `enrollment`: `unenrolled`, `pending`, `enrolled` or `unreadable`, or `unknown` when the
///   daemon has none yet; then `enrollment.key_sha256` for a pending host, and
///   `enrollment.host_id` and `enrollment.not_after_unix` for an enrolled one;
/// - `link.last_success_unix` (`never` when no heartbeat was answered) and
///   `link.consecutive_failures`, or `link: unknown` when the daemon has none yet;
/// - `trusted_root.age_secs`, the age of the Sigstore trusted root (`unknown` when the daemon
///   reported none), and `trusted_root.reason`, why it needs attention (`none` when it needs
///   none);
/// - `services`, how many; then each service ([`service_lines`]), in the daemon's order.
pub fn status_text(status: &StatusResponse) -> String {
    let mut out = Lines::default();
    match &status.enrollment {
        None => out.push("enrollment", "unknown"),
        Some(enrollment) => {
            let state = token_of!(EnrollmentState, "ENROLLMENT_STATE_", enrollment.state);
            out.push("enrollment", state);
            if !enrollment.key_sha256.is_empty() {
                out.push("enrollment.key_sha256", &enrollment.key_sha256);
            }
            if !enrollment.host_id.is_empty() {
                out.push("enrollment.host_id", &enrollment.host_id);
            }
            if let Some(not_after) = enrollment.not_after_unix {
                out.push("enrollment.not_after_unix", not_after);
            }
        }
    }
    match &status.link {
        None => out.push("link", "unknown"),
        Some(link) => {
            let last = link
                .last_success_unix
                .map_or_else(|| "never".to_owned(), |secs| secs.to_string());
            out.push("link.last_success_unix", last);
            out.push("link.consecutive_failures", link.consecutive_failures);
        }
    }
    let age = status
        .trusted_root_age_secs
        .map_or_else(|| "unknown".to_owned(), |secs| secs.to_string());
    out.push("trusted_root.age_secs", age);
    let reason = status.trusted_root_reason.as_str();
    out.push(
        "trusted_root.reason",
        if reason.is_empty() { "none" } else { reason },
    );
    out.push("services", status.services.len());
    for service in &status.services {
        service_lines(service, &mut out);
    }
    out.0
}

/// One service, every key under `service[<name>]`: `lifecycle`; `record_problem` (`none` when
/// the record reads); `liveness`, `version`, `healthy` and `unhealthy` (`none` when healthy or
/// not live) of how it runs, or `liveness: unknown` when it was not observed; then its `current`
/// and its `last` step record ([`record_lines`]), each `none` when there is none.
fn service_lines(service: &ServiceRecord, out: &mut Lines) {
    let at = format!("service[{}]", service.service);
    out.push(
        &format!("{at}.lifecycle"),
        token_of!(Lifecycle, "LIFECYCLE_", service.lifecycle),
    );
    out.push(
        &format!("{at}.record_problem"),
        token_of!(
            RecordProblem,
            "RECORD_PROBLEM_",
            service.record_problem,
            Some("none")
        ),
    );
    match &service.state {
        None => out.push(&format!("{at}.liveness"), "unknown"),
        Some(state) => {
            out.push(
                &format!("{at}.liveness"),
                token_of!(Liveness, "LIVENESS_", state.liveness),
            );
            out.text(
                &format!("{at}.version"),
                state.version.as_deref().unwrap_or(""),
            );
            out.push(&format!("{at}.healthy"), state.healthy);
            out.push(
                &format!("{at}.unhealthy"),
                token_of!(Unhealthy, "UNHEALTHY_", state.unhealthy, Some("none")),
            );
        }
    }
    for (name, record) in [("current", &service.current), ("last", &service.last)] {
        let key = format!("{at}.{name}");
        match record {
            None => out.push(&key, "none"),
            Some(record) => record_lines(&key, record, out),
        }
    }
}

/// A step record, every key under `at`: `order_id`, `order_try`, `version`, `step`, `outcome`
/// (`none` while it runs), `phase` and `class` (`none` unless it failed with one), `reason` and
/// `forced`.
fn record_lines(at: &str, record: &StepRecord, out: &mut Lines) {
    out.text(&format!("{at}.order_id"), &record.order_id);
    out.push(&format!("{at}.order_try"), record.order_try);
    out.text(&format!("{at}.version"), &record.version);
    out.text(&format!("{at}.step"), &record.step);
    out.push(
        &format!("{at}.outcome"),
        token_of!(StepOutcome, "STEP_OUTCOME_", record.outcome, Some("none")),
    );
    out.push(
        &format!("{at}.phase"),
        token_of!(FailurePhase, "FAILURE_PHASE_", record.phase, Some("none")),
    );
    out.push(
        &format!("{at}.class"),
        token_of!(FailureClass, "FAILURE_CLASS_", record.class, Some("none")),
    );
    out.text(&format!("{at}.reason"), &record.reason);
    out.push(&format!("{at}.forced"), record.forced);
}

/// The text of `hostkeeper admin abort <service>`: `aborted: <service>`, then the order as its
/// step record holds it once it ended, under `record` ([`record_lines`]; `record: none` when the
/// daemon sent none).
pub fn abort_text(service: &str, answer: &AbortResponse) -> String {
    let mut out = Lines::default();
    out.push("aborted", service);
    match &answer.record {
        None => out.push("record", "none"),
        Some(record) => record_lines("record", record, &mut out),
    }
    out.0
}

/// The text of `hostkeeper admin clear-record <service>`: `cleared: <service>`, then `problem`,
/// what the record was cleared for.
pub fn clear_record_text(service: &str, answer: &ClearRecordResponse) -> String {
    let mut out = Lines::default();
    out.push("cleared", service);
    out.push(
        "problem",
        token_of!(RecordProblem, "RECORD_PROBLEM_", answer.problem),
    );
    out.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves: each of the 17 gRPC status codes, by its number, has the exit code docs/admin.md
    /// gives it: 0 for OK, 2 for a request refused as made, 3 for a caller refused, 1 otherwise.
    ///
    /// Catches: a code moved to another exit code, so that a script reading the status takes a
    /// refused caller for a bad request, or a failure for a success.
    #[test]
    fn every_grpc_code_has_its_exit_code() {
        let want: [u8; 17] = [0, 1, 1, 2, 1, 1, 1, 3, 1, 2, 1, 1, 1, 1, 1, 1, 3];
        for (number, want) in (0..).zip(want) {
            let code = tonic::Code::from_i32(number);
            assert_eq!(code as i32, number, "tonic knows the code {number}");
            assert_eq!(exit::code(code), want, "{code:?}");
        }
        assert_eq!(
            (exit::SUCCESS, exit::FAILURE, exit::USAGE, exit::DENIED),
            (0, 1, 2, 3)
        );
    }

    /// Catches a runtime that could not be made reported as a refusal, or with another exit
    /// code than a failure.
    #[test]
    fn a_runtime_that_cannot_be_made_is_a_failure_on_stderr() {
        let failed = Failed::Runtime(io::Error::other("no threads"));
        assert_eq!(
            failed.line(Path::new("/s")),
            "hostkeeper admin: the async runtime could not be made: no threads"
        );
        assert_eq!(failed.exit_code(), exit::FAILURE);
    }

    /// Catches an enum value this client does not know shown as another, and a prefix that is
    /// not stripped.
    #[test]
    fn an_unknown_enum_value_is_named_by_its_number() {
        assert_eq!(token_of!(Lifecycle, "LIFECYCLE_", 99), "unknown_99");
        assert_eq!(token_of!(Lifecycle, "LIFECYCLE_", 0), "unspecified");
        assert_eq!(token_of!(Unhealthy, "UNHEALTHY_", 0, Some("none")), "none");
        assert_eq!(token(Some("OTHER_NAME"), "PREFIX_", 1, None), "other_name");
    }
}
