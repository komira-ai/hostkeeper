//! `activate`: the install from the version's own copy, the hold, the enablement, the record, the
//! rollback and a crash between the install and the record.

use hostkeeper_install::repository::{Class, Outcome, Reason};

use crate::harness::*;

use Class::Release;
use Outcome::Failed;

fn read_link(env: &Env, link: &str) -> Option<String> {
    std::fs::read_link(at_root(env, link))
        .ok()
        .map(|p| p.display().to_string())
}

/// Proves an activate records the version pending before the install, installs with no download,
/// shows the version, holds it, enables the run units by real links under the host's root,
/// reloads the service manager, and records it done with its links; every run is offline, under
/// the installer lock.
#[test]
fn an_activate_installs_holds_enables_and_records() {
    let env = Env::new("apt-activate");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    let activated = env.installer().activate(&order, &dir).expect("activates");
    assert_eq!(activated.drift, None);
    let held = env.host.installed(PACKAGE).expect("installed");
    assert_eq!(
        (held.version.as_str(), held.state, held.held),
        (V1, 'i', true)
    );
    let record = env.records.get(SERVICE).expect("a record");
    assert_eq!(
        record,
        hostkeeper_install::repository::PackageRecord {
            installer: "apt".to_owned(),
            package: PACKAGE.to_owned(),
            version: V1.to_owned(),
            repository: REPO.to_owned(),
            links: run3_links(),
            pending: false,
        }
    );
    for (link, unit) in [
        ("multi-user.target.wants/mdm.service", "mdm.service"),
        (
            "multi-user.target.wants/mdm-worker.service",
            "mdm-worker.service",
        ),
        ("timers.target.wants/mdm.timer", "mdm.timer"),
    ] {
        let target = read_link(&env, &format!("/etc/systemd/system/{link}"));
        assert_eq!(target, Some(format!("/usr/lib/systemd/system/{unit}")));
    }
    let log = env.event_log();
    let at = |needle: &str| log.iter().position(|e| e == needle).expect(needle);
    assert!(at(&format!("save pending {V1}")) < at("run apt-get install"));
    assert!(at("run apt-get install") < at("run apt-mark hold"));
    assert!(at("run apt-mark hold") < at("run systemctl daemon-reload"));
    assert!(at("run systemctl daemon-reload") < at(&format!("save done {V1}")));
    let install = &env.host.calls_of("apt-get install")[0];
    for flag in [
        "--no-download",
        "--allow-downgrades",
        "--allow-change-held-packages",
        "-y",
    ] {
        assert!(install.run.args.iter().any(|a| a == flag), "{flag}");
    }
    assert!(
        install
            .run
            .args
            .iter()
            .any(|a| a == "Dpkg::Options::=--force-confold")
    );
    let activate_calls: Vec<_> = env.host.calls().into_iter().skip(8).collect();
    assert!(activate_calls.iter().all(|c| c.locked));
    assert_eq!(
        env.host.calls_of("apt-get download").len(),
        1,
        "the stage's download only"
    );
    assert_eq!(
        env.host.calls_of("apt-get update").len(),
        1,
        "no fetch in the activate"
    );
}

/// Proves an activate run twice is the same as run once: the host's packages, the record and the
/// links are equal after the second run, which finds no drift.
#[test]
fn an_activate_twice_is_an_activate_once() {
    let env = Env::new("apt-idempotent");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    env.installer().activate(&order, &dir).expect("first");
    let once = env.snapshot();
    let second = env.installer().activate(&order, &dir).expect("second");
    assert_eq!(second.drift, None);
    assert_eq!(env.snapshot(), once);
}

/// Proves a later version that adds a run unit has it enabled, and a later one that drops it
/// has its link taken away and out of the record, the others kept.
#[test]
fn a_run_unit_added_is_enabled_and_one_dropped_loses_its_link() {
    let env = Env::new("apt-units-change");
    let (o1, d1) = env.staged(&mdm_deb(V1), &["mdm.service"]);
    env.installer().activate(&o1, &d1).expect("v1");
    assert_eq!(env.records.get(SERVICE).expect("record").links.len(), 1);
    let (o2, d2) = env.staged(&mdm_deb(V2), &RUN3);
    env.installer().activate(&o2, &d2).expect("v2");
    assert_eq!(
        env.records.get(SERVICE).expect("record").links,
        run3_links()
    );
    assert!(at_root(&env, "/etc/systemd/system/timers.target.wants/mdm.timer").is_symlink());
    let (o3, d3) = env.staged(&mdm_deb(V3), &["mdm.service", "mdm-worker.service"]);
    env.installer().activate(&o3, &d3).expect("v3");
    let links = env.records.get(SERVICE).expect("record").links;
    assert_eq!(links.len(), 2);
    assert!(!at_root(&env, "/etc/systemd/system/timers.target.wants/mdm.timer").exists());
    assert!(
        at_root(
            &env,
            "/etc/systemd/system/multi-user.target.wants/mdm-worker.service"
        )
        .is_symlink()
    );
}

/// Proves a link that is already there and that hostkeeper did not write is left as it is and
/// not recorded, a link of another target included, and a recorded link that was replaced by
/// something else is no longer claimed.
#[test]
fn a_link_not_written_by_hostkeeper_is_left_and_not_recorded() {
    let env = Env::new("apt-foreign-link");
    let wants = at_root(&env, "/etc/systemd/system/multi-user.target.wants");
    std::fs::create_dir_all(&wants).expect("the directory");
    std::os::unix::fs::symlink(
        "/usr/lib/systemd/system/mdm.service",
        wants.join("mdm.service"),
    )
    .expect("a link");
    std::os::unix::fs::symlink("/elsewhere", wants.join("mdm-worker.service")).expect("a link");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    env.installer().activate(&order, &dir).expect("activates");
    let links = env.records.get(SERVICE).expect("record").links;
    assert_eq!(links.len(), 1, "{links:?}");
    assert!(links.contains("/etc/systemd/system/timers.target.wants/mdm.timer"));
    assert_eq!(
        read_link(
            &env,
            "/etc/systemd/system/multi-user.target.wants/mdm-worker.service"
        ),
        Some("/elsewhere".to_owned())
    );
    // A link hostkeeper recorded, replaced since by one that points elsewhere, is not claimed.
    std::fs::remove_file(wants.join("mdm.service")).expect("removed");
    std::os::unix::fs::symlink("/elsewhere", wants.join("mdm.service")).expect("a link");
    let timer = at_root(&env, "/etc/systemd/system/timers.target.wants/mdm.timer");
    std::fs::remove_file(&timer).expect("removed");
    std::os::unix::fs::symlink("/elsewhere", &timer).expect("a link");
    env.installer().activate(&order, &dir).expect("again");
    assert!(env.records.get(SERVICE).expect("record").links.is_empty());
}

/// Proves a crash between the install and the record resumes to the same state: the second save
/// fails after the install, the record stays pending, and an activate again ends exactly where an
/// activate that did not crash ends; a pending record is no drift.
#[test]
fn a_crash_between_the_install_and_the_record_resumes() {
    let crashed = Env::new("apt-crash");
    let (order, dir) = crashed.staged(&mdm_deb(V1), &RUN3);
    crashed.records.faults(|f| f.save_nth = Some(1));
    let failure = crashed
        .installer()
        .activate(&order, &dir)
        .expect_err("the save fails");
    assert_eq!(failure.reason(), Reason::IoError);
    assert!(failure.detail().starts_with("the installed record"));
    assert!(crashed.records.get(SERVICE).expect("record").pending);
    assert_eq!(
        crashed.host.installed(PACKAGE).expect("installed").version,
        V1
    );
    let drift = crashed
        .installer()
        .drift(&service(SERVICE), PACKAGE, 5)
        .expect("drift");
    assert_eq!(drift, None);
    let resumed = crashed.installer().activate(&order, &dir).expect("resumes");
    assert_eq!(resumed.drift, None);

    let calm = Env::new("apt-no-crash");
    let (order, dir) = calm.staged(&mdm_deb(V1), &RUN3);
    calm.installer().activate(&order, &dir).expect("activates");
    let (a, b) = (crashed.snapshot(), calm.snapshot());
    assert_eq!(a.installed, b.installed);
    assert_eq!(a.root, b.root);
    let (ra, rb) = (&a.records[SERVICE], &b.records[SERVICE]);
    assert_eq!(
        (&ra.version, &ra.repository, ra.pending),
        (&rb.version, &rb.repository, rb.pending)
    );
}

/// Proves the links a run wrote before it crashed are the record's after the activate that
/// resumes it: the resumed record names every link, so a remove takes them away.
#[test]
fn a_resumed_activate_records_the_links_the_crashed_run_wrote() {
    let env = Env::new("apt-crash-links");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    env.records.faults(|f| f.save_nth = Some(1));
    env.installer()
        .activate(&order, &dir)
        .expect_err("the save fails");
    env.installer().activate(&order, &dir).expect("resumes");
    assert_eq!(
        env.records.get(SERVICE).expect("record").links,
        run3_links()
    );
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    for link in run3_links() {
        assert!(
            !at_root(&env, &link).is_symlink(),
            "{link} outlived the remove"
        );
    }
}

/// Proves a maintainer script that fails is `dpkg_error`, failed, release, quoting apt's last
/// error line; the rollback is an activate of the previous version's directory, which runs no
/// update and no download, works with the repository emptied, and leaves the previous version
/// installed, held, enabled and recorded.
#[test]
fn a_failing_maintainer_script_rolls_back_with_no_fetch() {
    let env = Env::new("apt-rollback");
    let (o1, d1) = env.installed(&mdm_deb(V1), &["mdm.service"]);
    let (o2, d2) = env.staged(&mdm_deb(V2), &RUN3);
    env.host
        .edit(|s| s.faults.script_fails_for = Some(V2.to_owned()));
    let failure = env
        .installer()
        .activate(&o2, &d2)
        .expect_err("the install fails");
    assert_failure(&failure, Reason::DpkgError, Release, Failed);
    assert!(
        failure
            .detail()
            .contains("Sub-process /usr/bin/dpkg returned an error code (1)")
    );
    assert_eq!(env.host.installed(PACKAGE).expect("installed").state, 'F');
    env.host.empty_repositories();
    let mark = env.host.labels().len();
    env.installer()
        .activate(&o1, &d1)
        .expect("the rollback activates");
    let after: Vec<String> = env.host.labels().into_iter().skip(mark).collect();
    for fetch in [
        "apt-get update",
        "apt-get download",
        "apt-cache show",
        "apt-get simulate",
    ] {
        assert!(
            !after.iter().any(|l| l == fetch),
            "{fetch} ran in the rollback"
        );
    }
    assert!(after.iter().any(|l| l == "apt-get install"));
    let now = env.host.installed(PACKAGE).expect("installed");
    assert_eq!((now.version.as_str(), now.state, now.held), (V1, 'i', true));
    let record = env.records.get(SERVICE).expect("record");
    assert_eq!((record.version.as_str(), record.pending), (V1, false));
    assert_eq!(record.links.len(), 1);
}

/// Proves a rollback to a version a repository no longer lists, whose index's `Valid-Until` has
/// passed, installs from the kept copy: the install is told the validity check is off.
#[test]
fn a_rollback_does_not_ask_the_repository_or_its_validity() {
    let env = Env::new("apt-rollback-valid");
    let (o1, d1) = env.installed(&deb(V1), &["mdm.service"]);
    env.host.empty_repositories();
    env.host
        .set_health(URI, crate::apt_fake::Health::Unreachable);
    env.installer()
        .activate(&o1, &d1)
        .expect("activates from the kept copy");
    let install = &env.host.calls_of("apt-get install")[1].run;
    assert!(
        install
            .args
            .windows(2)
            .any(|w| w == ["-o", "Acquire::Check-Valid-Until=false"])
    );
    assert_eq!(env.host.calls_of("apt-get update").len(), 1);
}

/// Proves a first install that fails is taken off with a purge: the failed activate leaves a
/// pending record and a half-configured package, and a remove purges it and drops the record.
#[test]
fn a_first_install_that_fails_is_removed_by_a_purge() {
    let env = Env::new("apt-first-fails");
    let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
    env.host
        .edit(|s| s.faults.script_fails_for = Some(V1.to_owned()));
    let failure = env.installer().activate(&order, &dir).expect_err("fails");
    assert_eq!(failure.reason(), Reason::DpkgError);
    let removed = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    assert_eq!(removed.drift, None, "a pending record names no drift");
    assert!(env.host.installed(PACKAGE).is_none());
    assert!(env.records.get(SERVICE).is_none());
}

/// Proves an install that exits 0 but leaves another version, a half-configured package or no
/// package is `dpkg_error`, failed, release, naming what the database shows.
#[test]
fn an_install_the_database_does_not_show_is_a_dpkg_error() {
    type Fault = fn(&mut crate::apt_fake::Faults);
    let cases: [(&str, Fault, &str); 3] = [
        (
            "another version",
            |f| f.leaves = Some("0.9-1".to_owned()),
            "0.9-1 (installed)",
        ),
        (
            "half-configured",
            |f| f.incomplete = true,
            "(not fully installed)",
        ),
        ("no package", |f| f.noop = true, "no version"),
    ];
    for (what, fault, shown) in cases {
        let env = Env::new("apt-not-shown");
        let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
        env.host.edit(|s| fault(&mut s.faults));
        let failure = env.installer().activate(&order, &dir).expect_err(what);
        assert_failure(&failure, Reason::DpkgError, Release, Failed);
        assert!(failure.detail().contains(shown), "{what}: {failure}");
        assert!(!env.has_run("apt-mark hold"), "{what}: held");
    }
}

/// Proves the lock held past the bound at the install is `package_manager_busy`, an interrupted
/// dpkg is `dpkg_interrupted` and an unmet dependency at the install is `dependency_missing`.
#[test]
fn the_install_reports_the_hosts_state() {
    type Fault = fn(&mut crate::apt_fake::Faults);
    let cases: [(Fault, Reason); 3] = [
        (
            |f| {
                f.busy.insert("install", 1);
            },
            Reason::PackageManagerBusy,
        ),
        (|f| f.interrupted = true, Reason::DpkgInterrupted),
        (|f| f.unmet = true, Reason::DependencyMissing),
    ];
    for (fault, reason) in cases {
        let env = Env::new("apt-install-state");
        let (order, dir) = env.staged(&deb(V1), &["mdm.service"]);
        env.host.edit(|s| fault(&mut s.faults));
        let failure = env.installer().activate(&order, &dir).expect_err("refused");
        assert_eq!(failure.reason(), reason);
    }
}
