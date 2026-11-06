#![cfg(feature = "privileged")]
//! A tierstore-shaped package updated end to end, as root, under the real systemd of the host:
//! the real coordinator (in process) and the real daemon (the `hostkeeper` program under a
//! `Type=notify` unit) fetch it from a release channel the test serves on loopback, check its
//! attestation with the pinned `gh` against a test trust root, unpack and seal it, switch
//! `current`, and run its hook in step units; the fixture's hook drains in `prepare` and starts
//! the new release beside the old in `cutover` (`tests/bin/tierstore_like.rs`).
//!
//! Each test builds its own state with the shared helpers of `e2e::scenario`, so none depends on
//! another or on their order; each starts by checking it is root on a host whose service manager
//! is systemd, and fails with that reason otherwise. Every wait is a bounded count of tries of an
//! explicit condition; nothing is asserted about time.
//!
//! What each assertion reads is produced only by the real stage it witnesses: the channel's
//! request log (the fetch), the version directory, its mode and its seal (the unpacker), the
//! `current` and `previous` links (the switch), the version string an instance serves, read from
//! the manifest inside the unpacked package it runs from (the switch and the hook), `systemctl
//! is-active` (the units), the step record and the hook's record files (the step launcher), and
//! `hostkeeper ctl` against the coordinator (the heartbeat's report).

mod e2e;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use e2e::channel::{ReleaseChannel, archive_path};
use e2e::host_root::{HostRoot, is_active};
use e2e::package::{FixtureVersion, PackageBuilder};
use e2e::scenario::{COORDINATED, LOCAL, Scenario, V1, V2, V3, pinned_gh};
use e2e::signer::FixtureSigner;
use e2e::{FIXTURE_BINARY, SERVICE, UNIT_TEMPLATE, instance, served_version, wait_for};
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_package::fetch::{FetchEnv, PackageOrder, fetch_package};
use hostkeeper_package::gh::{GhVerifier, OwnedBy, SystemRunner};
use hostkeeper_package::net::{NetFetcher, UreqTransport};
use hostkeeper_package::prove::{ProveEnv, prove};
use hostkeeper_steps::Clock;
use hostkeeper_steps::selfupdate::Layout;
use serde_json::Value;

/// The unit directory of the host, where the daemon links the package's units.
const UNIT_DIR: &str = "/etc/systemd/system";

/// Whether systemd reports `unit` active.
fn active(port: u16) -> bool {
    is_active(&instance(port))
}

/// Every file and directory under `dir`, by path relative to it, with its mode and its bytes:
/// what "byte-identical" compares.
fn tree(dir: &Path) -> Vec<(PathBuf, u32, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let mut entries: Vec<_> = fs::read_dir(&next)
            .expect("a directory of the version")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = fs::symlink_metadata(&path).expect("an entry's metadata");
            let relative = path
                .strip_prefix(dir)
                .expect("under the version")
                .to_path_buf();
            if meta.is_dir() {
                found.push((relative, meta.mode(), Vec::new()));
                pending.push(path);
            } else {
                found.push((
                    relative,
                    meta.mode(),
                    fs::read(&path).expect("a file's bytes"),
                ));
            }
        }
    }
    found.sort();
    found
}

/// The whole path from a desired version to a running new release, with the drain holding the
/// old release up until it is released: `1.0.0` installed and serving, the drain gate busy, the
/// coordinator told `1.1.0`. The daemon fetches `1.1.0` once, unpacks it root-owned and mode 0555
/// with a seal naming the digest set, and runs `prepare`, which waits while the old instances
/// keep serving `1.0.0`; once the gate is idle, `current` is `1.1.0` and `previous` `1.0.0`, the
/// new instances serve `1.1.0` and are active, the old ones are stopped, the coordinator reports
/// the host converged on `1.1.0`, and exactly `1.0.0` and `1.1.0` are on disk with their seals.
#[test]
fn a_tierstore_shaped_package_updates_end_to_end() {
    let s = Scenario::installed("upd");
    let old = s.package(V1).clone();
    let new = s.package(V2).clone();
    let fetched = archive_path(SERVICE, V2, &new.file_name);
    assert_eq!(
        s.channel.count(&fetched),
        0,
        "1.1.0 is not fetched before it is ordered"
    );

    s.host.set_gate(true);
    s.order(V2);

    wait_for("the daemon fetches 1.1.0", LOCAL, || {
        s.channel.count(&fetched) > 0
    });
    let dir = s.host.service_dir().join(V2);
    wait_for("1.1.0 is unpacked and sealed", LOCAL, || {
        dir.is_dir() && s.host.service_dir().join(format!("{V2}.seal")).is_file()
    });
    let meta = fs::metadata(&dir).expect("the version directory");
    assert_eq!(meta.uid(), 0, "the version directory is root's");
    assert_eq!(meta.permissions().mode() & 0o7777, 0o555);
    assert_eq!(s.seal(V2)["package_digest"], new.sha256.as_str());

    wait_for("prepare waits on the busy drain gate", LOCAL, || {
        s.host.record(&format!("prepare-{V2}.waiting")).is_some()
    });
    let record = s.host.step_record().expect("the step record of the update");
    assert_eq!(record["target_version"], V2);
    assert_eq!(record["step"], "prepare");
    assert_eq!(record["state"]["state"], "running");
    for port in old.ports {
        assert_eq!(
            served_version(port).as_deref(),
            Some(V1),
            "the old instance on {port} serves while prepare drains"
        );
    }
    assert_eq!(s.link("current").as_deref(), Some(V1));

    s.host.set_gate(false);
    wait_for("current points at 1.1.0", LOCAL, || {
        s.link("current").as_deref() == Some(V2)
    });
    assert_eq!(s.link("previous").as_deref(), Some(V1));
    s.wait_converged(V2);
    for port in new.ports {
        assert_eq!(
            served_version(port).as_deref(),
            Some(V2),
            "new instance {port}"
        );
        assert!(active(port), "the new instance {port} is active");
    }
    for port in old.ports {
        assert!(!active(port), "the old instance {port} is stopped");
        assert_eq!(served_version(port), None, "nothing answers on {port}");
    }
    assert_eq!(
        s.channel.count(&fetched),
        1,
        "1.1.0 was fetched exactly once"
    );
    assert_eq!(
        s.version_dirs(),
        BTreeSet::from([V1.to_owned(), V2.to_owned()])
    );
    for version in [V1, V2] {
        assert_eq!(
            s.seal(version)["package_digest"],
            s.package(version).sha256.as_str()
        );
    }
}

/// A release that starts but fails its verification leaves the host serving the old release
/// under real systemd: from `1.1.0` converged, `1.2.0` (whose `verify` exits 1) is fetched once,
/// unpacked and sealed; its `cutover` starts its instances; `verify` fails; the daemon runs
/// `rollback` from the new release's hook, switches `current` back to `1.1.0` (`previous` is the
/// version rolled back from), the old instances serve `1.1.0` and the new ones are stopped; the
/// update ends `Failed`, phase `gate_release`, class `release`, and `cleanup` never runs; the
/// coordinator marks `1.2.0` bad and puts the desired version back to `1.1.0`, which the host is
/// converged on. The end-to-end witness of `a_failed_verify_rolls_back_and_cleanup_never_runs`.
#[test]
fn a_failed_verify_rolls_back_and_the_old_release_serves() {
    let s = Scenario::updated("rbk");
    let old = s.package(V2).clone();
    let bad = s.package(V3).clone();
    let fetched = archive_path(SERVICE, V3, &bad.file_name);

    s.order(V3);

    wait_for("the host reports 1.2.0 failed", COORDINATED, || {
        let update = s.last_update();
        update["target"] == V3 && update["outcome"] == "OUTCOME_FAILED"
    });
    let update = s.last_update();
    assert_eq!(update["phase"], "FAILURE_PHASE_GATE_RELEASE", "{update}");
    assert_eq!(update["class"], "FAILURE_CLASS_RELEASE", "{update}");
    assert_eq!(update["reason"], "fixture_verify_failed", "{update}");

    assert_eq!(s.channel.count(&fetched), 1, "1.2.0 was fetched once");
    assert!(s.host.service_dir().join(V3).is_dir(), "1.2.0 was unpacked");
    assert_eq!(s.seal(V3)["package_digest"], bad.sha256.as_str());
    assert_eq!(
        s.host.record(&format!("cutover-{V3}")).as_deref(),
        Some("ok")
    );
    assert_eq!(
        s.host.record(&format!("verify-{V3}")).as_deref(),
        Some("failed")
    );
    assert_eq!(
        s.host.record(&format!("rollback-{V3}")).as_deref(),
        Some("ok")
    );
    assert_eq!(
        s.host.record(&format!("cleanup-{V3}")),
        None,
        "cleanup never ran"
    );

    assert_eq!(s.link("current").as_deref(), Some(V2));
    assert_eq!(s.link("previous").as_deref(), Some(V3));
    // The rollback hook starts the old units and returns at once (`Type=exec` is active when the
    // program is executed, before it has bound its port), so the answer is waited for.
    s.wait_serving(V2);
    for port in old.ports {
        assert_eq!(
            served_version(port).as_deref(),
            Some(V2),
            "old instance {port}"
        );
        assert!(active(port), "the old instance {port} is active again");
    }
    for port in bad.ports {
        assert!(!active(port), "the new instance {port} is stopped");
    }

    let record = s.host.step_record().expect("the step record of the update");
    assert_eq!(record["target_version"], V3);
    assert_eq!(record["state"]["state"], "finished", "{record}");
    let failed = &record["state"]["verdict"]["Failed"];
    assert_eq!(failed["phase"], "gate_release", "{record}");
    assert_eq!(failed["class"], "release", "{record}");

    // The coordinator holds 1.2.0 bad and wants 1.1.0 again: no order of 1.2.0 can follow
    // (a bad version is never ordered), and the single fetch above is the host's only try.
    wait_for(
        "the coordinator marks 1.2.0 bad and wants 1.1.0",
        COORDINATED,
        || {
            let status = s.status();
            let bad_marked = status["marks"]["bad"]
                .as_array()
                .is_some_and(|bad| bad.iter().any(|version| version == V3));
            bad_marked && status["desired"]["version"] == V2
        },
    );
    s.wait_converged(V2);
    assert_ne!(s.convergence()["desired"], V3);
    assert_eq!(
        s.channel.count(&fetched),
        1,
        "the host did not try 1.2.0 again"
    );
}

/// The previous version kept on disk is enough to repair a host while the release channel is
/// down, and the pruning rule keeps exactly the two versions: from `1.1.0` converged (`previous`
/// `1.0.0`, both sealed), the channel stopped and unreachable, a force to `1.0.0` through the
/// coordinator. The channel's log gains nothing; `1.0.0`'s tree and seal are byte-identical
/// after and its tree still matches its seal; `current` is `1.0.0` and `previous` `1.1.0`; the
/// instances of `1.0.0` are active and serve it, those of `1.1.0` are stopped; exactly `1.0.0`
/// and `1.1.0` are on disk with their seals; the host reports the forced order succeeded.
#[test]
fn force_back_to_previous_needs_no_fetch() {
    let mut s = Scenario::updated("frc");
    let target = s.package(V1).clone();
    let left = s.package(V2).clone();
    assert_eq!(s.link("current").as_deref(), Some(V2));
    assert_eq!(s.link("previous").as_deref(), Some(V1));
    assert_eq!(
        s.version_dirs(),
        BTreeSet::from([V1.to_owned(), V2.to_owned()])
    );
    let layout = Layout::package(&s.host.service_dir());
    layout
        .check_tree(V1)
        .expect("1.0.0's tree matches its seal");
    let seal_path = s.host.service_dir().join(format!("{V1}.seal"));
    let seal_before = fs::read(&seal_path).expect("1.0.0's seal");
    let tree_before = tree(&s.host.service_dir().join(V1));

    let requests = s.channel.total();
    s.channel.stop();
    assert!(
        !s.channel.reachable(),
        "the stopped channel is not reachable"
    );

    s.coordinator.force(SERVICE, V1, e2e::HOST_ID);

    // The verdict of the force order: the heartbeat wire carries no `forced` flag (the
    // coordinator's `update_result` says so), so the force's own order id is the witness.
    wait_for("the host reports the force to 1.0.0", COORDINATED, || {
        let update = s.last_update();
        update["target"] == V1
            && update["orderId"]
                .as_str()
                .is_some_and(|id| id.starts_with("force-"))
            && update["outcome"] != Value::Null
    });
    let update = s.last_update();
    assert_eq!(update["outcome"], "OUTCOME_SUCCEEDED", "{update}");
    assert_eq!(s.convergence()["force"]["version"], V1, "the force stands");
    assert_eq!(s.channel.total(), requests, "the force fetched nothing");

    assert_eq!(s.link("current").as_deref(), Some(V1));
    assert_eq!(s.link("previous").as_deref(), Some(V2));
    // A unit is active once its program is executed, a moment before it answers: wait for it.
    s.wait_serving(V1);
    for port in target.ports {
        assert!(active(port), "the instance {port} of 1.0.0 is active");
        assert_eq!(served_version(port).as_deref(), Some(V1), "instance {port}");
    }
    for port in left.ports {
        assert!(!active(port), "the instance {port} of 1.1.0 is stopped");
    }
    assert_eq!(
        s.version_dirs(),
        BTreeSet::from([V1.to_owned(), V2.to_owned()])
    );
    for version in [V1, V2] {
        layout
            .check_tree(version)
            .unwrap_or_else(|error| panic!("{version}'s tree matches its seal: {error}"));
    }
    assert_eq!(fs::read(&seal_path).expect("1.0.0's seal"), seal_before);
    assert_eq!(tree(&s.host.service_dir().join(V1)), tree_before);
    s.wait_converged(V1);
}

/// The daemon is ready under a `Type=notify` unit only after its boot pass: with `1.0.0`
/// installed, the daemon stopped and one of the wants links it placed removed, `systemctl start`
/// returns (which, for `Type=notify`, is after the daemon sent `READY=1`) with the link back in
/// place (the boot pass's reconcile of the placed paths ran before the notice), the unit active
/// and of type notify, the boot id recorded, the admin socket bound with mode 0600, and the
/// fixture's instances still serving.
#[test]
fn the_daemon_is_ready_under_a_notify_unit_only_after_its_boot_pass() {
    let s = Scenario::installed("boot");
    let installed = s.package(V1).clone();
    let wants = Path::new(UNIT_DIR)
        .join("multi-user.target.wants")
        .join(instance(installed.ports[0]));
    let template = Path::new(UNIT_DIR).join(UNIT_TEMPLATE);
    assert_eq!(
        fs::read_link(&wants).expect("the placed wants link"),
        template
    );

    s.daemon.stop();
    assert!(!s.daemon.is_active(), "the daemon is stopped");
    fs::remove_file(&wants).expect("remove the wants link");
    s.daemon.start();

    assert_eq!(
        fs::read_link(&wants).expect("the wants link is back when the start returns"),
        template
    );
    let unit = s.daemon.show(&["Type", "ActiveState"]);
    assert_eq!(unit.get("Type").map(String::as_str), Some("notify"));
    assert_eq!(unit.get("ActiveState").map(String::as_str), Some("active"));
    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id").expect("the boot id");
    let recorded = fs::read_to_string(s.host.state().join(".boot-id")).expect("the recorded id");
    assert_eq!(recorded.trim(), boot_id.trim());
    let socket = fs::metadata(s.host.admin_socket()).expect("the admin socket");
    assert!(socket.file_type().is_socket());
    assert_eq!(socket.permissions().mode() & 0o777, 0o600);
    for port in installed.ports {
        assert_eq!(served_version(port).as_deref(), Some(V1), "instance {port}");
    }
}

/// The fixture signer's attestation is checked by the production verifier, unchanged: the
/// package of `1.0.0`, fetched from the loopback channel by the production fetcher and proven by
/// `prove` with the pinned `gh` against the fixture signer's trusted root, is accepted; the same
/// package with a bundle for another digest is refused `not_attested`, and with a bundle of a
/// signer the trust root does not hold is refused by `gh`. Fails if the signer's bundle or trust
/// root is one the verifier would not take, or if anything but the verifier passes it.
#[test]
fn fixture_attestation_is_checked_by_the_production_verifier() {
    let host = HostRoot::new("att");
    host.install_gh(&pinned_gh());
    let signer = FixtureSigner::new();
    host.install_trusted_root(&signer.trusted_root_jsonl());
    let other_signer = FixtureSigner::new();
    let mut channel = ReleaseChannel::start();

    let built = host.base().join("built");
    fs::create_dir_all(&built).expect("the directory of the built packages");
    let builder = PackageBuilder::new(&host.packages(), &host.records(), Path::new(FIXTURE_BINARY));
    let ports = [e2e::free_port(), e2e::free_port()];
    let versions = [
        ("1.0.0", "good"),
        ("1.0.1", "other digest"),
        ("1.0.2", "other signer"),
    ];
    for (version, case) in versions {
        let package = builder.build(
            &FixtureVersion {
                version,
                ports,
                fail_verify: false,
            },
            &built,
        );
        let bundle = match case {
            "good" => signer.bundle(&package.file_name, &package.sha256),
            "other digest" => signer.bundle(&package.file_name, &"0".repeat(64)),
            _ => other_signer.bundle(&package.file_name, &package.sha256),
        };
        channel.publish(
            SERVICE,
            version,
            &package.file_name,
            package.bytes.clone(),
            bundle,
        );
    }

    let config = DaemonConfig::parse(&format!(
        "coordinator = \"https://localhost\"\nca_sha256 = \"{}\"\nrelease_channel = \"{}\"\n\n{}",
        "0".repeat(64),
        channel.base_url(),
        signer.trusted_signers_toml()
    ))
    .expect("a daemon config");
    let clock: Arc<dyn Clock> = Arc::new(hostkeeper_daemon::adapters::host_clock());
    let fetcher = NetFetcher::new(UreqTransport::new(&[channel.ca_der()]), Arc::clone(&clock));
    let home = host.state().join("gh-home");
    fs::create_dir_all(&home).expect("gh's home");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("gh's home is private");
    let verifier = GhVerifier::new(
        PathBuf::from(e2e::host_root::GH_PATH),
        PathBuf::from(e2e::host_root::TRUSTED_ROOT_PATH),
        home,
        Box::new(SystemRunner::new(Arc::clone(&clock))),
        Box::new(OwnedBy { uid: 0 }),
    );
    let fetch = FetchEnv {
        fetcher: &fetcher,
        clock: clock.as_ref(),
        bounds: &config.bounds,
        release_channel: &config.release_channel,
        packages_root: &host.packages(),
        platform: hostkeeper_package::Platform::current(),
    };
    let proof = ProveEnv {
        fetcher: &fetcher,
        verifier: &verifier,
        clock: clock.as_ref(),
        bounds: &config.bounds,
        signers: &config.trusted_signers,
    };
    fs::create_dir_all(host.packages().join(SERVICE)).expect("the service's package directory");
    let proven = |version: &str| {
        let order = PackageOrder {
            service: ServiceName::new(SERVICE).expect("a service name"),
            version: Version::new(version).expect("a version"),
            // An order naming the package by its digest alone, as the scenarios set it.
            index_digest: None,
        };
        let staged = fetch_package(&fetch, &order).expect("the package is fetched");
        prove(&proof, staged).map(|verified| verified.sha256().as_str().to_owned())
    };

    let good = proven("1.0.0").expect("the fixture attestation is accepted");
    assert_eq!(good.len(), 64);
    let refused = proven("1.0.1").expect_err("a bundle for another digest is refused");
    assert_eq!(refused.reason, "not_attested", "{refused}");
    let refused = proven("1.0.2").expect_err("a signer outside the trust root is refused");
    assert_eq!(refused.reason, "not_attested", "{refused}");
    channel.stop();
}
