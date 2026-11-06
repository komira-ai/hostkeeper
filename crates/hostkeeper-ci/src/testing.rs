//! What the tests share: a fake coordinator behind the real `hostkeeper ctl`, and fake ports.

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use hostkeeper_ctl::transport::ConnectError;
use hostkeeper_ctl::{Connect, CtlTransport, Target};
use hostkeeper_proto::hostkeeper::operator::v1::{
    ConvergeResponse, Convergence, ConvergenceState, DesiredState, RoleCounts, SetDesiredResponse,
};
use prost::Message as _;
use tonic::Status;
use tonic::metadata::MetadataMap;

use crate::call::TOKEN_FILE;
use crate::flags::{SetDesiredFlags, Settings, validate};
use crate::flow::set_desired;
use crate::ports::Ports;
use crate::token::TokenError;

/// A reply of the fake coordinator: an encoded response, or the status the call ends with.
pub(crate) type Reply = Result<Bytes, Status>;

/// A call the fake coordinator received.
pub(crate) struct Call {
    pub path: String,
    pub request: Bytes,
    pub bearer: String,
}

/// Everything the fakes saw, in order.
#[derive(Default)]
pub(crate) struct Log {
    /// `token N`, `SetDesired`, `Converge`, `pause S`.
    pub events: Vec<String>,
    pub calls: Vec<Call>,
    /// The lines printed on stdout.
    pub said: Vec<String>,
    /// The mode and the text of the token file at the time of each call.
    pub token_files: Vec<(u32, String)>,
    /// The host, port and CA file of each target.
    pub targets: Vec<(String, u16, PathBuf)>,
    pub audiences: Vec<String>,
}

pub(crate) type SharedLog = Arc<Mutex<Log>>;

/// The coordinator: it answers `SetDesired` with one reply, and `Converge` with its replies in
/// turn, the last one again and again.
#[derive(Clone)]
pub(crate) struct Coordinator {
    log: SharedLog,
    set_desired: Reply,
    converge: Arc<Mutex<VecDeque<Reply>>>,
    work_dir: PathBuf,
}

impl Connect for Coordinator {
    fn connect(&self, target: &Target) -> Result<Box<dyn CtlTransport>, ConnectError> {
        self.log.lock().expect("lock").targets.push((
            target.endpoint.host().to_owned(),
            target.endpoint.port(),
            target.ca_file.clone(),
        ));
        Ok(Box::new(self.clone()))
    }
}

impl CtlTransport for Coordinator {
    fn call(&self, path: &str, request: Bytes, metadata: &MetadataMap) -> Result<Bytes, Status> {
        let token_file = self.work_dir.join(TOKEN_FILE);
        let mode = std::fs::metadata(&token_file)
            .expect("the token file exists while the call runs")
            .permissions()
            .mode()
            & 0o777;
        let text = std::fs::read_to_string(&token_file).expect("the token file is readable");
        let bearer = metadata
            .get("authorization")
            .expect("an authorization header")
            .to_str()
            .expect("visible ASCII")
            .to_owned();
        let rpc = path.rsplit('/').next().expect("a path").to_owned();
        {
            let mut log = self.log.lock().expect("lock");
            log.events.push(rpc.clone());
            log.token_files.push((mode, text));
            log.calls.push(Call {
                path: path.to_owned(),
                request,
                bearer,
            });
        }
        if rpc == "SetDesired" {
            return self.set_desired.clone();
        }
        let mut replies = self.converge.lock().expect("lock");
        if replies.len() > 1 {
            replies.pop_front().expect("a reply")
        } else {
            replies.front().expect("a reply").clone()
        }
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
    pub(crate) fn new(work_dir: &Path, set_desired: Reply, converge: Vec<Reply>) -> Fake {
        let log = SharedLog::default();
        Fake {
            coordinator: Coordinator {
                log: Arc::clone(&log),
                set_desired,
                converge: Arc::new(Mutex::new(converge.into())),
                work_dir: work_dir.to_owned(),
            },
            log,
            issued: 0,
            token_fails: false,
        }
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

    fn connect(&self) -> &dyn Connect {
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
pub(crate) fn stored(version: &str) -> Reply {
    let response = SetDesiredResponse {
        desired: Some(DesiredState {
            version: version.to_owned(),
            ..DesiredState::default()
        }),
        ..SetDesiredResponse::default()
    };
    Ok(Bytes::from(response.encode_to_vec()))
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
) -> Reply {
    let response = ConvergeResponse {
        convergence: Some(Convergence {
            desired: desired.map(str::to_owned),
            state: state as i32,
            roles,
            ..Convergence::default()
        }),
    };
    Ok(Bytes::from(response.encode_to_vec()))
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

    pub(crate) fn token_file_exists(&self) -> bool {
        self.dir.path().join(TOKEN_FILE).exists()
    }
}

/// Runs `set_desired` on the fake coordinator, after `tweak` changed the flags.
pub(crate) fn run_case(
    name: &str,
    set_desired_reply: Reply,
    converge: Vec<Reply>,
    token_fails: bool,
    tweak: impl FnOnce(&mut SetDesiredFlags),
) -> Ran {
    let dir = scratch(name);
    let mut flags = flags(dir.path());
    tweak(&mut flags);
    let mut fake = Fake::new(&flags.work_dir, set_desired_reply, converge);
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
