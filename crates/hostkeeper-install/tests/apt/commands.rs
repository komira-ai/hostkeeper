//! How every run is made: its options, its network, its confinement and its environment.

use hostkeeper_install::repository::apt::options;
use hostkeeper_install::repository::{Class, Outcome};
use hostkeeper_install::repository::{Network, Reason, Tool, ToolOutput};

use crate::harness::*;

/// Every call of one full install (the stage, then the activate) of a package with a run unit.
fn full_install() -> Env {
    let env = Env::new("apt-commands");
    env.installed(&mdm_deb(V1), &RUN3);
    env
}

/// Proves every apt run of a stage and an activate starts with exactly the options of section
/// 4.2 for the service's work directory, whose dpkg lock timeout is the step's bound, but for
/// the install, whose timeout is the installer's `lock_wait_secs` and whose run is bounded by the
/// step's bound and that wait together; and that no argument names a host path for the lists,
/// the caches or the sources.
/// Catches: a run that falls back to the host's `/etc/apt` or `/var/lib/apt`, or an install that
/// waits for the lock for the step's bound rather than the daemon's setting.
#[test]
fn every_apt_run_carries_every_option_of_the_work_directory() {
    let env = full_install();
    let work = env.work();
    let mut seen = 0;
    for call in env.host.calls() {
        if !matches!(call.run.tool, Tool::AptGet | Tool::AptCache) {
            continue;
        }
        seen += 1;
        let lock_secs = match call.label.as_str() {
            "apt-get install" => {
                assert_eq!(call.run.bound_secs, ACTIVATE_SECS + LOCK_WAIT_SECS);
                LOCK_WAIT_SECS
            }
            _ => call.run.bound_secs,
        };
        let expected = options(&work, lock_secs);
        assert_eq!(
            call.run.args[..expected.len()],
            expected[..],
            "{}",
            call.label
        );
        for arg in &call.run.args {
            for host_path in [
                "/var/lib/apt",
                "/var/cache/apt",
                "/etc/apt",
                "/var/lib/dpkg",
            ] {
                assert!(!arg.contains(host_path), "{}: {arg}", call.label);
            }
        }
    }
    assert_eq!(seen, 5, "update, show, simulate, download and install");
}

/// Proves the options are the ones section 4.2 lists: the work directory's copy of the one entry
/// as the only source, no trusted key, no preference, lists and archives in the work directory,
/// no package cache, no unauthenticated or insecure repository, no recommends or suggests, and
/// the lock timeout of the bound.
#[test]
fn the_options_are_those_of_section_4_2() {
    let work = std::path::Path::new("/w/mdm");
    let w = "/w/mdm";
    let expected: Vec<String> = [
        "Dir::Etc::SourceList=/dev/null".to_owned(),
        format!("Dir::Etc::SourceParts={w}/sources"),
        "Dir::Etc::Trusted=/dev/null".to_owned(),
        format!("Dir::Etc::TrustedParts={w}/empty"),
        "Dir::Etc::Preferences=/dev/null".to_owned(),
        format!("Dir::Etc::PreferencesParts={w}/empty"),
        format!("Dir::State::Lists={w}/lists"),
        format!("Dir::Cache::Archives={w}/archives"),
        "Dir::Cache::pkgcache=".to_owned(),
        "Dir::Cache::srcpkgcache=".to_owned(),
        "APT::Get::AllowUnauthenticated=false".to_owned(),
        "Acquire::AllowInsecureRepositories=false".to_owned(),
        "Acquire::AllowDowngradeToInsecureRepositories=false".to_owned(),
        "APT::Install-Recommends=false".to_owned(),
        "APT::Install-Suggests=false".to_owned(),
        "DPkg::Lock::Timeout=77".to_owned(),
    ]
    .into_iter()
    .flat_map(|o| ["-o".to_owned(), o])
    .collect();
    assert_eq!(options(work, 77), expected);
}

/// Proves only `apt-get update` and `apt-get download` may reach the network, each confined to
/// writing the work directory, and the download runs in the work directory's archives; every
/// other run, the install and the purge included, is offline.
#[test]
fn only_the_update_and_the_download_fetch_and_both_are_confined() {
    let env = full_install();
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed");
    let work = env.work();
    let calls = env.host.calls();
    assert_eq!(calls.len(), 16);
    for call in &calls {
        let fetching = call.label == "apt-get update" || call.label == "apt-get download";
        let expected = if fetching {
            Network::Fetch
        } else {
            Network::Offline
        };
        assert_eq!(call.run.network, expected, "{}", call.label);
        if fetching {
            assert_eq!(
                call.run.writable,
                Some(vec![work.clone()]),
                "{}",
                call.label
            );
        }
    }
    let download = &env.host.calls_of("apt-get download")[0].run;
    assert_eq!(download.working_dir, Some(work.join("archives")));
    for label in ["apt-get install", "dpkg --purge"] {
        let run = &env.host.calls_of(label)[0].run;
        assert_eq!(run.writable, None, "{label} writes the host");
        assert!(
            run.env
                .contains(&("DEBIAN_FRONTEND".to_owned(), "noninteractive".to_owned()))
        );
    }
    assert!(
        calls
            .iter()
            .all(|c| c.run.env.contains(&("LC_ALL".to_owned(), "C".to_owned())))
    );
}

/// Proves the dpkg-deb runs are confined: reading the fields writes nothing and the extraction
/// writes the tree only, each run's bound is the stage's, and no run reads the control members.
#[test]
fn dpkg_deb_runs_write_only_the_tree_and_read_no_control_members() {
    let env = Env::new("apt-confined");
    let (_, staging) = env.staged(&deb(V1), &["mdm.service"]);
    let field = &env.host.calls_of("dpkg-deb --field")[0].run;
    assert_eq!(field.writable, Some(Vec::new()));
    assert!(env.host.calls_of("dpkg-deb --control").is_empty());
    assert!(!env.work().join("control").exists());
    let extract = &env.host.calls_of("dpkg-deb --extract")[0].run;
    assert_eq!(extract.writable, Some(vec![staging.join("tree")]));
    for label in ["dpkg-deb --field", "dpkg-deb --extract"] {
        assert_eq!(env.host.calls_of(label)[0].run.bound_secs, STAGE_SECS);
    }
    let arch = &env.host.calls_of("dpkg --print-architecture")[0].run;
    assert_eq!(arch.writable, Some(Vec::new()));
}

/// Proves section 4.5's table: each reason's class and its outcome.
#[test]
fn each_reason_has_the_class_and_outcome_of_section_4_5() {
    use Class::{Environment as E, Release as R};
    use Outcome::{Deferred as D, Failed as F};
    let table = [
        (Reason::RepositoryUnknown, E, F),
        (Reason::FetchFailed, E, D),
        (Reason::Signature, E, F),
        (Reason::NotInRepository, E, D),
        (Reason::PackageDigestMismatch, R, F),
        (Reason::PackageTooLarge, R, F),
        (Reason::StoreDigestMismatch, E, D),
        (Reason::ManifestInvalid, R, F),
        (Reason::PackageScripts, R, F),
        (Reason::PackageConflict, R, F),
        (Reason::DependencyMissing, E, F),
        (Reason::DependencyChanges, E, F),
        (Reason::PackageManagerBusy, E, D),
        (Reason::DpkgInterrupted, E, F),
        (Reason::DpkgError, R, F),
        (Reason::OrderInvalid, R, F),
        (Reason::CoordinatorTooOld, E, D),
        (Reason::IoError, E, F),
        (Reason::UnsafeDirectory, E, F),
        (Reason::InstallerError, R, F),
        (Reason::UnitNotLoaded, E, F),
    ];
    assert_eq!(table.len(), Reason::ALL.len());
    for (reason, class, outcome) in table {
        assert_eq!(
            (reason.class(), reason.outcome()),
            (class, outcome),
            "{reason:?}"
        );
        assert_eq!(
            reason.class().token(),
            if class == R { "release" } else { "environment" }
        );
    }
    let tokens: std::collections::BTreeSet<_> = Reason::ALL.iter().map(|r| r.token()).collect();
    assert_eq!(tokens.len(), Reason::ALL.len());
}

/// Proves each tool's path is `/usr/bin/<name>` and its name is unique.
#[test]
fn every_tool_is_run_by_its_absolute_path() {
    let mut names = std::collections::BTreeSet::new();
    for tool in Tool::ALL {
        assert_eq!(tool.path(), format!("/usr/bin/{}", tool.name()));
        assert!(names.insert(tool.name()));
    }
    assert_eq!(names.len(), 7);
}

/// Proves a failure report quotes apt's last `E:` line first, else the last line of the standard
/// error, else of the standard output, and that a success is an exit of 0 only.
#[test]
fn the_last_line_of_an_output_is_the_one_a_failure_quotes() {
    let mut out = ToolOutput::exit(100, "out one\nout two\n");
    assert_eq!(out.last_line(), "out two");
    out.stderr = b"err one\n\nerr two\n".to_vec();
    assert_eq!(out.last_line(), "err two");
    out.stderr = b"E: first\nW: later\n".to_vec();
    out.stdout = b"plain\n".to_vec();
    assert_eq!(out.last_line(), "E: first");
    assert_eq!(ToolOutput::default().last_line(), "");
    assert!(!out.succeeded());
    assert!(ToolOutput::exit(0, "").succeeded());
    assert!(
        !ToolOutput {
            code: None,
            ..ToolOutput::default()
        }
        .succeeded()
    );
    assert!(out.text().contains("plain") & out.text().contains("W: later"));
}

/// Proves the host's pause returns (it sleeps for the seconds it is given, here none), and that
/// a wait reported to it returns at once: the host's pause reports nowhere.
#[test]
fn the_hosts_pause_returns() {
    use hostkeeper_install::repository::{LockWait, Pause, tools::Sleep};
    Sleep.pause(0);
    Sleep.lock_wait(&LockWait {
        tool: Tool::Dpkg,
        waited_secs: 0,
        bound_secs: 1,
        holder: String::new(),
    });
}

/// Proves the record's JSON form leaves out an empty link set and a false `pending`, and reads an
/// entry that lacks them as no links and not pending.
#[test]
fn the_record_has_a_compact_json_form() {
    use hostkeeper_install::repository::PackageRecord;
    let record = PackageRecord {
        installer: "apt".to_owned(),
        package: PACKAGE.to_owned(),
        version: V1.to_owned(),
        repository: REPO.to_owned(),
        links: Default::default(),
        pending: false,
    };
    let json = serde_json::to_value(&record).expect("json");
    assert_eq!(
        json,
        serde_json::json!({"installer": "apt", "package": "mdm", "version": V1, "repository": "main"})
    );
    assert_eq!(
        serde_json::from_value::<PackageRecord>(json).expect("read"),
        record
    );
    let pending = PackageRecord {
        pending: true,
        links: run3_links(),
        ..record
    };
    let json = serde_json::to_value(&pending).expect("json");
    assert_eq!(json["pending"], true);
    assert_eq!(json["links"].as_array().map(Vec::len), Some(3));
}
