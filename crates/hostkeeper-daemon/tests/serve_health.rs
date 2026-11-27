//! The daemon served until its SIGTERM (`hostkeeper_daemon::start::serve_until`): at SIGTERM the
//! clean-shutdown marker is judged before the loops are told to stop; at its own update it is
//! not, and the marker stays `running`. The SIGTERM is a future the test ends; the real signal is
//! `tests/sigterm.rs`'s.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hostkeeper_daemon::health_state::{self, Marker, Read};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_daemon::start;
use hostkeeper_daemon::terminate::Terminated;
use hostkeeper_link::testing::RecordingLog;
use support::{OWN, Rig, idle_links};

/// A probe that says the system is going down and counts how often it was asked.
#[derive(Clone, Default)]
struct Counted(Arc<AtomicUsize>);

impl ShutdownProbe for Counted {
    fn going_down(&self) -> GoingDown {
        self.0.fetch_add(1, Ordering::SeqCst);
        GoingDown::Yes("the system is stopping".to_owned())
    }
}

fn marker(rig: &Rig) -> Marker {
    match health_state::read(&rig.state, "") {
        Read::Read(file) => file.marker,
        other => panic!("no state file: {other:?}"),
    }
}

/// Serves `rig`'s daemon with its keeper until `terminated` ends or the daemon asks to exit for
/// its own update (`update`): whether the loops were told to stop, and how often the probe was
/// asked.
fn serve(rig: &Rig, terminated: Terminated, update: bool) -> (bool, usize) {
    let started = start::startup(&rig.settings(), rig.ports(), OWN).expect("the daemon starts");
    let daemon = Arc::clone(started.daemon());
    let probe = Counted::default();
    let boot = BootFacts {
        boot_id: Some("boot-1".to_owned()),
        ..BootFacts::default()
    };
    let keeper = HealthKeeper::open(
        &rig.state,
        boot,
        Box::new(probe.clone()),
        Arc::new(RecordingLog::default()),
    );
    let links = hostkeeper_daemon::links::Links {
        health: Some(Arc::new(keeper)),
        ..idle_links()
    };
    let stop = links.stop.clone();
    if update {
        daemon.ctx().restart().request();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(start::serve_until(started, links, terminated));
    (stop.is_stopped(), probe.0.load(Ordering::SeqCst))
}

/// Proves a SIGTERM ends serving with the marker judged once (`clean`, the probe saying the
/// system goes down) and the loops told to stop.
///
/// Catches: a SIGTERM that ends the daemon with no marker, or leaves its loops running.
#[test]
fn a_sigterm_judges_the_marker_and_stops_the_loops() {
    let rig = Rig::new("serve-sigterm");
    let (stopped, asked) = serve(&rig, Box::pin(std::future::ready(())), false);
    assert_eq!((stopped, asked), (true, 1));
    assert_eq!(marker(&rig), Marker::Clean);
}

/// Proves the daemon's exit for its own update stops the loops with no probe asked and the marker
/// left `running`.
///
/// Catches: a self-update counted as a clean shutdown, so a crash after it reads as clean.
#[test]
fn an_exit_for_the_daemons_own_update_leaves_running() {
    let rig = Rig::new("serve-update");
    let (stopped, asked) = serve(&rig, Box::pin(std::future::pending()), true);
    assert_eq!((stopped, asked), (true, 0));
    assert_eq!(marker(&rig), Marker::Running);
}
