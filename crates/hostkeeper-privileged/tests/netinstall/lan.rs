//! The farm LAN of a network-install test: a Linux bridge with no route out, in a network
//! namespace of its own, with the boxes of a real LAN on it, each in its own namespace joined to
//! the bridge by a veth pair with its own address (so each binds UDP 67 in its own namespace):
//!
//! - the router (its DHCP is [`super::router`]), at `.2`;
//! - the boot server, `pxe`, at `.3`;
//! - the guest, by a tap device of the bridge's namespace, which QEMU opens there;
//! - the test's own host namespace, at `.1`: the coordinator's private and LAN listeners and the
//!   release channel are bound there, and `hostkeeper ctl` and SSH dial the LAN from there.
//!
//! The bridge is in a namespace of its own so that no firewall of the host (a Docker `FORWARD`
//! policy, with `br_netfilter`) sees its frames. Nothing on the LAN routes beyond it: the router's
//! namespace has only its veth, and the clients' router (option 3) is that address. Every name
//! carries the test's pid; dropping the [`Lan`] deletes the namespaces, and with them every link.

use std::fs::File;
use std::net::Ipv4Addr;
use std::os::fd::AsFd;

use rustix::thread::{LinkNameSpaceType, move_into_link_name_space};

use crate::support::run;

/// `ip`, of iproute2, found on the `PATH` (`/usr/bin` or `/usr/sbin`, by distribution).
const IP: &str = "ip";

/// The LAN of one test.
pub struct Lan {
    /// The namespace of the bridge, where the guest's QEMU runs.
    pub bridge_ns: String,
    /// The router's namespace.
    pub router_ns: String,
    /// The boot server's namespace.
    pub pxe_ns: String,
    /// The host's end of its veth pair.
    host_link: String,
    /// The `/24` the LAN is.
    pub net: [u8; 3],
}

/// The name of the router's and the boot server's interface, each in its own namespace.
pub const LAN_LINK: &str = "lan0";
/// The guest's tap device, in the bridge's namespace.
pub const TAP: &str = "tap0";

fn ip(args: &[&str]) {
    let out = run(IP, args);
    assert!(
        out.status.success(),
        "ip {}: {}{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Lan {
    /// Makes the LAN `<net>.0/24` of the test `tag` (a few lowercase letters).
    pub fn new(tag: &str, net: [u8; 3]) -> Lan {
        let pid = std::process::id();
        let lan = Lan {
            bridge_ns: format!("hk-{tag}-lan-{pid}"),
            router_ns: format!("hk-{tag}-rt-{pid}"),
            pxe_ns: format!("hk-{tag}-px-{pid}"),
            host_link: format!("hk{pid}h"),
            net,
        };
        let cidr = |last: u8| format!("{}/24", lan.address(last));
        for ns in [&lan.bridge_ns, &lan.router_ns, &lan.pxe_ns] {
            ip(&["netns", "add", ns]);
            ip(&["-n", ns, "link", "set", "lo", "up"]);
        }
        let b = lan.bridge_ns.as_str();
        ip(&["-n", b, "link", "add", "br0", "type", "bridge"]);
        ip(&["-n", b, "link", "set", "br0", "up"]);
        // The host: its end in this namespace, the other a port of the bridge.
        ip(&[
            "link",
            "add",
            &lan.host_link,
            "type",
            "veth",
            "peer",
            "name",
            "host",
            "netns",
            b,
        ]);
        ip(&["addr", "add", &cidr(1), "dev", &lan.host_link]);
        ip(&["link", "set", &lan.host_link, "up"]);
        ip(&["-n", b, "link", "set", "host", "master", "br0", "up"]);
        // The router and the boot server.
        for (ns, port, last) in [(&lan.router_ns, "router", 2), (&lan.pxe_ns, "pxe", 3)] {
            ip(&[
                "-n", b, "link", "add", port, "type", "veth", "peer", "name", LAN_LINK, "netns", ns,
            ]);
            ip(&["-n", b, "link", "set", port, "master", "br0", "up"]);
            ip(&["-n", ns, "addr", "add", &cidr(last), "dev", LAN_LINK]);
            ip(&["-n", ns, "link", "set", LAN_LINK, "up"]);
        }
        // The guest's NIC.
        ip(&["-n", b, "tuntap", "add", "dev", TAP, "mode", "tap"]);
        ip(&["-n", b, "link", "set", TAP, "master", "br0", "up"]);
        lan
    }

    /// The address `.last` of the LAN.
    pub fn address(&self, last: u8) -> Ipv4Addr {
        let [a, b, c] = self.net;
        Ipv4Addr::new(a, b, c, last)
    }

    /// The host's address on the LAN.
    pub fn host(&self) -> Ipv4Addr {
        self.address(1)
    }

    /// The router's address.
    pub fn router(&self) -> Ipv4Addr {
        self.address(2)
    }

    /// The boot server's address.
    pub fn pxe(&self) -> Ipv4Addr {
        self.address(3)
    }

    /// The argument list that runs `program` with `args` in the namespace `ns`.
    pub fn exec_in(ns: &str, program: &str, args: &[String]) -> Vec<String> {
        let mut all = vec![
            "netns".to_owned(),
            "exec".to_owned(),
            ns.to_owned(),
            program.to_owned(),
        ];
        all.extend(args.iter().cloned());
        all
    }

    /// Runs `make` in a thread that has moved into the namespace `ns`, and gives what it made:
    /// a socket made there stays of that namespace in any thread.
    pub fn within<T: Send + 'static>(ns: &str, make: impl FnOnce() -> T + Send + 'static) -> T {
        let path = format!("/run/netns/{ns}");
        std::thread::spawn(move || {
            let file = File::open(&path).unwrap_or_else(|e| panic!("open {path}: {e}"));
            move_into_link_name_space(file.as_fd(), Some(LinkNameSpaceType::Network))
                .unwrap_or_else(|e| panic!("enter the namespace {path}: {e}"));
            make()
        })
        .join()
        .unwrap_or_else(|_| panic!("the thread in {ns} failed"))
    }
}

/// The program `ip`, for a child process run in a namespace.
pub fn ip_program() -> &'static str {
    IP
}

impl Drop for Lan {
    fn drop(&mut self) {
        for ns in [&self.bridge_ns, &self.router_ns, &self.pxe_ns] {
            let _ = run(IP, &["netns", "del", ns.as_str()]);
        }
        // Gone with its peer already, unless the bridge's namespace was never made.
        let _ = run(IP, &["link", "del", self.host_link.as_str()]);
    }
}
