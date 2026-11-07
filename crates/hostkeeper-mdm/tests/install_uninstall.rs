//! `install --uninstall`: the order of what it stops and moves, and the snapshots it keeps.

mod common;
mod install_support;

use common::{acme, config, kit, ready_host};
use hostkeeper_mdm_core::render::{UnitNames, render, render_tls};
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Mode};
use install_support::{
    CORE, STATE, UNITS, ca_is_made_by_init, changes, install, marker, uninstall, uninstall_at,
};

const CERT_SERVICE: &str = "hostkeeper-mdm-cert.service";
const RELOAD_SERVICE: &str = "hostkeeper-mdm-proxy-reload.service";
const DISABLE: &str = "/usr/bin/systemctl disable --now -- hostkeeper-mdm-scep.service hostkeeper-mdm-nanohub.service hostkeeper-mdm-depserver.service hostkeeper-mdm-depsyncer.service hostkeeper-mdm-proxy.service";

/// A host with the kit installed, and the certificate job's units too when `jobs`.
fn installed(jobs: bool) -> FakeHost {
    let kit = kit();
    let host = ready_host(&kit);
    ca_is_made_by_init(&host);
    install(&host, &kit, Mode::Apply).0.unwrap();
    if jobs {
        for unit in render_tls(&config(), &acme()).unwrap() {
            host.put_file(
                &format!("{UNITS}/{}", unit.name),
                0o644,
                (0, 0),
                unit.text.as_bytes(),
            );
        }
    }
    host
}

/// Catches an uninstall that leaves the certificate or reload job running: after the timer,
/// the path unit and the services are disabled and stopped, the two jobs are stopped (one
/// command), then the files are moved, then systemd reloads; the commands in exactly that
/// order.
#[test]
fn uninstall_stops_the_certificate_and_reload_jobs_before_moving_their_files() {
    let host = installed(true);
    let (result, shown) = uninstall(&host, Mode::DryRun);
    result.unwrap();
    let steps: Vec<&str> = shown
        .lines()
        .filter(|l| l.starts_with("DRY-RUN: "))
        .collect();
    let disable = format!(
        "DRY-RUN: systemctl {}",
        DISABLE.strip_prefix("/usr/bin/systemctl ").unwrap()
    );
    assert_eq!(
        steps[0],
        format!("{disable} hostkeeper-mdm-cert.timer hostkeeper-mdm-proxy-reload.path")
    );
    assert_eq!(
        steps[1],
        format!("DRY-RUN: run /usr/bin/systemctl stop -- {CERT_SERVICE} {RELOAD_SERVICE}")
    );
    assert!(steps[2].starts_with(&format!("DRY-RUN: mkdir {STATE}/removed/1700000000 ")));
    assert_eq!(steps.last().unwrap(), &"DRY-RUN: systemctl daemon-reload");

    host.clear_calls();
    uninstall(&host, Mode::Apply).0.unwrap();
    assert_eq!(
        changes(&host),
        [
            format!("{DISABLE} hostkeeper-mdm-cert.timer hostkeeper-mdm-proxy-reload.path"),
            format!("/usr/bin/systemctl stop -- {CERT_SERVICE} {RELOAD_SERVICE}"),
            "/usr/bin/systemctl daemon-reload".to_owned(),
        ]
    );
}

/// Catches a stop of a job that is not installed, and a stop planned when no job is: with
/// only the certificate job installed only it is stopped; with neither there is no stop.
#[test]
fn uninstall_stops_only_the_jobs_that_are_installed() {
    let host = installed(false);
    host.clear_calls();
    uninstall(&host, Mode::Apply).0.unwrap();
    assert_eq!(
        changes(&host),
        [
            DISABLE.to_owned(),
            "/usr/bin/systemctl daemon-reload".to_owned()
        ]
    );

    let host = installed(false);
    let tls = render_tls(&config(), &acme()).unwrap();
    let cert = tls.iter().find(|u| u.name == CERT_SERVICE).unwrap();
    host.put_file(
        &format!("{UNITS}/{CERT_SERVICE}"),
        0o644,
        (0, 0),
        cert.text.as_bytes(),
    );
    host.clear_calls();
    uninstall(&host, Mode::Apply).0.unwrap();
    assert_eq!(
        changes(&host),
        [
            DISABLE.to_owned(),
            format!("/usr/bin/systemctl stop -- {CERT_SERVICE}"),
            "/usr/bin/systemctl daemon-reload".to_owned(),
        ]
    );
}

/// Catches a second uninstall in the same second overwriting the first one's snapshot: three
/// uninstalls at one injected time, the kit put back (and its units edited) between them, keep
/// three snapshots, `<now>`, `<now>-1` and `<now>-2`, each holding the files of its own run.
#[test]
fn uninstalls_in_the_same_second_keep_every_snapshot() {
    let kit = kit();
    let host = installed(false);
    let slice = UnitNames::new(&config()).slice;
    let rendered = render(&config()).unwrap();
    let text = |unit: &str| {
        let found = rendered.units.iter().find(|u| u.name == unit).unwrap();
        found.text.clone().into_bytes()
    };
    let at = 1_700_000_000;
    uninstall_at(&host, Mode::Apply, at).0.unwrap();

    for (suffix, edit) in [
        ("-1", b"edited once".to_vec()),
        ("-2", b"edited twice".to_vec()),
    ] {
        install(&host, &kit, Mode::Apply).0.unwrap();
        host.put_file(&format!("{UNITS}/{slice}"), 0o644, (0, 0), &edit);
        let (result, shown) = uninstall_at(&host, Mode::DryRun, at);
        result.unwrap();
        assert!(
            shown.contains(&format!("DRY-RUN: mkdir {STATE}/removed/{at}{suffix} ")),
            "{shown}"
        );
        uninstall_at(&host, Mode::Apply, at).0.unwrap();
        let moved = host
            .node(&format!("{STATE}/removed/{at}{suffix}/{slice}"))
            .unwrap();
        assert_eq!(moved.bytes, edit);
    }
    let first = host.node(&format!("{STATE}/removed/{at}/{slice}")).unwrap();
    assert_eq!(first.bytes, text(&slice));
    let other = host
        .node(&format!(
            "{STATE}/removed/{at}-1/hostkeeper-mdm-scep.service"
        ))
        .unwrap();
    assert_eq!(other.bytes, text("hostkeeper-mdm-scep.service"));
    assert!(host.node(&format!("{STATE}/removed/{at}-3")).is_none());
    assert!(host.stat(&format!("{UNITS}/{slice}")).unwrap().is_none());
}

/// Catches a pending restart outliving the units it is for: an uninstall removes the markers
/// of the services, so the install after it starts them and restarts nothing for an old run.
#[test]
fn uninstall_removes_the_pending_restart_markers() {
    let host = installed(false);
    for unit in [CORE[0], CORE[2]] {
        host.put_file(&marker(unit), 0o600, (0, 0), b"");
    }
    let (result, shown) = uninstall(&host, Mode::DryRun);
    result.unwrap();
    let removals: Vec<&str> = shown.lines().filter(|l| l.contains("/run/")).collect();
    assert_eq!(
        removals,
        [
            format!("DRY-RUN: remove {}", marker(CORE[0])),
            format!("DRY-RUN: remove {}", marker(CORE[2])),
        ]
    );
    uninstall(&host, Mode::Apply).0.unwrap();
    for unit in CORE {
        assert!(host.node(&marker(unit)).is_none(), "{unit}");
    }
}
