//! The console's issue file of the enrollment (`hostkeeper_daemon::console`): its exact text in
//! each state the enrollment loop can leave, the write and the redraw that follow a change of
//! text and only a change, the failures and how often they are logged, and the loop that writes
//! it after each step and each failure to make the enroller.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_daemon::cli::{ConsoleFiles, HostFiles, host_links};
use hostkeeper_daemon::console::{
    AGETTY, ConsoleIssue, ENROLLMENT_ISSUE, ISSUE_LINK, NO_ISSUE, NO_REDRAW, RELOAD, step_text,
    unopened_text,
};
use hostkeeper_daemon::links::Links;
use hostkeeper_enroll::{
    CoordinatorEnroll, EnrollAnswer, EnrollRequest, EnrollState, Enrollment, Error, HostId,
    LinuxClaims, OsEntropy, Outcome, PkiDir, Poll, PollAnswer, Reason, RenewAnswer, RenewRequest,
    RequestId, Settings as EnrollSettings, SystemWallClock, TransportError,
};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::StopFlag;
use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper};
use hostkeeper_placed::CommandLauncher;
use sha2::{Digest, Sha256};
use support::{Rig, Scratch, idle_links, open_once};
use x509_cert::der::{Decode, Encode};
use x509_cert::request::CertReq;

const COORDINATOR: &str = "https://192.0.2.10";

/// The key hash of the tests' pending key: the SHA-256 of the bytes `spki`.
const KEY: &str = "6feecc8c16c5551d9feb3eb5f77e2da773bf68bd9ef9c52927ceb2c86e56892b";

fn pending() -> Enrollment {
    Enrollment::Pending {
        key_sha256: RequestId::of_spki(b"spki"),
    }
}

fn enrolled() -> Enrollment {
    Enrollment::Enrolled {
        host_id: HostId::new("node-7").expect("a host id"),
        not_after: 2_000_000_000,
    }
}

fn down() -> Error {
    Error::Transport(TransportError("connection refused".to_owned()))
}

fn untrusted() -> Error {
    Error::UntrustedCoordinator {
        presented: "ab".repeat(32),
    }
}

/// Catches a console that shows anything but the request's state, the coordinator and the key
/// hash, or shows them otherwise than documented: the exact text of each state the loop can
/// leave, pending, approved and an untrusted coordinator first.
#[test]
fn the_issue_text_of_each_state_is_exact() {
    assert_eq!(
        step_text(COORDINATOR, &Ok(Outcome::Pending), &pending()),
        format!(
            "hostkeeper enrollment: pending approval; approve only the request whose key_sha256 is the key sha256 below\n  coordinator: https://192.0.2.10\n  key sha256:  {KEY}\n\n"
        )
    );
    let approved = "hostkeeper enrollment: approved, enrolled as host:node-7\n  coordinator: https://192.0.2.10\n\n";
    // Approved whatever the step came to: a renewal's error is the heartbeat's to report.
    for step in [Ok(Outcome::Idle), Err(down()), Err(untrusted())] {
        assert_eq!(step_text(COORDINATOR, &step, &enrolled()), approved);
    }
    let not_sent = "hostkeeper enrollment: nothing sent: the coordinator's CA is not the pinned ca_sha256\n  coordinator: https://192.0.2.10\n";
    assert_eq!(
        step_text(COORDINATOR, &Err(untrusted()), &Enrollment::Unenrolled),
        format!("{not_sent}  key sha256:  none yet\n\n")
    );
    assert_eq!(
        step_text(COORDINATOR, &Err(untrusted()), &pending()),
        format!("{not_sent}  key sha256:  {KEY}\n\n")
    );
    let reason = Reason::new("host_retired").expect("a reason");
    assert_eq!(
        step_text(COORDINATOR, &Err(Error::Denied(reason)), &pending()),
        format!(
            "hostkeeper enrollment: denied: host_retired\n  coordinator: https://192.0.2.10\n  key sha256:  {KEY}\n\n"
        )
    );
    assert_eq!(
        step_text(COORDINATOR, &Err(down()), &Enrollment::Unenrolled),
        "hostkeeper enrollment: error: the coordinator could not be reached: connection refused\n  coordinator: https://192.0.2.10\n  key sha256:  none yet\n\n"
    );
    assert_eq!(
        step_text(
            COORDINATOR,
            &Ok(Outcome::Idle),
            &Enrollment::Unreadable("the pki directory is gone".to_owned())
        ),
        "hostkeeper enrollment: error: the pki directory is gone\n  coordinator: https://192.0.2.10\n  key sha256:  none yet\n\n"
    );
    assert_eq!(
        step_text(COORDINATOR, &Ok(Outcome::Idle), &Enrollment::Unenrolled),
        "hostkeeper enrollment: no request yet\n  coordinator: https://192.0.2.10\n  key sha256:  none yet\n\n"
    );
    assert_eq!(
        unopened_text(COORDINATOR, "the pki directory is locked"),
        "hostkeeper enrollment: error: the enroller cannot be opened: the pki directory is locked\n  coordinator: https://192.0.2.10\n  key sha256:  unknown\n\n"
    );
}

/// Catches a text that agetty could read as an escape or that breaks the lines: a backslash,
/// a line break, a tab, an escape character and a character that is not ASCII, in the
/// coordinator and in an error, are each written as `?`.
#[test]
fn every_character_that_is_not_printable_ascii_is_a_question_mark() {
    assert_eq!(
        unopened_text("https://c\\4.example", "a\nb\tc\u{1b}[2Jd\u{e9}"),
        "hostkeeper enrollment: error: the enroller cannot be opened: a?b?c?[2Jd?\n  coordinator: https://c?4.example\n  key sha256:  unknown\n\n"
    );
}

/// What the fake agetty saw: the argument vector of each run, and the issue file's text then.
type Runs = Arc<Mutex<Vec<(Vec<String>, String)>>>;

/// An agetty that notes each run with the issue file's text at that moment, and fails while
/// `failing` holds a reason.
struct FakeAgetty {
    issue: PathBuf,
    runs: Runs,
    failing: Arc<Mutex<Option<String>>>,
}

impl CommandLauncher for FakeAgetty {
    fn run(&self, argv: &[&str]) -> Result<(), String> {
        let text = fs::read_to_string(&self.issue).unwrap_or_else(|e| format!("unread: {e}"));
        let argv = argv.iter().map(|arg| (*arg).to_owned()).collect();
        self.runs.lock().expect("the runs").push((argv, text));
        match self.failing.lock().expect("the failure").clone() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }
}

/// A console over the issue file `issue` and a [`FakeAgetty`]: the console, its runs and the
/// switch that makes it fail.
fn console_at(issue: &Path) -> (ConsoleIssue, Runs, Arc<Mutex<Option<String>>>) {
    let runs = Runs::default();
    let failing = Arc::new(Mutex::new(None));
    let agetty = FakeAgetty {
        issue: issue.to_path_buf(),
        runs: runs.clone(),
        failing: failing.clone(),
    };
    let console = ConsoleIssue::new(
        issue.to_path_buf(),
        COORDINATOR.to_owned(),
        Path::new("/sbin/agetty"),
        Box::new(agetty),
    );
    (console, runs, failing)
}

fn reload() -> Vec<String> {
    vec!["/sbin/agetty".to_owned(), "--reload".to_owned()]
}

/// Catches a getty asked to redraw before the file is written or not at all, a file another
/// process could read, a file written again (and the gettys asked again) every five seconds
/// for the same text, and a changed text not written: the file holds the text, mode 0600,
/// before agetty runs with `--reload`; the same text again writes and runs nothing; a new text
/// is written and redrawn.
#[test]
fn a_changed_text_is_written_then_redrawn_and_the_same_text_is_left_alone() {
    let scratch = Scratch::new("console-write");
    let issue = scratch.path().join("enrollment.issue");
    let (mut console, runs, _) = console_at(&issue);
    let log = RecordingLog::default();
    console.after_step(&Ok(Outcome::Pending), &pending(), &log);
    let first = step_text(COORDINATOR, &Ok(Outcome::Pending), &pending());
    assert_eq!(fs::read_to_string(&issue).expect("the issue"), first);
    let mode = fs::metadata(&issue).expect("its mode").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(*runs.lock().expect("runs"), [(reload(), first.clone())]);
    assert!(!scratch.path().join("enrollment.issue.tmp").exists());

    fs::write(&issue, "changed by hand").expect("overwrite it");
    console.after_step(&Ok(Outcome::Pending), &pending(), &log);
    assert_eq!(
        fs::read_to_string(&issue).expect("the issue"),
        "changed by hand"
    );
    assert_eq!(runs.lock().expect("runs").len(), 1);

    console.after_step(&Ok(Outcome::Idle), &enrolled(), &log);
    let second = step_text(COORDINATOR, &Ok(Outcome::Idle), &enrolled());
    assert_eq!(fs::read_to_string(&issue).expect("the issue"), second);
    assert_eq!(
        *runs.lock().expect("runs"),
        [(reload(), first), (reload(), second)]
    );
    assert_eq!(log.lines(), Vec::<String>::new());
}

/// Catches a write that fails silently, is logged every step, is never tried again, or still
/// asks the gettys to redraw: with no directory for the file, the failure is logged once with
/// the path and the error, nothing is run, and once the directory is there the next step writes
/// the file and redraws.
#[test]
fn a_failed_write_is_logged_once_and_tried_again_after_the_next_step() {
    let scratch = Scratch::new("console-no-dir");
    let issue = scratch.path().join("run").join("enrollment.issue");
    let (mut console, runs, _) = console_at(&issue);
    let log = RecordingLog::default();
    for _ in 0..3 {
        console.unopened("locked", &log);
    }
    assert_eq!(
        log.lines(),
        [format!(
            "{NO_ISSUE}{}: No such file or directory (os error 2)",
            issue.display()
        )]
    );
    assert_eq!(runs.lock().expect("runs").len(), 0);

    fs::create_dir(scratch.path().join("run")).expect("the directory");
    console.unopened("locked", &log);
    let text = unopened_text(COORDINATOR, "locked");
    assert_eq!(fs::read_to_string(&issue).expect("the issue"), text);
    assert_eq!(*runs.lock().expect("runs"), [(reload(), text)]);
    assert_eq!(log.lines().len(), 1);
}

/// Catches a failed redraw that is not logged, is logged at every step, makes the same text be
/// written again, or that hides a later failure once a redraw has worked: the failure is logged
/// once with the command and why; the same text is not run again; a new text is written and
/// run, and the same failure is not logged again; after a redraw that works, a failure is
/// logged anew.
#[test]
fn a_failed_redraw_is_logged_once_until_a_redraw_works() {
    let scratch = Scratch::new("console-redraw");
    let issue = scratch.path().join("enrollment.issue");
    let (mut console, runs, failing) = console_at(&issue);
    let log = RecordingLog::default();
    *failing.lock().expect("the switch") = Some("exit status 1".to_owned());
    let refused = format!("{NO_REDRAW}/sbin/agetty --reload: exit status 1");

    console.unopened("locked", &log);
    console.unopened("locked", &log);
    assert_eq!(log.lines(), std::slice::from_ref(&refused));
    assert_eq!(runs.lock().expect("runs").len(), 1);

    console.after_step(&Ok(Outcome::Pending), &pending(), &log);
    let pending_text = step_text(COORDINATOR, &Ok(Outcome::Pending), &pending());
    assert_eq!(fs::read_to_string(&issue).expect("the issue"), pending_text);
    assert_eq!(runs.lock().expect("runs").len(), 2);
    assert_eq!(log.lines(), std::slice::from_ref(&refused));

    *failing.lock().expect("the switch") = None;
    console.after_step(&Err(down()), &pending(), &log);
    *failing.lock().expect("the switch") = Some("exit status 1".to_owned());
    console.after_step(&Err(untrusted()), &pending(), &log);
    assert_eq!(runs.lock().expect("runs").len(), 4);
    assert_eq!(log.lines(), [refused.clone(), refused]);
}

/// The coordinator of the loop's test: each step answers by the next of `script`, and the last
/// step stops the loop. A step that presents the pinned CA posts, and its request stays pending.
struct Scripted {
    script: Vec<Answer>,
    stop: StopFlag,
    /// The id of the request posted last.
    posted: Arc<Mutex<Option<RequestId>>>,
}

#[derive(Clone, Copy)]
enum Answer {
    /// Presents a CA other than the pinned one.
    OtherCa,
    /// Cannot be reached.
    Down,
    /// Presents the pinned CA and leaves the request pending.
    Pending,
}

/// The CA certificate the scripted coordinator presents when it presents the pinned one.
const CA_DER: &[u8] = b"the coordinator's CA";

impl CoordinatorEnroll for Scripted {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        let answer = self.script.remove(0);
        if self.script.is_empty() {
            self.stop.stop();
        }
        match answer {
            Answer::OtherCa => Ok(b"another CA".to_vec()),
            Answer::Down => Err(TransportError("connection refused".to_owned())),
            Answer::Pending => Ok(CA_DER.to_vec()),
        }
    }

    fn enroll(&mut self, request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        let id = request_id(request);
        *self.posted.lock().expect("the request") = Some(id.clone());
        Ok(EnrollAnswer {
            request_id: id,
            state: EnrollState::Pending,
        })
    }

    fn poll(&mut self, poll: &Poll) -> Result<PollAnswer, TransportError> {
        match poll.signature.is_empty() {
            true => Ok(PollAnswer::Challenge(vec![7; 32])),
            false => Ok(PollAnswer::Pending),
        }
    }

    fn renew(&mut self, _request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }
}

/// The request id of `request`: the SHA-256 of its CSR's public key.
fn request_id(request: &EnrollRequest) -> RequestId {
    let csr = CertReq::from_der(&request.csr_der).expect("a CSR");
    RequestId::of_spki(&csr.info.public_key.to_der().expect("the public key's DER"))
}

/// Catches a loop that does not show its steps on the console, shows a step's text before the
/// step or after the next one, or shows nothing while its enroller cannot be made: the first
/// try to make the enroller fails, then the enroller's steps meet a coordinator with another
/// CA, one that cannot be reached, and one that leaves the request pending; the gettys are
/// asked to redraw once after each, each time with that state's text in the file, and the
/// pending text names the key the request was posted with.
#[test]
fn the_enrollment_loop_shows_each_step_and_each_failure_to_open_on_the_console() {
    let rig = Rig::new("console-loop");
    let daemon = Arc::new(rig.daemon());
    let issue = rig.scratch.path().join("enrollment.issue");
    let (console, runs, _) = console_at(&issue);
    let stop = StopFlag::default();
    let posted: Arc<Mutex<Option<RequestId>>> = Arc::default();
    let claims = rig.scratch.path().join("no-claims");
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(Scripted {
            script: vec![Answer::OtherCa, Answer::Down, Answer::Pending],
            stop: stop.clone(),
            posted: posted.clone(),
        }),
        store: Box::new(PkiDir::new(&rig.scratch.path().join("pki"))),
        claims: Box::new(LinuxClaims::new(&claims, &claims, &claims, &claims)),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(FakeClock::new(0, 0)),
    };
    let settings = EnrollSettings {
        ca_sha256: Sha256Hex::parse(&hex(&Sha256::digest(CA_DER))).expect("a SHA-256"),
        image: "image-1".to_owned(),
    };
    let mut opened = Some(open_once(settings, ports));
    let mut tries = 0;
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        // The enroller is made at the second try: the first is refused.
        enroller: Box::new(move || {
            tries += 1;
            if tries == 1 {
                return Err("the pki directory is locked".to_owned());
            }
            let mut open = opened.take().ok_or("made twice")?;
            open()
        }),
        enroll_sleeper: Box::new(ScriptedSleeper::new(1)),
        dispatch: dispatch.clone(),
        stop: stop.clone(),
        console: Some(console),
        ..idle_links()
    };
    links.start(&daemon);
    assert!(dispatch.run_next(), "the enrollment loop");
    assert!(stop.is_stopped());

    let key = posted
        .lock()
        .expect("the request")
        .clone()
        .expect("a request was posted");
    let wanted = [
        unopened_text(COORDINATOR, "the pki directory is locked"),
        step_text(COORDINATOR, &Err(untrusted()), &Enrollment::Unenrolled),
        step_text(COORDINATOR, &Err(down()), &Enrollment::Unenrolled),
        step_text(
            COORDINATOR,
            &Ok(Outcome::Pending),
            &Enrollment::Pending { key_sha256: key },
        ),
    ];
    let runs = runs.lock().expect("runs").clone();
    let seen: Vec<String> = runs.iter().map(|(_, text)| text.clone()).collect();
    assert_eq!(seen, wanted);
    assert!(runs.iter().all(|(argv, _)| *argv == reload()));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Catches a Linux host whose loop writes no issue file, or one that names another coordinator
/// or runs another agetty: the host's links over files that name a console write its issue
/// file with the daemon config's coordinator and run the given agetty with `--reload` as a
/// child process; an agetty that is not there is a redraw that failed, logged with its path.
/// The paths of a Linux host are the documented ones, and the link the packages install is in
/// `/etc/issue.d`.
#[test]
fn the_host_links_write_the_issue_file_and_run_the_given_agetty() {
    let rig = Rig::new("console-host");
    let daemon = Arc::new(rig.daemon());
    let root = rig.scratch.path();
    let agetty = root.join("no-agetty");
    let files = HostFiles {
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        pki: root.join("pki"),
        console: Some(ConsoleFiles {
            issue: root.join("enrollment.issue"),
            agetty: agetty.clone(),
        }),
    };
    let links = host_links(
        &daemon,
        &files,
        hostkeeper_link::DEFAULT_HEARTBEAT_SECS,
        PlatformKind::Linux,
        Box::new(hostkeeper_daemon::NoWatchdog),
    );
    let log = RecordingLog::default();
    let mut console = links.console.expect("a console");
    console.unopened("why", &log);
    let coordinator = daemon.ctx().config.coordinator.as_str().to_owned();
    assert_eq!(
        fs::read_to_string(root.join("enrollment.issue")).expect("the issue"),
        unopened_text(&coordinator, "why")
    );
    let lines = log.lines();
    let prefix = format!("{NO_REDRAW}{} {RELOAD}: ", agetty.display());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with(&prefix), "{lines:?}");
    assert_eq!(ENROLLMENT_ISSUE, "/run/hostkeeper/enrollment.issue");
    assert_eq!(AGETTY, "/sbin/agetty");
    assert_eq!(ISSUE_LINK, "/etc/issue.d/hostkeeper.issue");
}
