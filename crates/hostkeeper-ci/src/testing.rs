//! What the tests share: a fake Releases service behind the port, and fake ports.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::operator::v1::{
    ConvergeRequest, ConvergeResponse, Convergence, ConvergenceState, DesiredState, RoleCounts,
    SetDesiredRequest, SetDesiredResponse,
};
use tonic::Status;

use crate::flags::{SetDesiredFlags, Settings, validate};
use crate::flow::set_desired;
use crate::grpc::{Failure, Releases, Target};
use crate::ports::Ports;
use crate::token::TokenError;

/// A reply of the fake coordinator to `SetDesired`: the response, or the status the call ends with.
pub(crate) type SetReply = Result<SetDesiredResponse, Status>;

/// A reply of the fake coordinator to `Converge`.
pub(crate) type ConvReply = Result<ConvergeResponse, Status>;

/// Everything the fakes saw, in order.
#[derive(Default)]
pub(crate) struct Log {
    /// `token N`, `SetDesired`, `Converge`, `pause S`.
    pub events: Vec<String>,
    /// The `SetDesired` requests.
    pub sets: Vec<SetDesiredRequest>,
    /// The `Converge` requests.
    pub converges: Vec<ConvergeRequest>,
    /// The bearer of each call, in the order of the calls.
    pub bearers: Vec<String>,
    /// The lines printed on stdout.
    pub said: Vec<String>,
    /// The endpoint and the CA file of each call's target.
    pub targets: Vec<(String, PathBuf)>,
    pub audiences: Vec<String>,
}

pub(crate) type SharedLog = Arc<Mutex<Log>>;

/// The coordinator: it answers `SetDesired` with one reply, and `Converge` with its replies in
/// turn, the last one again and again. When `not_sent` is set, every call ends before anything
/// is sent, with that text.
#[derive(Clone)]
pub(crate) struct Coordinator {
    log: SharedLog,
    set_desired: SetReply,
    converge: Arc<Mutex<VecDeque<ConvReply>>>,
    not_sent: Option<String>,
}

impl Coordinator {
    /// Records a call; the failure when the call is not to be sent.
    fn record(&self, rpc: &str, target: &Target, bearer: &str) -> Result<(), Failure> {
        let mut log = self.log.lock().expect("lock");
        log.events.push(rpc.to_owned());
        log.bearers.push(bearer.to_owned());
        log.targets
            .push((target.endpoint.clone(), target.ca_file.clone()));
        self.not_sent
            .clone()
            .map_or(Ok(()), |why| Err(Failure::NotSent(why)))
    }
}

impl Releases for Coordinator {
    fn set_desired(
        &self,
        target: &Target,
        bearer: &str,
        request: SetDesiredRequest,
    ) -> Result<SetDesiredResponse, Failure> {
        self.record("SetDesired", target, bearer)?;
        self.log.lock().expect("lock").sets.push(request);
        self.set_desired.clone().map_err(Failure::Status)
    }

    fn converge(
        &self,
        target: &Target,
        bearer: &str,
        request: ConvergeRequest,
    ) -> Result<ConvergeResponse, Failure> {
        self.record("Converge", target, bearer)?;
        self.log.lock().expect("lock").converges.push(request);
        let mut replies = self.converge.lock().expect("lock");
        let reply = if replies.len() > 1 {
            replies.pop_front().expect("a reply")
        } else {
            replies.front().expect("a reply").clone()
        };
        reply.map_err(Failure::Status)
    }
}

/// Fake ports: tokens `token-1`, `token-2`..., the fake coordinator, a pause that records.
pub(crate) struct Fake {
    pub log: SharedLog,
    coordinator: Coordinator,
    issued: u32,
    pub token_fails: bool,
}

impl Fake {
    pub(crate) fn new(set_desired: SetReply, converge: Vec<ConvReply>) -> Fake {
        let log = SharedLog::default();
        Fake {
            coordinator: Coordinator {
                log: Arc::clone(&log),
                set_desired,
                converge: Arc::new(Mutex::new(converge.into())),
                not_sent: None,
            },
            log,
            issued: 0,
            token_fails: false,
        }
    }

    /// Makes every call end before anything is sent, with `why`.
    pub(crate) fn not_sending(&mut self, why: &str) {
        self.coordinator.not_sent = Some(why.to_owned());
    }
}

impl Ports for Fake {
    fn token(&mut self, audience: &str) -> Result<String, TokenError> {
        let mut log = self.log.lock().expect("lock");
        log.audiences.push(audience.to_owned());
        if self.token_fails {
            return Err(TokenError::NoVariables);
        }
        self.issued += 1;
        log.events.push(format!("token {}", self.issued));
        Ok(format!("token-{}", self.issued))
    }

    fn releases(&self) -> &dyn Releases {
        &self.coordinator
    }

    fn pause(&mut self, secs: u64) {
        self.log
            .lock()
            .expect("lock")
            .events
            .push(format!("pause {secs}"));
    }

    fn say(&mut self, line: &str) {
        self.log.lock().expect("lock").said.push(line.to_owned());
    }
}

/// The reply of `SetDesired` that stored `version`.
pub(crate) fn stored(version: &str) -> SetReply {
    Ok(SetDesiredResponse {
        desired: Some(DesiredState {
            version: version.to_owned(),
            ..DesiredState::default()
        }),
        ..SetDesiredResponse::default()
    })
}

/// One role's counts.
pub(crate) fn role(
    name: &str,
    at_desired: u32,
    updating: u32,
    failed: u32,
    down: u32,
) -> RoleCounts {
    RoleCounts {
        role: name.to_owned(),
        at_desired,
        updating,
        failed,
        down,
        pinned: 0,
    }
}

/// The reply of `Converge`.
pub(crate) fn convergence(
    desired: Option<&str>,
    state: ConvergenceState,
    roles: Vec<RoleCounts>,
) -> ConvReply {
    Ok(ConvergeResponse {
        convergence: Some(Convergence {
            desired: desired.map(str::to_owned),
            state: state as i32,
            roles,
            ..Convergence::default()
        }),
    })
}

/// A directory of a test, beside the test binary in the target directory.
pub(crate) fn scratch(name: &str) -> hostkeeper_testkit::Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe
        .parent()
        .expect("its directory")
        .join("hostkeeper-ci-unit");
    hostkeeper_testkit::Scratch::under(&base, name).expect("a scratch directory")
}

/// The PEM of a root certificate, made now.
pub(crate) fn root_pem() -> String {
    rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("a certificate")
        .cert
        .pem()
}

/// Flags that pass every check, in `dir`: a root certificate file, an outputs file's path.
pub(crate) fn flags(dir: &Path) -> SetDesiredFlags {
    let web_roots = dir.join("web-roots.pem");
    std::fs::write(&web_roots, root_pem()).expect("write the roots");
    SetDesiredFlags {
        service: "svc".to_owned(),
        service_version: "1.2.3".to_owned(),
        package_digest: Some("a".repeat(64)),
        release_index_file: None,
        release_index_digest: None,
        endpoint: "https://coordinator.example".to_owned(),
        ca_file: dir.join("ca.pem"),
        audience: "hostkeeper-ci".to_owned(),
        wait: true,
        max_polls: 5,
        pause_secs: 10,
        work_dir: dir.to_owned(),
        github_output: Some(dir.join("github-output")),
        web_roots,
    }
}

/// The checked settings of `flags`.
pub(crate) fn settings(flags: &SetDesiredFlags) -> Settings {
    validate(flags).expect("valid flags").0
}

/// What one run of `set_desired` left.
pub(crate) struct Ran {
    pub code: u8,
    /// The job's output file, when there is one.
    pub outputs: Option<String>,
    pub log: SharedLog,
    pub dir: hostkeeper_testkit::Scratch,
}

impl Ran {
    pub(crate) fn said(&self) -> Vec<String> {
        self.log.lock().expect("lock").said.clone()
    }

    pub(crate) fn events(&self) -> Vec<String> {
        self.log.lock().expect("lock").events.clone()
    }

    /// The names in the work directory, sorted.
    pub(crate) fn work_dir_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
            .expect("read the work directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

/// Runs `set_desired` on the fake coordinator, after `tweak` changed the flags.
pub(crate) fn run_case(
    name: &str,
    set_desired_reply: SetReply,
    converge: Vec<ConvReply>,
    token_fails: bool,
    tweak: impl FnOnce(&mut SetDesiredFlags),
) -> Ran {
    let dir = scratch(name);
    let mut flags = flags(dir.path());
    tweak(&mut flags);
    let mut fake = Fake::new(set_desired_reply, converge);
    fake.token_fails = token_fails;
    let mut stderr = Vec::new();
    let code = set_desired(&flags, &mut stderr, |_| &mut fake);
    let outputs = flags
        .github_output
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    Ran {
        code,
        outputs,
        log: Arc::clone(&fake.log),
        dir,
    }
}
