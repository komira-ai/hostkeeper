//! A host that has no certificate yet: the daemon's real start path (`startup_notifying` and
//! `serve`, over the fakes of the privileged ports), with the daemon config filled in from the
//! packaging's template, reaches ready, starts its loops, and the heartbeat loop waits for the
//! certificate under the pki directory, naming it once, beating nothing.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::cli::{HostFiles, host_links};
use hostkeeper_daemon::links::{Links, NO_ENROLLMENT};
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::{FakeNotifier, Log as Events};
use hostkeeper_link::testing::FakeLink;
use hostkeeper_link::{DEFAULT_HEARTBEAT_SECS, Log, PkiCertSource, Sleeper, ThreadDispatch, Tick};
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
struct Lines(Mutex<Sender<String>>);

impl Log for Lines {
    fn line(&self, text: &str) {
        let sent = self.0.lock().expect("the sender").send(text.to_owned());
        assert!(sent.is_ok(), "the test reads every line");
    }
}

/// The heartbeat loop's wait, stepped by the test: it says how long it would wait, then waits
/// for the test to say beat or stop.
struct Stepped {
    waits: Mutex<Sender<u64>>,
    ticks: Mutex<Receiver<Tick>>,
}

impl Sleeper for Stepped {
    fn sleep(&self, secs: u64) -> Tick {
        let said = self.waits.lock().expect("the sender").send(secs);
        assert!(said.is_ok(), "the test reads every wait");
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
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        pki: root.join("pki"),
    };
    let notifier = FakeNotifier::new(Events::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(notifier.sent(), 1, "ready");
    let daemon = Arc::clone(started.daemon());

    let (said, lines) = mpsc::channel();
    let log = Arc::new(Lines(Mutex::new(said)));
    let (waited, waits) = mpsc::channel();
    let (step, ticks) = mpsc::channel();
    let link = Arc::new(FakeLink::default());
    let links = Links {
        enroller: Box::new(|| Err("this test has no coordinator".to_owned())),
        link: link.clone(),
        certs: Arc::new(PkiCertSource::new(&files.pki, log.clone())),
        log,
        sleeper: Box::new(Stepped {
            waits: Mutex::new(waited),
            ticks: Mutex::new(ticks),
        }),
        dispatch: Arc::new(ThreadDispatch),
        ..host_links(&daemon, &files, DEFAULT_HEARTBEAT_SECS)
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let served = runtime.spawn(start::serve(started, links));

    let mut first = vec![lines.recv().expect("a line"), lines.recv().expect("a line")];
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
            waits.recv().expect("the heartbeat loop waits"),
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
        waits.recv().expect("the heartbeat loop waits"),
        DEFAULT_HEARTBEAT_SECS.get()
    );
    assert!(
        lines.try_recv().is_err(),
        "what it waits for is said once, not at every try"
    );
    assert!(
        link.calls().is_empty(),
        "no heartbeat without a certificate"
    );
    assert!(!files.pki.join("current").exists());

    step.send(Tick::Stop).expect("the loop takes the tick");
    daemon.ctx().restart().request();
    let ended = runtime.block_on(served).expect("the server task");
    assert!(ended.is_ok(), "{ended:?}");
}
