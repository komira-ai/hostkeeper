//! A host that has no certificate yet: the daemon's real start path (`startup_notifying` and
//! `serve`, over the fakes of the privileged ports), with the daemon config filled in from the
//! packaging's template, reaches ready, starts its loops, and the heartbeat loop waits for the
//! certificate under the pki directory, naming it once, beating nothing.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use hostkeeper_daemon::cli::{HostFiles, host_links};
use hostkeeper_daemon::links::{Links, NO_ENROLLMENT};
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::{Ending, FakeNotifier, Log as Events, Notice, Signal};
use hostkeeper_link::testing::FakeLink;
use hostkeeper_link::{DEFAULT_HEARTBEAT_SECS, Dispatch, Job, Log, PkiCertSource, Sleeper, Tick};
use support::{OWN, Rig};

/// The daemon config of a host as the packaging ships it, before it is filled in.
const TEMPLATE: &str = include_str!("../../../packaging/linux/hostkeeper.toml.template");

/// The value of each placeholder of the template, as TOML.
const VALUES: [(&str, &str); 7] = [
    ("COORDINATOR", "\"https://coordinator.example:8443\""),
    (
        "CA_SHA256",
        "\"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\"",
    ),
    ("RELEASE_CHANNEL", "\"https://releases.example\""),
    ("SIGNER_ORGANISATION", "\"example-org\""),
    ("SIGNER_WORKFLOW", "\".github/workflows/release.yml\""),
    ("SIGNER_REFS", "\"refs/tags/*\""),
    ("SIGNER_HOSTED_RUNNER", "true"),
];

/// The template with each quoted placeholder, its quotes included, replaced by its value.
fn filled() -> String {
    let mut text = TEMPLATE.to_owned();
    for (name, value) in VALUES {
        let start = text
            .find(&format!("\"@@{name}@@"))
            .unwrap_or_else(|| panic!("the template has the placeholder {name}"));
        let end = text[start + 1..]
            .find('"')
            .map(|at| start + 1 + at + 1)
            .unwrap_or_else(|| panic!("the placeholder {name} is closed"));
        text.replace_range(start..end, value);
    }
    for (name, _) in VALUES {
        assert!(
            !text.contains(&format!("@@{name}@@")),
            "{name} is filled:\n{text}"
        );
    }
    text
}

/// Each line of the loops, as it is written.
struct Lines(Notice<String>);

impl Log for Lines {
    fn line(&self, text: &str) {
        self.0.send(text.to_owned());
    }
}

/// Declares its party over when dropped: when the task that owns it ends, or is cancelled, or
/// panics.
struct OverOnDrop(Ending);

impl Drop for OverOnDrop {
    fn drop(&mut self) {
        self.0.over("the server ended");
    }
}

/// Declares its party over when dropped: when the thread of a loop that owns it ends or panics.
struct LoopOver {
    left: Arc<AtomicUsize>,
    ending: Ending,
}

impl Drop for LoopOver {
    fn drop(&mut self) {
        if self.left.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.ending.over("both loops ended");
        }
    }
}

/// Runs each of the two loops on a thread of its own, and declares `ending` over when both have
/// ended: the lines and the waits are sent by the loops, so the test waits on them, not only on
/// the server.
struct Loops {
    left: Arc<AtomicUsize>,
    ending: Ending,
}

impl Dispatch for Loops {
    fn start(&self, job: Job) {
        let over = LoopOver {
            left: Arc::clone(&self.left),
            ending: self.ending.clone(),
        };
        thread::spawn(move || {
            let _over = over;
            job();
        });
    }
}

/// The heartbeat loop's wait, stepped by the test: it says how long it would wait, then waits
/// for the test to say beat or stop.
struct Stepped {
    waits: Notice<u64>,
    ticks: Mutex<Receiver<Tick>>,
}

impl Sleeper for Stepped {
    fn sleep(&self, secs: u64) -> Tick {
        self.waits.send(secs);
        self.ticks
            .lock()
            .expect("the ticks")
            .recv()
            .unwrap_or(Tick::Stop)
    }
}

/// Catches a daemon with no certificate that does not reach ready, exits, spins or beats: with
/// the template's config the daemon starts and says ready once; the enrollment loop says why it
/// has no enroller here; the heartbeat loop names the pki directory's `current` link it waits
/// for, once, makes no heartbeat call, and waits a whole period after every try; and the daemon
/// still serves after three periods, until it is asked to exit.
#[test]
fn without_a_certificate_the_daemon_waits_and_names_the_missing_file() {
    let rig = Rig::new("no-certificate");
    fs::write(&rig.config, filled()).expect("the daemon config");
    fs::set_permissions(&rig.config, fs::Permissions::from_mode(0o600)).expect("its mode");
    let root = rig.scratch.path();
    let files = HostFiles {
        repositories: None,
        os_root: root.join("os"),
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        stores: root.join("stores.toml"),
        macos_pkg_signers: None,
        pki: root.join("pki"),
        console: None,
        managed_config: None,
    };
    let notifier = FakeNotifier::new(Events::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(notifier.sent(), 1, "ready");
    let daemon = Arc::clone(started.daemon());

    // Over when the server ends, or when both loops have.
    let serving = Ending::default();
    let (said, lines) = Signal::new();
    let log = Arc::new(Lines(said));
    let (waited, waits) = Signal::new();
    let (step, ticks) = mpsc::channel();
    let link = Arc::new(FakeLink::default());
    let links = Links {
        enroller: Box::new(|| Err("this test has no coordinator".to_owned())),
        link: link.clone(),
        certs: Arc::new(PkiCertSource::new(&files.pki, log.clone())),
        log,
        sleeper: Box::new(Stepped {
            waits: waited,
            ticks: Mutex::new(ticks),
        }),
        dispatch: Arc::new(Loops {
            left: Arc::new(AtomicUsize::new(2)),
            ending: serving.clone(),
        }),
        ..host_links(
            &daemon,
            &files,
            DEFAULT_HEARTBEAT_SECS,
            hostkeeper_launchd::PlatformKind::Linux,
            Box::new(hostkeeper_daemon::NoWatchdog),
        )
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let over = OverOnDrop(serving.clone());
    let served = runtime.spawn(async move {
        let _over = over;
        start::serve(started, links).await
    });

    let mut first = vec![
        lines.wait(&serving).expect("a line"),
        lines.wait(&serving).expect("a line"),
    ];
    first.sort();
    let waiting = format!(
        "not enrolled: waiting for {}",
        files.pki.join("current").display()
    );
    assert_eq!(
        first,
        [
            format!("{NO_ENROLLMENT}this test has no coordinator"),
            waiting
        ]
    );
    for period in 1..=3 {
        assert_eq!(
            waits.wait(&serving).expect("the heartbeat loop waits"),
            DEFAULT_HEARTBEAT_SECS.get(),
            "a whole period after try {period}"
        );
        assert!(
            !served.is_finished(),
            "the daemon serves after try {period}"
        );
        step.send(Tick::Beat).expect("the loop takes the tick");
    }
    assert_eq!(
        waits.wait(&serving).expect("the heartbeat loop waits"),
        DEFAULT_HEARTBEAT_SECS.get()
    );
    assert!(
        link.calls().is_empty(),
        "no heartbeat without a certificate"
    );
    assert!(!files.pki.join("current").exists());

    step.send(Tick::Stop).expect("the loop takes the tick");
    daemon.ctx().restart().request();
    runtime.block_on(served).expect("the server task");
    // Checked once the server is over, on purpose: a line said again would be queued, so it is
    // taken at once, and a wait with none fails at once rather than waiting.
    assert!(
        lines.wait(&serving).is_err(),
        "what it waits for is said once, not at every try"
    );
}
