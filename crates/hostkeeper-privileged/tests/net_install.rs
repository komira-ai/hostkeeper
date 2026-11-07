#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "privileged"))]
//! Network install of an x86-64 machine, end to end (docs/design/network-install.md section 11,
//! privileged.yml's job `net-install`): a QEMU guest with OVMF under KVM, an empty disk, a fixed
//! UUID and serial, its NIC first in the boot order, on a LAN with no route out, with the
//! router's DHCP faked by the test, the real `hostkeeper-pxe` and the real coordinator with its
//! LAN listener (`netinstall`). The operator acts only through `hostkeeper ctl`, and the guest is
//! only booted, rebooted (by its operator account) and read.
//!
//! In order, it checks:
//!
//! 1. Proxy mode: the router's DHCP saw the guest's DISCOVER and `hostkeeper-pxe` sent its proxy
//!    offer; the guest got the install menu of its MAC and fetched its seed; the reboot after the
//!    install got no proxy offer (its MAC left the answer set) and booted the disk.
//! 2. A pending request appears, matched to the record, with claims equal to the guest's UUID and
//!    serial; the console shows its key hash; `Approve` by that hash succeeds; the first
//!    heartbeat arrives; the fixture service assigned to the host's role reports installed.
//! 3. Router mode, the enrolled guest rebooted: `hostkeeper-pxe` learns its address from its
//!    REQUEST, serves `hostkeeper/boot.efi` as the x86-64 shim, and its local-disk menu; its
//!    heartbeat comes back from the same disk.
//! 4. Reinstall with `--reinstall`, in proxy mode (router mode installs once the record has its
//!    platform, item N14): the install menu, a request flagged `duplicate_claim`, an approval
//!    without `--replace` refused, the `--replace` approval, and the old certificate's heartbeat
//!    refused `forbidden`.
//! 5. The host retired (`Hosts.Retire`), the coordinator restarted, the guest network-booted
//!    without `--reinstall`: no answer in proxy mode, the local-disk menu in router mode, and each
//!    time the same installed system boots from its disk, with no seed fetched.
//!
//! The guest's console, the logs and the coordinator's are kept under
//! `target/privileged-artifacts/net_install/`, with the inputs of the measurement of the first
//! install that are not logs (design section 11, item N11): `pending.json`, the pending list as
//! `enrollment list` gave it before the approval; `host.json`, the host as `hosts get` gave it
//! once the fixture service was installed; and `admin-status.txt`, what `hostkeeper admin
//! status` printed on the guest then. The job's step after this test makes the report of them
//! with `cargo xtask netinstall-report`; no test asserts a time.

mod e2e;
mod netinstall;
mod support;

use std::path::Path;

use netinstall::events::Source;
use netinstall::guest::{Arch, Machine};
use netinstall::pxe::Mode;
use netinstall::{Rig, SERVICE, VERSION, fixture, heartbeat};
use serde_json::Value;
use support::pair::{Coordinator, ROLE, put};

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

/// A unix time the ctl prints (a string), as a number.
fn unix(value: &Value) -> Option<u64> {
    value.as_str().and_then(|s| s.parse().ok())
}

/// The host as `hosts get` gives it, or why not.
fn host_of(coordinator: &Coordinator) -> Result<Value, String> {
    coordinator
        .ctl(&["hosts", "get", "--node-id", HOST])
        .map(|got| got["host"].clone())
}

/// The record's `hostHeld`, as `enrollment list-records` gives it.
fn host_held(coordinator: &Coordinator) -> Result<Value, String> {
    let records = coordinator.ctl(&["enrollment", "list-records"])?;
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

    // The fixture service: its package on the channel, its desired version set; the rollout
    // file runs it on the record's role, so the host is assigned it at its approval.
    let package = fixture(&rig.signer);
    rig.channel.serve(&package);
    let request = rig.scratch().join("desired.json");
    put(
        &request,
        format!("{{\"desired\":{{\"version\":\"{VERSION}\"}}}}").as_bytes(),
        0o644,
    );
    rig.ctl(&[
        "releases",
        "set-desired",
        "--service",
        SERVICE,
        "--package-digest",
        &package.digest,
        "--request",
        &request.display().to_string(),
    ]);

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
    let pending = rig.ctl(&["enrollment", "list"]);
    rig.keep("pending.json", pending.to_string().as_bytes());
    let key = request["keySha256"]
        .as_str()
        .expect("a key hash")
        .to_owned();
    rig.console_shows(seeded, &key);
    rig.trust_channel();
    let approved = rig.ctl(&[
        "enrollment",
        "approve",
        "--request-id",
        request["requestId"].as_str().expect("a request id"),
        "--key-sha256",
        &key,
        "--role",
        ROLE,
        "--reason",
        "network-install test: the key hash is the console's",
    ]);
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
    let got = rig.ctl(&["hosts", "get", "--node-id", HOST]);
    rig.keep("host.json", got.to_string().as_bytes());
    let status = rig
        .ssh
        .output("sudo hostkeeper admin status", b"")
        .expect("the guest's admin status");
    let text = String::from_utf8_lossy(&status);
    assert!(
        text.contains(&format!("service[{SERVICE}].last.outcome: succeeded\n")),
        "the fixture's install is the daemon's last order of it, succeeded: {text}"
    );
    rig.keep("admin-status.txt", &status);

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

    // 4. Reinstall, in proxy mode: the old identity is kept to be refused.
    let old_cert = rig
        .ssh
        .output("sudo cat /etc/updater/pki/current/cert.pem", b"")
        .expect("the old certificate");
    let old_key = rig
        .ssh
        .output("sudo cat /etc/updater/pki/current/key", b"")
        .expect("the old key");
    rig.serve(Mode::Proxy, &[HOST]);
    let from = rig.bus.mark();
    rig.ssh.reboot();
    let seeded = rig.wait_install(from, Mode::Proxy);
    rig.login_prompt(seeded);
    let again = rig.wait_request(std::slice::from_ref(&key));
    assert_eq!(
        again["duplicateClaim"], true,
        "the new key's request: {again}"
    );
    let new_key = again["keySha256"].as_str().expect("a key hash").to_owned();
    rig.console_shows(seeded, &new_key);
    rig.trust_channel();
    let approve = |replace: Option<&str>| {
        let mut args = vec![
            "enrollment",
            "approve",
            "--request-id",
            again["requestId"].as_str().expect("a request id"),
            "--key-sha256",
            &new_key,
            "--role",
            ROLE,
            "--reason",
            "network-install test: reinstalled, the key hash is the console's",
        ];
        if let Some(replace) = replace {
            args.extend(["--replace", replace]);
        }
        rig.coordinator.ctl(&args)
    };
    let refused = approve(None).expect_err("approval of a duplicate claim without --replace");
    assert!(refused.contains("refused: duplicate_claim"), "{refused}");
    approve(Some("true")).expect("the --replace approval");
    let ca_der = std::fs::read(rig.coordinator.ca_cert_path()).expect("the CA");
    let lan = rig.coordinator.lan.expect("the LAN listener");
    let (status, body) = heartbeat::post(lan, &ca_der, &old_cert, &old_key);
    assert_eq!(status, 403, "the old certificate's heartbeat: {body}");
    assert!(body.contains("forbidden"), "{body}");
    wait_installed(&mut rig, first_cert);

    // 5. Retired, the coordinator restarted, and booted from the network again.
    rig.ctl(&[
        "hosts",
        "retire",
        "--node-id",
        HOST,
        "--reason",
        "network-install test: retired",
    ]);
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
