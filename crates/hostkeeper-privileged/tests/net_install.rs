#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "privileged"))]
//! Network install of an x86-64 machine, end to end (docs/design/network-install.md section 11,
//! net-install.yml's job `net-install`): a QEMU guest with OVMF under KVM, an empty disk, a fixed
//! UUID and serial, its NIC first in the boot order, on a LAN with no route out, with the
//! router's DHCP faked by the test, the real `pxe` and the real coordinator with its
//! LAN listener (`netinstall`). The operator acts only through the operator API, and the guest is
//! only booted, rebooted (by its operator account) and read.
//!
//! In order, it checks:
//!
//! 1. Proxy mode: the router's DHCP saw the guest's DISCOVER and `pxe` sent its proxy
//!    offer; the guest got the install menu of its MAC and fetched its seed; the reboot after the
//!    install got no proxy offer (its MAC left the answer set) and booted the disk.
//! 2. A pending request appears, matched to the record, with claims equal to the guest's UUID and
//!    serial; the console shows its key hash; `Approve` by that hash succeeds; the first
//!    heartbeat arrives; the fixture service assigned to the host's role reports installed.
//!    The installed system holds exactly the repository files `serve --repositories` was given,
//!    root's with mode 0644.
//! 3. Router mode, the enrolled guest rebooted: `pxe` learns its address from its
//!    REQUEST, serves `hostkeeper/boot.efi` as the x86-64 shim, and its local-disk menu; its
//!    heartbeat comes back from the same disk.
//! 4. Reinstall with `--reinstall`, in router mode (the record states its platform): the guest's
//!    REQUEST learned, `hostkeeper/boot.efi` served as the x86-64 shim, the install menu and the
//!    seed fetched; the reboot after the install got the local-disk menu (its MAC left the answer
//!    set); then a request flagged `duplicate_claim`, an approval without `replace` refused,
//!    the `replace` approval, and the old certificate's heartbeat refused `forbidden`.
//! 5. The host retired (`Hosts.Retire`), the coordinator restarted, the guest network-booted
//!    without `--reinstall`: no answer in proxy mode, the local-disk menu in router mode, and each
//!    time the same installed system boots from its disk, with no seed fetched.
//!
//! The guest's console, the logs and the coordinator's are kept under
//! `target/privileged-artifacts/net_install/`, with the inputs of the measurement of the first
//! install that are not logs (design section 11, item N11): `pending.json`, the pending list as
//! `Enrollment.List` gave it before the approval, and `host.json`, the host as `Hosts.Get` gave
//! it once the fixture service was installed, its serving time among them. The job's step after
//! this test makes the report of them with `cargo xtask netinstall-report`; no test asserts a
//! time.
//!
//! A second test, `an_assigned_pxe_package_prepares_serves_and_uninstalls`, needs no guest: the
//! `pxe` .deb (the program of the tests and the committed unit) assigned to a host by the
//! operator API is installed by the real daemon through apt, its `update-hook prepare` fetches
//! and seals a boot set from the channel, its unit serves from it under systemd, and an
//! uninstall takes it off.

mod e2e;
mod netinstall;
mod support;

use std::net::Ipv4Addr;
use std::path::Path;

use e2e::aptrepo::{self, AptRepo, Deb, File, Service};
use e2e::signer::FixtureSigner;
use hostkeeper_proto::hostkeeper::release::v1::StepBounds;
use netinstall::events::Source;
use netinstall::guest::{Arch, Machine, Ssh};
use netinstall::pxe::{self, Mode, ServeSettings, Tools};
use netinstall::{Rig, SERVICE, VERSION, fixture, heartbeat};
use serde_json::Value;
use support::channel::Channel;
use support::operator_api::{Approval, must};
use support::pair::{Coordinator, Daemon, HostPaths, PACKAGES_DIR, ROLE, TestCa, put, rollout};
use support::spawn::poll;
use support::{Bench, journal, run};
use tonic::Code;

/// The record's host id, and the guest's host name once installed.
const HOST: &str = "net-1";

fn machine() -> Machine {
    Machine {
        arch: Arch::X86_64,
        mac: "52:54:00:4e:10:01",
        uuid: "6f1e2d3c-4b5a-4978-8a1b-2c3d4e5f6071",
        serial: "HKNET-X86-0001",
    }
}

/// The fixture service as the host's latest heartbeat reports it (`null` when it does not).
fn beat(host: &Value) -> Value {
    host["heartbeat"]["services"]
        .as_array()
        .and_then(|all| all.iter().find(|s| s["name"] == SERVICE).cloned())
        .unwrap_or(Value::Null)
}

/// The fixture service as the coordinator counts it on the host.
fn seen(host: &Value) -> Value {
    host["services"]
        .as_array()
        .and_then(|all| all.iter().find(|s| s["service"] == SERVICE).cloned())
        .unwrap_or(Value::Null)
}

/// A unix time the operator API's JSON has (a string), as a number.
fn unix(value: &Value) -> Option<u64> {
    value.as_str().and_then(|s| s.parse().ok())
}

/// The host as `Hosts.Get` gives it, or why not.
fn host_of(coordinator: &Coordinator) -> Result<Value, String> {
    coordinator
        .api()
        .get_host(HOST)
        .map(|got| got["host"].clone())
        .map_err(|e| e.to_string())
}

/// The record's `hostHeld`, as `Enrollment.ListRecords` gives it.
fn host_held(coordinator: &Coordinator) -> Result<Value, String> {
    let records = coordinator
        .api()
        .list_records()
        .map_err(|e| e.to_string())?;
    records["records"]
        .as_array()
        .and_then(|all| all.iter().find(|r| r["hostId"] == HOST))
        .map(|record| record["hostHeld"].clone())
        .ok_or_else(|| format!("no record of {HOST}: {records}"))
}

/// Waits until the host reports the fixture service installed at [`VERSION`], with no order of
/// it under way, in a heartbeat whose certificate expires after `after` (so a heartbeat of the
/// host's current key, not of one it held before).
fn wait_installed(rig: &mut Rig, after: u64) {
    let coordinator = &rig.coordinator;
    rig.bus.until("the fixture service reported installed", || {
        let host = host_of(coordinator)?;
        let fresh = unix(&host["certNotAfterUnix"]).is_some_and(|t| t > after);
        let (seen, beat) = (seen(&host), beat(&host));
        match (fresh, seen["installedVersion"].as_str()) {
            (true, Some(v)) if v == VERSION && beat["updating"].is_null() => Ok(()),
            _ => Err(format!(
                "{seen} (beat {beat}, a heartbeat of the current key: {fresh})"
            )),
        }
    });
}

#[test]
fn an_x86_64_machine_installs_by_network_enrolls_gets_its_service_is_reinstalled_and_retired() {
    assert!(
        Path::new("/dev/kvm").exists(),
        "/dev/kvm is missing: the x86-64 guest runs under KVM, on a runner that has it"
    );
    let mut rig = Rig::new("net_install", "x", [192, 0, 2], machine(), HOST);
    let mac = rig.mac();
    let guest_ip = rig.guest_address().to_string();

    // The fixture service: its package in the apt repository the channel serves, its desired
    // version set by its `apt` entry; the rollout file runs it on the record's role, so the host
    // is assigned it at its approval.
    let entry = fixture(&rig.repo);
    rig.channel.serve_tree(rig.repo.dir());
    must(
        "releases set-desired",
        rig.coordinator.api().set_desired_targets_alone(
            SERVICE,
            VERSION,
            e2e::aptrepo::targets(entry),
        ),
    );

    // 1. Proxy mode: the install, and the reboot after it.
    rig.serve(Mode::Proxy, &[]);
    let start = rig.bus.mark();
    rig.guest.start(&rig.bus);
    let discover = format!("DHCPDISCOVER from {mac}: offer {guest_ip}, no boot file");
    rig.bus.wait(
        start,
        Source::Router,
        "the router's DHCP saw the guest's DISCOVER",
        |l| l == discover,
    );
    let seeded = rig.wait_install(start, Mode::Proxy);
    let booted = rig.login_prompt(seeded);
    assert!(
        !rig.bus.saw(seeded, booted, Source::Pxe, |l| l
            .contains("proxy offer sent")),
        "no proxy offer after the seed's fetch: {:?}",
        rig.bus.lines_since(seeded, Source::Pxe)
    );
    // The reboot tried the network first, by QEMU's boot order, whatever curtin wrote in the
    // firmware's variables, and got no answer: design section 11, marked to verify.
    let asked = format!("dhcp: DHCPDISCOVER from {mac}, ");
    let left = format!(
        "no answer: not in the answer set: the record of host {HOST}: its seed was fetched in \
         this run, so its install started"
    );
    assert!(
        rig.bus
            .saw(seeded, booted, Source::Pxe, |l| l.contains(&asked)
                && l.ends_with(&left)),
        "the reboot after the install asked the network first and got no answer: {:?}",
        rig.bus.lines_since(seeded, Source::Pxe)
    );

    // 2. Enrollment, approval, the first heartbeat and the fixture service.
    let request = rig.wait_request(&[]);
    let pending = must("enrollment list", rig.coordinator.api().pending());
    rig.keep("pending.json", pending.to_string().as_bytes());
    let key = request["keySha256"]
        .as_str()
        .expect("a key hash")
        .to_owned();
    rig.console_shows(seeded, &key);
    rig.trust_channel();
    let approved = must(
        "enrollment approve",
        rig.coordinator.api().approve(&Approval {
            request_id: request["requestId"].as_str().expect("a request id"),
            key_sha256: &key,
            role: ROLE,
            reason: "network-install test: the key hash is the console's",
            ..Approval::default()
        }),
    );
    assert_eq!(approved["hostId"], HOST, "the record's host id: {approved}");
    let coordinator = &rig.coordinator;
    let first = rig.bus.until("the host's first heartbeat", || {
        let host = host_of(coordinator)?;
        unix(&host["firstHeartbeatUnix"])
            .map(|_| host.clone())
            .ok_or_else(|| format!("{host}"))
    });
    let first_cert = unix(&first["certNotAfterUnix"]).expect("the host's certificate");
    wait_installed(&mut rig, first_cert - 1);
    // The first heartbeat that reports the fixture service healthy is kept as serving: the one
    // wait_installed saw, or the next.
    let coordinator = &rig.coordinator;
    let got = rig.bus.until("the host's serving time", || {
        let got = coordinator
            .api()
            .get_host(HOST)
            .map_err(|e| e.to_string())?;
        unix(&got["host"]["firstServingUnix"])
            .map(|_| got.clone())
            .ok_or_else(|| format!("{got}"))
    });
    rig.keep("host.json", got.to_string().as_bytes());
    // The seed wrote the repository entries the operator gave `serve`, and the daemon, whose
    // loader refuses to start on a defect, started on them.
    netinstall::repositories::assert_installed(&rig.ssh, &rig.repository_files);

    // 3. Router mode: the enrolled guest boots its disk.
    rig.serve(Mode::Router, &[]);
    let last = unix(&rig.host().expect("the host")["lastHeartbeatUnix"]).expect("a heartbeat");
    let from = rig.bus.mark();
    rig.ssh.reboot();
    let (at, _) = rig.pxe_says(
        from,
        "the enrolled guest's REQUEST, learned",
        &[
            &format!("DHCPREQUEST from {mac}, "),
            &format!("it takes {guest_ip}"),
        ],
    );
    let (at, _) = rig.pxe_says(
        at,
        "hostkeeper/boot.efi served as the x86-64 shim",
        &[&format!(
            "hostkeeper/boot.efi is hostkeeper/shimx64.efi for {mac} (linux-x86_64)"
        )],
    );
    let (at, _) = rig.pxe_says(
        at,
        "the shim's GRUB served",
        &["sent hostkeeper/grubx64.efi"],
    );
    let (at, menu) = rig.pxe_says(
        at,
        "the enrolled guest's local-disk menu",
        &[&format!("menu grub/hostkeeper/x86_64/{mac}: local disk: ")],
    );
    assert!(
        menu.contains(&format!("the record of host {HOST}: a host")),
        "left out because a host holds the record: {menu}"
    );
    let at = rig.login_prompt(at);
    assert!(
        !rig.bus
            .saw(from, at, Source::Pxe, |l| l.contains("its install started"))
    );
    let coordinator = &rig.coordinator;
    rig.bus.until("a heartbeat from the same disk", || {
        let host = host_of(coordinator)?;
        unix(&host["lastHeartbeatUnix"])
            .filter(|t| *t > last)
            .map(|_| ())
            .ok_or_else(|| format!("{host}"))
    });

    // 4. Reinstall, in router mode: the old identity is kept to be refused.
    let old_cert = rig
        .ssh
        .output("sudo cat /etc/updater/pki/current/cert.pem", b"")
        .expect("the old certificate");
    let old_key = rig
        .ssh
        .output("sudo cat /etc/updater/pki/current/key", b"")
        .expect("the old key");
    rig.serve(Mode::Router, &[HOST]);
    let from = rig.bus.mark();
    rig.ssh.reboot();
    let seeded = rig.wait_install(from, Mode::Router);
    let booted = rig.login_prompt(seeded);
    // The reboot after the install: its MAC left the answer set, so router mode gave the
    // local-disk menu.
    let local = format!("menu grub/hostkeeper/x86_64/{mac}: local disk: ");
    assert!(
        rig.bus
            .saw(seeded, booted, Source::Pxe, |l| l.contains(&local)
                && l.contains("its seed was fetched in this run")),
        "the reboot after the reinstall got the local-disk menu: {:?}",
        rig.bus.lines_since(seeded, Source::Pxe)
    );
    let again = rig.wait_request(std::slice::from_ref(&key));
    assert_eq!(
        again["duplicateClaim"], true,
        "the new key's request: {again}"
    );
    let new_key = again["keySha256"].as_str().expect("a key hash").to_owned();
    rig.console_shows(seeded, &new_key);
    rig.trust_channel();
    let approve = |replace: bool| {
        rig.coordinator.api().approve(&Approval {
            request_id: again["requestId"].as_str().expect("a request id"),
            key_sha256: &new_key,
            role: ROLE,
            reason: "network-install test: reinstalled, the key hash is the console's",
            replace,
            ..Approval::default()
        })
    };
    let refused = approve(false).expect_err("approval of a duplicate claim without replace");
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused}");
    assert_eq!(refused.message(), "duplicate_claim", "{refused}");
    approve(true).expect("the replace approval");
    let ca_der = std::fs::read(rig.coordinator.ca_cert_path()).expect("the CA");
    let lan = rig.coordinator.lan.expect("the LAN listener");
    let (status, body) = heartbeat::post(lan, &ca_der, &old_cert, &old_key);
    assert_eq!(status, 403, "the old certificate's heartbeat: {body}");
    assert!(body.contains("forbidden"), "{body}");
    wait_installed(&mut rig, first_cert);

    // 5. Retired, the coordinator restarted, and booted from the network again.
    must(
        "hosts retire",
        rig.coordinator
            .api()
            .retire(HOST, "network-install test: retired"),
    );
    let coordinator = &rig.coordinator;
    rig.bus.until("the record's host_held RETIRED", || {
        let held = host_held(coordinator)?;
        (held == "HOST_HELD_RETIRED")
            .then_some(())
            .ok_or_else(|| format!("hostHeld {held}"))
    });
    rig.coordinator.restart();
    assert_eq!(
        host_held(&rig.coordinator),
        Ok(Value::from("HOST_HELD_RETIRED")),
        "after the coordinator's restart"
    );
    let installed = rig.machine_id();
    for mode in [Mode::Proxy, Mode::Router] {
        rig.serve(mode, &[]);
        let from = rig.bus.mark();
        rig.ssh.reboot();
        let retired = format!("the record of host {HOST}: that id is retired");
        let (at, _) = match mode {
            Mode::Proxy => rig.pxe_says(
                from,
                "no answer to the retired host's DISCOVER",
                &[
                    &format!("DHCPDISCOVER from {mac}, "),
                    "no answer: not in the answer set: ",
                    &retired,
                ],
            ),
            Mode::Router => rig.pxe_says(
                from,
                "the retired host's local-disk menu",
                &[
                    &format!("menu grub/hostkeeper/x86_64/{mac}: local disk: "),
                    &retired,
                ],
            ),
        };
        let booted = rig.login_prompt(at);
        assert!(
            !rig.bus.saw(from, booted, Source::Pxe, |l| {
                l.contains("proxy offer sent") || l.contains("its install started")
            }),
            "{mode:?}: nothing offered or fetched: {:?}",
            rig.bus.lines_since(from, Source::Pxe)
        );
        assert_eq!(
            rig.machine_id(),
            installed,
            "{mode:?}: the disk is untouched"
        );
    }
}

/// The `pxe` service, its package and its unit, and what the .deb and the hook place on the host.
const PXE_SERVICE: &str = "pxe";
const PXE_UNIT: &str = "pxe.service";
const PXE_PROGRAM: &str = "usr/lib/pxe/pxe";
const PXE_UNIT_FILE: &str = "/usr/lib/systemd/system/pxe.service";
const PXE_WANTS: &str = "/etc/systemd/system/multi-user.target.wants/pxe.service";
/// The host's settings of `pxe` (the package param `settings`) and its boot sets (`boot_dir`).
const PXE_SETTINGS_DIR: &str = "/etc/pxe";
const PXE_SETTINGS: &str = "/etc/pxe/pxe.toml";
const PXE_BOOT_DIR: &str = "/var/lib/pxe";
/// The hook command of the package (the manifest's `hook.command`).
const PXE_HOOK: [&str; 2] = ["/usr/lib/pxe/pxe", "update-hook"];
/// The host, and the role of the `pxe` service in the rollout file (no host has it: only the
/// assignment brings the service).
const PXE_NODE: &str = "ci-pxe-host";
const NO_HOST_ROLE: &str = "unused";

/// The entry of `list` whose `key` names the `pxe` service, or `null`.
fn pxe_entry(list: &Value, key: &str) -> Value {
    list.as_array()
        .and_then(|all| all.iter().find(|entry| entry[key] == PXE_SERVICE))
        .cloned()
        .unwrap_or(Value::Null)
}

/// What `systemctl is-active pxe.service` prints.
fn pxe_active() -> String {
    let out = run(
        hostkeeper_steps::systemd::SYSTEMCTL,
        &["is-active", PXE_UNIT],
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Whether dpkg holds the package `pxe` installed (`ii`).
fn pxe_installed() -> bool {
    let out = run(
        "/usr/bin/dpkg-query",
        &["-W", "-f=${db:Status-Abbrev}", PXE_SERVICE],
    );
    out.status.success() && String::from_utf8_lossy(&out.stdout).starts_with("ii")
}

/// The `pxe` .deb, with the program of the tests at `/usr/lib/pxe/pxe` and the committed unit at
/// `/usr/lib/systemd/system/pxe.service` (its two files, as the release's .deb), assigned to a
/// host by the operator API, is installed by the real daemon through apt: `prepare` runs as
/// `/usr/lib/pxe/pxe update-hook prepare <staged> --params <file>`, fetches the netboot release
/// from the channel (attested by the end-to-end rig's signer, checked by the job's `gh`) and
/// seals the boot set under `/var/lib/pxe`; `cutover` points `current` at it and starts the unit,
/// which serves from the host's settings file in router mode with every listener on loopback.
/// `unassign --uninstall` stops the unit, runs `update-hook uninstall` and purges the package.
///
/// Catches a hook command, params or unit the program does not answer, a `prepare` that does
/// not seal the boot set under `boot_dir`, a `cutover` that does not point `current` at it and
/// start the unit, and an uninstall that leaves the boot sets, the unit, the package or the
/// directory of the daemon's packages, or removes the operator's settings file.
#[test]
fn an_assigned_pxe_package_prepares_serves_and_uninstalls() {
    let bench = Bench::new("pxe_package");
    let _host = HostPaths::claim(
        &[
            "/etc/hostkeeper",
            PXE_SETTINGS_DIR,
            PXE_BOOT_DIR,
            "/usr/lib/pxe",
            PXE_UNIT_FILE,
            PXE_WANTS,
        ],
        &[PXE_UNIT],
    )
    .purging(&[PXE_SERVICE]);
    let scratch = bench.scratch().to_path_buf();
    let ca = TestCa::new("release channel CA");
    let channel = Channel::start(&ca);
    let signer = FixtureSigner::new();
    let platform = pxe::platform();

    // The release `prepare` fetches, on the channel, and the apt repository holding the .deb of
    // the same version, which the host's repository entry and the registry name.
    let version = pxe::release(&channel, &signer, platform);
    let program = std::fs::read(pxe::PXE).expect("the program of the tests");
    let repo = AptRepo::new(&scratch, "pxe-ci");
    repo.install_registered(aptrepo::REGISTRY_NAME);
    let published = repo.publish(&Deb {
        package: PXE_SERVICE.to_owned(),
        version: version.clone(),
        files: vec![
            File::new(PXE_PROGRAM, program, 0o755),
            File::new("usr/lib/systemd/system/pxe.service", pxe::UNIT, 0o644),
        ],
    });
    let service = Service {
        repository: aptrepo::REPOSITORY.to_owned(),
        run: vec![PXE_UNIT.to_owned()],
        health_url: None,
        params: [
            ("release", channel.url()),
            ("settings", PXE_SETTINGS.to_owned()),
            ("boot_dir", PXE_BOOT_DIR.to_owned()),
            ("unit", PXE_UNIT.to_owned()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), aptrepo::text(&value)))
        .collect(),
        hook_command: PXE_HOOK.map(String::from).to_vec(),
        steps: StepBounds {
            prepare: 0,
            cutover: 120,
            verify: 30,
            rollback: 120,
            cleanup: 60,
            uninstall: Some(60),
        },
        verify_deadline_secs: 300,
    };
    let entry = aptrepo::entry(&service, &published);

    // The coordinator and the daemon, and the host's files: the daemon config the seeds carry,
    // the SSH keys, the settings file.
    let services = format!(
        "[[service]]\nname = \"{PXE_SERVICE}\"\nroles = [\"{NO_HOST_ROLE}\"]\n\
         repository = \"{}\"\n",
        aptrepo::REGISTRY_NAME
    );
    let mut coordinator = Coordinator::start(&scratch, &rollout(&services), &[]);
    must(
        "repositories put",
        coordinator
            .api()
            .put_repository(repo.registry_entry(aptrepo::REGISTRY_NAME)),
    );
    let daemon_config = scratch.join("pxe-daemon.toml");
    put(
        &daemon_config,
        format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"{}\"\n\n{}",
            coordinator.url(),
            coordinator.ca_sha256(),
            channel.url(),
            signer.trusted_signers_toml()
        )
        .as_bytes(),
        0o600,
    );
    let ssh = Ssh::new(&scratch.join("ssh"), Ipv4Addr::LOCALHOST);
    let tools = Tools::put(&scratch, &ca.pem(), &signer);
    let serve = ServeSettings {
        mode: Mode::Router,
        address: Ipv4Addr::LOCALHOST,
        coordinator: &coordinator,
        daemon_config: &daemon_config,
        ssh_keys: &ssh.public,
        serial_console: None,
        repositories: None,
        loopback: true,
    };
    put(
        Path::new(PXE_SETTINGS),
        format!("{}\n{}", serve.table(), tools.table(platform)).as_bytes(),
        0o600,
    );
    let mut daemon = Daemon::start(&scratch, &coordinator, &channel.url());
    coordinator.approve_first(PXE_NODE, &mut daemon);
    coordinator.wait_heard(PXE_NODE, &mut daemon);

    // The operator sets the desired version and assigns the service to the host.
    must(
        "releases set-desired",
        coordinator
            .api()
            .set_desired_targets_alone(PXE_SERVICE, &version, aptrepo::targets(entry)),
    );
    must(
        "services assign",
        coordinator.api().assign(PXE_NODE, PXE_SERVICE),
    );

    // The install: the host reports the service at the version, with no order under way.
    poll(&format!("{PXE_NODE} reporting pxe {version}"), || {
        coordinator.assert_running();
        daemon.assert_running();
        let listed = coordinator
            .host(PXE_NODE)?
            .ok_or_else(|| format!("hosts list lists no {PXE_NODE}"))?;
        let seen = pxe_entry(&listed["services"], "service");
        let beat = pxe_entry(&listed["heartbeat"]["services"], "name");
        match seen["installedVersion"].as_str() {
            Some(v) if v == version && beat["updating"].is_null() => Ok(()),
            _ => Err(format!(
                "{seen} (beat {beat}); the daemon's log:\n{}",
                daemon.log()
            )),
        }
    });
    let sealed = format!("sealed {PXE_BOOT_DIR}/{version}:");
    assert!(
        daemon
            .log()
            .lines()
            .any(|line| line.contains(": updater-pxe-prepare-") && line.contains(&sealed)),
        "prepare sealed the boot set: {}",
        daemon.log()
    );
    assert_eq!(
        std::fs::read_link(format!("{PXE_BOOT_DIR}/current")).ok(),
        Some(version.clone().into()),
        "current points at the version's boot set"
    );
    assert!(
        Path::new(&format!("{PXE_BOOT_DIR}/{version}/seal.json")).is_file(),
        "the boot set is sealed"
    );
    assert_eq!(
        pxe_active(),
        "active",
        "pxe.service is active\n{}",
        journal(PXE_UNIT)
    );
    assert!(pxe_installed(), "dpkg holds the package pxe");
    assert!(
        Path::new(PXE_UNIT_FILE).exists(),
        "the .deb placed the unit"
    );
    assert!(Path::new(PXE_WANTS).exists(), "the unit is enabled");

    // The operator takes the service off the host with an uninstall.
    must(
        "services unassign",
        coordinator.api().unassign(PXE_NODE, PXE_SERVICE, true),
    );
    let packages = Path::new(PACKAGES_DIR).join(PXE_SERVICE);
    poll(&format!("{PXE_NODE} uninstalled pxe"), || {
        coordinator.assert_running();
        daemon.assert_running();
        let entry = coordinator.service(PXE_SERVICE)?;
        let uninstalling = entry["uninstalling"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if uninstalling.is_empty() && !packages.exists() {
            Ok(())
        } else {
            Err(format!("{entry}; the daemon's log:\n{}", daemon.log()))
        }
    });
    for gone in [PXE_BOOT_DIR, PXE_UNIT_FILE, PXE_WANTS] {
        assert!(
            std::fs::symlink_metadata(gone).is_err(),
            "{gone} is removed\n{}",
            daemon.log()
        );
    }
    assert!(!pxe_installed(), "dpkg no longer holds the package pxe");
    assert!(
        std::fs::symlink_metadata(PXE_SETTINGS).is_ok(),
        "the settings file is the operator's and stays"
    );
    assert_eq!(pxe_active(), "inactive", "pxe.service is stopped");
}
