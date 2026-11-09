//! `stage`: each row of packages-repositories.md section 4.5, and the order of its checks.

use hostkeeper_install::repository::{Class, Network, Outcome, Reason};

use crate::apt_fake::{Download, Health, Sig};
use crate::harness::*;

use Class::{Environment, Release};
use Outcome::{Deferred, Failed};

/// Stages `order` in a fresh staging directory, expects a refusal and that the host is as it was.
fn refused(
    env: &Env,
    order: &hostkeeper_install::repository::Order,
) -> hostkeeper_install::repository::Failure {
    let before = env.snapshot();
    let (result, staging) = env.stage(order);
    let failure = result.expect_err("the stage is refused");
    env.assert_unchanged(&before);
    assert!(
        !staging.join("apt").exists(),
        "a copy of the index was kept"
    );
    failure
}

/// Proves a stage of a good package writes the tree and the backend's copy of the index and the
/// package into the staging directory, keeps no lock file of apt's, and changes nothing else.
#[test]
fn a_good_package_stages_its_tree_and_its_copy_of_the_index() {
    let env = Env::new("apt-stage-ok");
    let package = deb(V1);
    let order = env.publish(&package);
    let (staged, staging) = env.stage(&order);
    assert!(!staged.expect("staged").hook);
    assert!(staging.join("tree/usr/bin/mdm").is_file());
    assert!(
        staging
            .join("tree/usr/lib/systemd/system/mdm.service")
            .is_file()
    );
    assert!(staging.join("apt/main.sources").is_file());
    assert!(staging.join("apt/archives/mdm_1.4.0-1_amd64.deb").is_file());
    let lists = listing(&staging.join("apt/lists"));
    assert_eq!(lists.len(), 2, "{lists:?}");
    assert!(lists.iter().all(|f| f != "lock" && f != "partial/"));
    assert!(env.records.saved().is_empty());
    assert!(env.host.all_installed().is_empty());
    assert!(listing(&env.dirs.root).is_empty());
    let sources = std::fs::read_to_string(staging.join("apt/main.sources")).expect("sources");
    assert!(sources.contains(URI));
}

/// Proves the checks run in section 4.3's order: the update, the architecture, the stanza, the
/// dependency check under the installer lock, then the download and the package's own checks;
/// only the dependency check holds the lock; nothing is downloaded before it.
#[test]
fn the_simulation_runs_before_any_download_and_under_the_installer_lock() {
    let env = Env::new("apt-order");
    env.installed(&deb(V1), &["mdm.service"]);
    let calls = env.host.calls();
    let labels: Vec<&str> = calls.iter().take(8).map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "apt-get update",
            "dpkg --print-architecture",
            "apt-cache show",
            "apt-get simulate",
            "apt-get download",
            "dpkg-deb --field",
            "dpkg-deb --control",
            "dpkg-deb --extract",
        ]
    );
    for call in &calls[..8] {
        assert_eq!(
            call.locked,
            call.label == "apt-get simulate",
            "{}",
            call.label
        );
    }
    for call in &calls[8..] {
        assert!(call.locked, "{} ran without the installer lock", call.label);
    }
}

/// Proves a package that is installed and held is updated: the simulation passes the option that
/// lets it change a held package.
#[test]
fn a_held_package_is_updated() {
    let env = Env::new("apt-held");
    env.installed(&deb(V1), &["mdm.service"]);
    assert!(env.host.installed(PACKAGE).expect("installed").held);
    let (order, dir) = env.staged(&deb(V2), &["mdm.service"]);
    env.installer()
        .activate(&order, &dir)
        .expect("the update activates");
    let now = env.host.installed(PACKAGE).expect("installed");
    assert_eq!((now.version.as_str(), now.held), (V2, true));
}

/// Proves an order whose repository is not an entry of the host's configuration is
/// `repository_unknown`, failed, environment, before any tool runs.
#[test]
fn an_unknown_repository_is_refused_before_anything_runs() {
    let env = Env::new("apt-repo-unknown");
    let mut order = env.publish(&deb(V1));
    order.repository = "elsewhere".to_owned();
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::RepositoryUnknown, Environment, Failed);
    assert!(env.host.calls().is_empty());
}

/// Proves a repository that cannot be reached is `fetch_failed`, deferred, after the update and
/// before any other run.
#[test]
fn an_unreachable_repository_defers() {
    let env = Env::new("apt-unreachable");
    let order = env.publish(&deb(V1));
    env.host.set_health(URI, Health::Unreachable);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::FetchFailed, Environment, Deferred);
    assert_eq!(env.host.labels(), ["apt-get update"]);
}

/// Proves a repository the host has no entry for in its configuration as seen by apt (the work
/// directory's sources name a URI the fake does not serve) is also a fetch failure.
#[test]
fn a_uri_nobody_serves_defers() {
    let env = Env::new("apt-nobody");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.repos.clear());
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::FetchFailed, Environment, Deferred);
}

/// Proves each form of apt's signature failure, gpgv's four tokens, sqv's two and the two
/// `Valid-Until` and "no longer signed" lines, is `signature`, failed, environment, and that
/// nothing is downloaded.
#[test]
fn every_signature_failure_is_a_signature_reason() {
    for sig in Sig::ALL {
        let env = Env::new("apt-signature");
        let order = env.publish(&deb(V1));
        env.host.set_health(URI, Health::Signature(sig));
        let failure = refused(&env, &order);
        assert_failure(&failure, Reason::Signature, Environment, Failed);
        assert_eq!(env.host.labels(), ["apt-get update"], "{sig:?}");
    }
}

/// Proves a version the index does not list is `not_in_repository`, deferred, before any
/// download.
#[test]
fn a_missing_version_is_not_in_the_repository() {
    let env = Env::new("apt-missing");
    env.publish(&deb(V2));
    let order = order(&deb(V1), &["mdm.service"]);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::NotInRepository, Environment, Deferred);
    assert!(!env.has_run("apt-get download"));
}

/// Proves a version listed for another architecture only is `not_in_repository` too, and the
/// same stanza for `all` is accepted.
#[test]
fn a_stanza_of_another_architecture_only_is_not_in_the_repository() {
    let env = Env::new("apt-arch");
    let package = deb(V1);
    let order = env.publish(&package);
    env.host.each_entry(|e| e.arch = "arm64".to_owned());
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::NotInRepository, Environment, Deferred);
    env.host.each_entry(|e| e.arch = "all".to_owned());
    let (staged, _) = env.stage(&order);
    staged.expect("a stanza of architecture all is the host's");
}

/// Proves a package whose index digest is not the order's pin is `package_digest_mismatch`,
/// failed, release, and nothing is downloaded.
#[test]
fn an_index_digest_other_than_the_pin_is_refused() {
    let env = Env::new("apt-digest");
    let order = env.publish(&deb(V1));
    env.host.each_entry(|e| e.sha256 = "0".repeat(64));
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::PackageDigestMismatch, Release, Failed);
    assert!(!env.has_run("apt-get download"));
}

/// Proves a downloaded file that is not the pin is `store_digest_mismatch`, deferred.
#[test]
fn downloaded_bytes_that_are_not_the_pin_defer() {
    let env = Env::new("apt-store");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.download = Download::WrongBytes);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::StoreDigestMismatch, Environment, Deferred);
    assert!(!env.has_run("dpkg-deb --field"));
}

/// Proves apt's own hash failure on the download is `store_digest_mismatch`, deferred.
#[test]
fn apts_hash_failure_defers() {
    let env = Env::new("apt-hash");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.download = Download::HashFailure);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::StoreDigestMismatch, Environment, Deferred);
}

/// Proves a download that fails otherwise is `fetch_failed`, deferred.
#[test]
fn a_download_that_cannot_reach_the_repository_defers() {
    let env = Env::new("apt-download-fails");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.download = Download::Unreachable);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::FetchFailed, Environment, Deferred);
}

/// Proves an index size one over `max_package_bytes` is `package_too_large`, failed, release,
/// with nothing downloaded, and a size of exactly `max_package_bytes` is accepted.
#[test]
fn the_size_limit_is_inclusive() {
    let env = Env::new("apt-size");
    let order = env.publish(&deb(V1));
    env.host.each_entry(|e| e.size = MAX + 1);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::PackageTooLarge, Release, Failed);
    assert!(!env.has_run("apt-get download"));
    env.host.each_entry(|e| e.size = MAX);
    let (staged, _) = env.stage(&order);
    staged.expect("a package of exactly max_package_bytes stages");
}

/// Proves a package whose control fields are not the order's is `manifest_invalid`, failed,
/// release, one case for each of Package, Version and Architecture, and architecture `all` is
/// accepted.
#[test]
fn each_control_field_is_checked_and_all_is_accepted() {
    let cases = [
        (
            "Package",
            deb(V1).mutate(|d| d.package = "other".to_owned()),
        ),
        (
            "Version",
            deb(V1).mutate(|d| d.version = "9.9-1".to_owned()),
        ),
        (
            "Architecture",
            deb(V1).mutate(|d| d.arch = "arm64".to_owned()),
        ),
    ];
    for (field, package) in cases {
        let env = Env::new("apt-fields");
        let mut order = env.publish(&package);
        // The index says what the order asks; the file says otherwise.
        env.host.each_entry(|e| {
            e.package = PACKAGE.to_owned();
            e.version = V1.to_owned();
            e.arch = "amd64".to_owned();
        });
        order.package = PACKAGE.to_owned();
        order.package_version = V1.to_owned();
        let failure = refused(&env, &order);
        assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
        assert!(
            failure.detail().contains("not mdm 1.4.0-1"),
            "{field}: {failure}"
        );
    }
    let env = Env::new("apt-fields-all");
    let all = deb(V1).mutate(|d| d.arch = "all".to_owned());
    let order = env.publish(&all);
    env.host.each_entry(|e| e.arch = "amd64".to_owned());
    let (staged, _) = env.stage(&order);
    staged.expect("a package of architecture all stages");
}

/// Proves a `run` unit that is not a unit file of the package is `manifest_invalid`.
#[test]
fn a_run_unit_not_in_the_tree_is_refused() {
    let env = Env::new("apt-run-missing");
    let mut order = env.publish(&deb(V1));
    order.run = vec!["mdm.service".to_owned(), "mdm-ghost.service".to_owned()];
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
    assert!(failure.detail().contains("mdm-ghost.service"));
}

/// Proves a run unit whose `WantedBy` is not a unit name is `manifest_invalid`, in the stage.
#[test]
fn a_unit_wanted_by_a_bad_name_is_refused() {
    let env = Env::new("apt-bad-target");
    let package = deb(V1).file(
        &format!("{UNIT_DIR}/mdm.service"),
        0o644,
        &unit(Some("not a name")),
    );
    let order = env.publish(&package);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::ManifestInvalid, Release, Failed);
}

/// Proves another service's record of the package is `package_conflict`, failed, release, before
/// any tool runs.
#[test]
fn a_package_another_service_records_is_a_conflict() {
    let env = Env::new("apt-conflict");
    let order = env.publish(&deb(V1));
    env.records.put(
        "web",
        hostkeeper_install::repository::PackageRecord {
            installer: "apt".to_owned(),
            package: PACKAGE.to_owned(),
            version: V1.to_owned(),
            repository: REPO.to_owned(),
            links: Default::default(),
            pending: false,
        },
    );
    let before = env.snapshot();
    let (result, _) = env.stage(&order);
    let failure = result.expect_err("a conflict");
    assert_failure(&failure, Reason::PackageConflict, Release, Failed);
    assert!(env.host.calls().is_empty());
    assert_eq!(env.snapshot(), before);
}

/// Proves an unmet dependency in the simulation is `dependency_missing`, failed, environment,
/// and nothing is downloaded.
#[test]
fn an_unmet_dependency_is_missing() {
    let env = Env::new("apt-unmet");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.unmet = true);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::DependencyMissing, Environment, Failed);
    assert!(!env.has_run("apt-get download"));
}

/// Proves a simulation that would install, configure, remove or downgrade another package is
/// `dependency_changes`, failed, environment, one case for each verb, nothing downloaded.
#[test]
fn a_simulation_that_touches_another_package_changes_dependencies() {
    for extra in [
        "Inst libfoo (2.0 main [amd64])",
        "Remv libold [1.0]",
        "Conf libbar (3.1 main [amd64])",
        "Inst other [2.0] (1.0 main [amd64])",
        "Purg libgone [1.0]",
    ] {
        let env = Env::new("apt-changes");
        let order = env.publish(&deb(V1));
        env.host
            .edit(|s| s.faults.sim_extra = vec![extra.to_owned()]);
        let failure = refused(&env, &order);
        assert_failure(&failure, Reason::DependencyChanges, Environment, Failed);
        assert!(!env.has_run("apt-get download"), "{extra}");
    }
}

/// Proves a dpkg lock held at the simulation is `package_manager_busy`, deferred, environment.
#[test]
fn a_held_dpkg_lock_defers() {
    let env = Env::new("apt-busy");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| {
        s.faults.busy.insert("simulate", 1);
    });
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::PackageManagerBusy, Environment, Deferred);
}

/// Proves an interrupted dpkg is `dpkg_interrupted`, failed, environment, and hostkeeper runs
/// nothing host-wide to mend it.
#[test]
fn an_interrupted_dpkg_is_refused() {
    let env = Env::new("apt-interrupted");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.interrupted = true);
    let failure = refused(&env, &order);
    assert_failure(&failure, Reason::DpkgInterrupted, Environment, Failed);
    assert!(!env.has_run("apt-get download"));
}

/// Proves no stage run is allowed the network but the two fetches, also when the stage fails.
#[test]
fn a_failed_stage_still_fetches_only_with_its_two_commands() {
    let env = Env::new("apt-fail-network");
    let order = env.publish(&deb(V1));
    env.host.edit(|s| s.faults.unmet = true);
    let _ = refused(&env, &order);
    for call in env.host.calls() {
        let fetch = call.run.network == Network::Fetch;
        assert_eq!(
            fetch,
            call.label == "apt-get update" || call.label == "apt-get download"
        );
    }
}
