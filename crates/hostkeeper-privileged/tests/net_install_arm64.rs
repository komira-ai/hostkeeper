#![cfg(all(
    target_os = "linux",
    feature = "privileged",
    not(target_arch = "aarch64")
))]
//! Network install of an arm64 machine, the fallback of docs/design/network-install.md section 11
//! (net-install.yml's job `net-install-arm64`): the hosted arm64 runner exposes no `/dev/kvm`
//! (actions/runner-images#14062, closed as not planned), so the guest is an arm64 QEMU guest with
//! the AAVMF firmware under TCG on the x86-64 runner, which the full live-server install does not
//! fit; it boots until it fetches its seed. That is every part of the boot chain that differs by
//! platform: the architecture its DISCOVER states (11), the arm64 shim and GRUB, the arm64 menu
//! of its MAC and a seed that names the arm64 deb. The install, the deb and the enrollment of an
//! arm64 machine are proven on the first real one (item N13), by the runbook.
//!
//! It runs in router mode, the record stating `linux-arm64`: the guest's REQUEST states
//! architecture 11 and `hostkeeper/boot.efi` is served as `hostkeeper/shimaa64.efi`.

mod e2e;
mod netinstall;
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

use netinstall::Rig;
use netinstall::guest::{Arch, Machine};
use netinstall::pxe::Mode;

/// The record's host id.
const HOST: &str = "net-arm-1";

fn machine() -> Machine {
    Machine {
        arch: Arch::Arm64,
        mac: "52:54:00:4e:10:02",
        uuid: "6f1e2d3c-4b5a-4978-8a1b-2c3d4e5f6072",
        serial: "HKNET-ARM-0001",
    }
}

/// `GET <path>` over plain HTTP from `server`: the body, failing the test unless it is a 200.
fn http_get(server: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(server).expect("connect to pxe's HTTP");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {server}\r\nConnection: close\r\n\r\n"
    )
    .expect("send the request");
    let mut reply = String::new();
    stream.read_to_string(&mut reply).expect("the answer");
    let (head, body) = reply.split_once("\r\n\r\n").expect("a head and a body");
    assert!(head.starts_with("HTTP/1.1 200 "), "GET {path}: {head}");
    body.to_owned()
}

#[test]
fn arm64_boots_to_seed_fetch() {
    let mut rig = Rig::new("net_install_arm64", "a", [198, 51, 100], machine(), HOST);
    let mac = rig.mac();
    rig.serve(Mode::Router, &[]);
    let start = rig.bus.mark();
    rig.guest.start(&rig.bus);

    // The REQUEST states arm64 UEFI, and hostkeeper/boot.efi is the arm64 shim.
    let (at, _) = rig.pxe_says(
        start,
        "the guest's REQUEST, architecture 11",
        &[&format!(
            "dhcp: DHCPREQUEST from {mac}, architecture 11, PXEClient, "
        )],
    );
    let (at, _) = rig.pxe_says(
        at,
        "hostkeeper/boot.efi served as the arm64 shim",
        &[&format!(
            "hostkeeper/boot.efi is hostkeeper/shimaa64.efi for {mac} (linux-arm64)"
        )],
    );
    let (at, _) = rig.pxe_says(
        at,
        "the arm64 shim served",
        &["sent hostkeeper/shimaa64.efi"],
    );
    rig.pxe_says(
        at,
        "the arm64 GRUB served",
        &["sent hostkeeper/grubaa64.efi"],
    );
    // GRUB asks for the arm64 menu of its MAC, and the live system fetches its seed.
    rig.wait_install(start, Mode::Router);

    // The seed, as the guest fetched it (a MAC whose install started may fetch it again): it
    // installs the arm64 deb.
    let pxe = SocketAddr::from((rig.lan.pxe(), 80));
    let user_data = http_get(pxe, &format!("/seed/{mac}/user-data"));
    let deb = format!("http://{}/deb/hostkeeper_arm64.deb", rig.lan.pxe());
    assert!(
        user_data.contains(&deb),
        "the seed names {deb}:\n{user_data}"
    );
    assert!(
        !user_data.contains("hostkeeper_amd64.deb"),
        "and not the amd64 deb:\n{user_data}"
    );
}
