//! Restarts that survive a run which stops part-way: the pending-restart markers of `install`.

mod common;
mod install_support;

use common::{SVC, kit, kit_with, ready_host};
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Kind, Mode, SYSTEMCTL};
use install_support::{CERT, CORE, MARKERS, ca_is_made_by_init, changes, install, marker, running};

const PROXY: &str = "hostkeeper-mdm-proxy.service";
const IP_AND_SS: [&str; 2] = ["/usr/bin/ip -o -4 addr show", "/usr/bin/ss -Hltn"];

fn restart_line(unit: &str) -> String {
    format!("/usr/bin/systemctl restart -- {unit}")
}

fn assert_marker(host: &FakeHost, unit: &str) {
    let n = host
        .node(&marker(unit))
        .unwrap_or_else(|| panic!("no marker for {unit}"));
    assert_eq!(
        (n.kind, n.mode, n.uid, n.gid, n.bytes),
        (Kind::File, 0o600, 0, 0, Vec::new()),
        "{unit}"
    );
}

/// A host the first run of which has come to the point where every service is installed and
/// enabled, but whose restart of NanoHUB (and so every restart after the first) did not happen.
fn interrupted_install() -> (common::Kit, FakeHost) {
    let kit = kit();
    let host = ready_host(&kit);
    host.put_file(CERT, 0o644, (SVC, SVC), b"CERT");
    ca_is_made_by_init(&host);
    host.reply(&[SYSTEMCTL, "restart", "--", CORE[1]], 1, "");
    let (result, _) = install(&host, &kit, Mode::Apply);
    let err = result.unwrap_err();
    assert!(matches!(err, MdmError::Exec(_)), "{err}");
    assert_eq!(err.exit_code(), 1);
    (kit, host)
}

/// Catches a restart lost with the run that was to do it: the run stopped at the restart of
/// NanoHUB, after the SCEP server's; the units' files, binaries and secrets are in place, so
/// the rerun sees nothing different on disk. It restarts exactly the units the first run did
/// not reach (the markers say which; the SCEP server's went with its restart), removes their
/// markers, and a run after that has nothing to do.
#[test]
fn a_restart_that_a_failed_run_did_not_reach_is_done_by_the_rerun() {
    let (kit, host) = interrupted_install();
    let dir = host.node(MARKERS).unwrap();
    assert_eq!(
        (dir.kind, dir.mode, dir.uid, dir.gid),
        (Kind::Dir, 0o700, 0, 0)
    );
    // The SCEP server was restarted before the failure, so its marker went with its restart.
    assert!(host.node(&marker(CORE[0])).is_none());
    for unit in [CORE[1], CORE[2], PROXY] {
        assert_marker(&host, unit);
    }
    assert_eq!(
        host.paths()
            .iter()
            .filter(|p| p.starts_with(MARKERS))
            .count(),
        4
    );

    // The failure is gone; the services run the old files and nothing else differs.
    host.reply(&[SYSTEMCTL, "restart", "--", CORE[1]], 0, "");
    running(&host, &CORE);
    running(&host, &[PROXY]);
    host.clear_calls();
    let (result, shown) = install(&host, &kit, Mode::Apply);
    result.unwrap();
    assert_eq!(
        changes(&host),
        [
            IP_AND_SS[0].to_owned(),
            IP_AND_SS[1].to_owned(),
            restart_line(CORE[1]),
            restart_line(CORE[2]),
            restart_line(PROXY),
        ]
    );
    for unit in CORE.iter().chain([&PROXY]) {
        assert!(
            host.node(&marker(unit)).is_none(),
            "{unit} is still pending"
        );
    }
    assert!(
        shown.contains(&format!("+ remove {}", marker(PROXY))),
        "{shown}"
    );

    host.clear_calls();
    let (result, shown) = install(&host, &kit, Mode::Apply);
    result.unwrap();
    assert_eq!(changes(&host), IP_AND_SS);
    assert!(shown.ends_with("nothing to do\n"), "{shown}");
}

/// Catches a restart planned only from this run's changes: with nothing different on the host
/// and a marker for the dep server alone, only the dep server is restarted, and its marker is
/// removed; the others are left running.
#[test]
fn only_the_unit_with_a_marker_is_restarted_when_nothing_else_differs() {
    let kit = kit();
    let host = ready_host(&kit);
    ca_is_made_by_init(&host);
    install(&host, &kit, Mode::Apply).0.unwrap();
    running(&host, &CORE);
    host.put_file(&marker(CORE[2]), 0o600, (0, 0), b"");
    host.clear_calls();
    let (result, _) = install(&host, &kit, Mode::Apply);
    result.unwrap();
    assert_eq!(
        changes(&host),
        [
            IP_AND_SS[0].to_owned(),
            IP_AND_SS[1].to_owned(),
            restart_line(CORE[2])
        ]
    );
    assert!(host.node(&marker(CORE[2])).is_none());
}

/// Catches a marker written twice or a restart planned twice: a unit that has a marker and
/// whose binary changes again is restarted once, with no new marker step for it, while the
/// unit without one gets its marker.
#[test]
fn a_unit_with_a_marker_gets_no_second_one_when_its_inputs_change_again() {
    let kit = kit();
    let host = ready_host(&kit);
    ca_is_made_by_init(&host);
    install(&host, &kit, Mode::Apply).0.unwrap();
    running(&host, &CORE);
    host.put_file(&marker(CORE[1]), 0o600, (0, 0), b"");
    let v2 = kit_with(|name, bytes| match name {
        "nanohub" | "depserver" => format!("{name} v2").into_bytes(),
        _ => bytes,
    });
    for (file, bytes) in &v2.archives {
        host.put_file(&format!("/srv/fetch/{file}"), 0o644, (1000, 1000), bytes);
    }
    let (result, shown) = install(&host, &v2, Mode::DryRun);
    result.unwrap();
    let lines: Vec<&str> = shown.lines().filter(|l| l.contains("/run/")).collect();
    assert_eq!(
        lines,
        [
            format!(
                "DRY-RUN: write {} mode 0600 owner root:root (0 bytes)",
                marker(CORE[2])
            ),
            format!("DRY-RUN: remove {}", marker(CORE[1])),
            format!("DRY-RUN: remove {}", marker(CORE[2])),
        ]
    );
    assert_eq!(
        shown
            .matches("systemctl restart -- hostkeeper-mdm-nanohub.service")
            .count(),
        1
    );
}

/// Catches a pending proxy restart done before there is a certificate, or dropped with it:
/// without the certificate the proxy is not restarted and its marker stays; once the
/// certificate is there the proxy is restarted and the marker removed.
#[test]
fn a_proxy_marker_waits_for_the_certificate_and_is_removed_by_its_restart() {
    let kit = kit();
    let host = ready_host(&kit);
    ca_is_made_by_init(&host);
    install(&host, &kit, Mode::Apply).0.unwrap();
    running(&host, &CORE);
    host.put_file(&marker(PROXY), 0o600, (0, 0), b"");
    host.clear_calls();
    install(&host, &kit, Mode::Apply).0.unwrap();
    assert_eq!(changes(&host), IP_AND_SS);
    assert_marker(&host, PROXY);

    host.put_file(CERT, 0o644, (SVC, SVC), b"CERT");
    running(&host, &[PROXY]);
    host.clear_calls();
    install(&host, &kit, Mode::Apply).0.unwrap();
    assert_eq!(
        changes(&host),
        [
            IP_AND_SS[0].to_owned(),
            IP_AND_SS[1].to_owned(),
            restart_line(PROXY)
        ]
    );
    assert!(host.node(&marker(PROXY)).is_none());
}

/// Catches a dry run that writes a marker: it prints the marker steps in their places (the
/// directory and a marker per unit before the plan, each removal right after its restart) and
/// changes nothing, not even the directory under `/run`.
#[test]
fn a_dry_run_prints_the_marker_steps_and_makes_no_marker() {
    let kit = kit();
    let host = ready_host(&kit);
    let before = host.paths();
    let (result, shown) = install(&host, &kit, Mode::DryRun);
    result.unwrap();
    assert_eq!(host.paths(), before);
    let steps: Vec<&str> = shown
        .lines()
        .filter(|l| l.starts_with("DRY-RUN: "))
        .collect();
    let run_steps: Vec<(usize, &str)> = steps
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("/run/"))
        .map(|(i, l)| (i, *l))
        .collect();
    let at = |text: &str| steps.iter().position(|l| *l == text).unwrap();
    let mark = |unit: &str| {
        format!(
            "DRY-RUN: write {} mode 0600 owner root:root (0 bytes)",
            marker(unit)
        )
    };
    let clear = |unit: &str| format!("DRY-RUN: remove {}", marker(unit));
    let restart = |unit: &str| format!("DRY-RUN: systemctl restart -- {unit}");
    let expected: Vec<String> = [
        format!("DRY-RUN: mkdir {MARKERS} mode 0700 owner root:root"),
        mark(CORE[0]),
        mark(CORE[1]),
        mark(CORE[2]),
        clear(CORE[0]),
        clear(CORE[1]),
        clear(CORE[2]),
    ]
    .into();
    let got: Vec<String> = run_steps.iter().map(|(_, l)| (*l).to_owned()).collect();
    assert_eq!(got, expected);
    // The markers are made before the first step of the plan, and each is removed by the step
    // right after its unit's restart.
    let first =
        at("DRY-RUN: mkdir /var/lib/hostkeeper-mdm mode 0700 owner hostkeeper-mdm:hostkeeper-mdm");
    assert!(run_steps[3].0 < first);
    for unit in CORE {
        assert_eq!(at(&clear(unit)), at(&restart(unit)) + 1, "{unit}");
    }
    assert_eq!(changes(&host), IP_AND_SS);
}
