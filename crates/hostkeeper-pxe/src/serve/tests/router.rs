//! Router mode, end to end: no DHCP packet is ever sent; DISCOVERs and REQUESTs teach which
//! client took which address and its architecture; `hostkeeper/boot.efi` is the shim of that
//! architecture; and every MAC gets the fixed `grub.cfg` and, while the inventory's records carry
//! no platform (item N14 of the design), the local-disk menu.

use std::net::Ipv4Addr;

use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_pxe_seed::menu::LOCAL_DISK_MENU;

use super::kit::{
    ENROLLED, Kit, NOW, RECORDED, SHIM_BYTES, client, content, dhcp, tftp_read, tftp_read_from,
    waiting,
};

const PXE: Option<&str> = Some("PXEClient:Arch:00007:UNDI:003016");

/// For one recorded and one enrolled MAC in router mode: the start binds no proxyDHCP socket;
/// the DISCOVER and the selecting REQUEST of each are logged as learned, and nothing is sent
/// for any of them; `hostkeeper/boot.efi` read from the address a REQUEST named is the x86-64
/// shim for architecture 7 and the arm64 shim for architecture 11, a REQUEST with no option 93
/// taking the architecture of its MAC's DISCOVER; both MACs get the fixed `grub.cfg` and the
/// local-disk menu, the recorded one because router mode needs the record's platform.
///
/// Catches a router-mode server that answers DHCP and fights the router, one that serves an
/// arm64 machine the x86-64 shim, and one that installs a record whose platform is not stated.
#[test]
fn router_mode_learns_the_architecture_sends_no_dhcp_and_serves_its_shim() {
    let kit = Kit::new("router-learn");
    let reply_to = client();
    let mut run = kit.start(kit.args("router", reply_to.local_addr().expect("its address"), &[]));
    assert!(
        run.seen[0].contains(", proxyDHCP not bound, "),
        "{}",
        run.seen[0]
    );
    assert_eq!(run.proxy, None);
    let firmware = client();
    let here = Some(Ipv4Addr::LOCALHOST);

    firmware
        .send_to(&dhcp(1, RECORDED, PXE, Some(7), None), run.dhcp)
        .expect("send");
    assert_eq!(
        run.wait(&format!("DHCPDISCOVER from {RECORDED}")),
        format!(
            "{NOW} dhcp: DHCPDISCOVER from {RECORDED}, architecture 7, PXEClient, on the DHCP port: \
             its architecture is kept for its REQUEST"
        )
    );
    firmware
        .send_to(&dhcp(3, RECORDED, PXE, Some(7), here), run.dhcp)
        .expect("send");
    assert_eq!(
        run.wait(&format!("DHCPREQUEST from {RECORDED}")),
        format!(
            "{NOW} dhcp: DHCPREQUEST from {RECORDED}, architecture 7, PXEClient, on the DHCP port: \
             it takes 127.0.0.1"
        )
    );
    let reader = client();
    let at = reader.local_addr().expect("its address");
    let shim = tftp_read_from(&reader, run.tftp, "hostkeeper/boot.efi", &[]).expect("the shim");
    assert_eq!(shim, content(LinuxPlatform::X86_64, "shim", SHIM_BYTES));
    assert_eq!(
        run.wait("hostkeeper/boot.efi is"),
        format!(
            "{NOW} tftp {at}: hostkeeper/boot.efi is hostkeeper/shimx64.efi for {RECORDED} (linux-x86_64)"
        )
    );
    assert_eq!(
        run.wait("sent hostkeeper/shimx64.efi"),
        format!("{NOW} tftp {at}: sent hostkeeper/shimx64.efi ({SHIM_BYTES} bytes)")
    );

    // The enrolled machine is arm64: its DISCOVER states 11, its REQUEST nothing.
    firmware
        .send_to(&dhcp(1, ENROLLED, PXE, Some(11), None), run.dhcp)
        .expect("send");
    run.wait(&format!("DHCPDISCOVER from {ENROLLED}"));
    firmware
        .send_to(&dhcp(3, ENROLLED, PXE, None, here), run.dhcp)
        .expect("send");
    assert_eq!(
        run.wait(&format!("DHCPREQUEST from {ENROLLED}")),
        format!(
            "{NOW} dhcp: DHCPREQUEST from {ENROLLED}, architecture none, PXEClient, on the DHCP port: \
             it takes 127.0.0.1"
        )
    );
    let shim = tftp_read(run.tftp, "/hostkeeper/boot.efi", &[]).expect("the shim");
    assert_eq!(shim, content(LinuxPlatform::Arm64, "shim", SHIM_BYTES));
    run.wait(&format!(
        "hostkeeper/boot.efi is hostkeeper/shimaa64.efi for {ENROLLED} (linux-arm64)"
    ));

    for mac in [RECORDED, ENROLLED] {
        let cfg = tftp_read(run.tftp, "/grub/grub.cfg", &[]).expect("grub.cfg");
        assert_eq!(
            cfg,
            b"configfile ${prefix}/hostkeeper/${grub_cpu}/${net_default_mac}\n"
        );
        let menu =
            tftp_read(run.tftp, &format!("/grub/hostkeeper/x86_64/{mac}"), &[]).expect("the menu");
        assert_eq!(menu, LOCAL_DISK_MENU.as_bytes());
    }
    let (_, lines) = run.stop();
    let menus: Vec<&String> = lines.iter().filter(|l| l.contains(" menu ")).collect();
    assert_eq!(
        menus,
        [
            &format!(
                "{NOW} menu grub/hostkeeper/x86_64/{RECORDED}: local disk: {RECORDED}: not in the \
                 answer set: the record of host new-1 has no platform, which router mode needs"
            ),
            &format!(
                "{NOW} menu grub/hostkeeper/x86_64/{ENROLLED}: local disk: {ENROLLED}: not in the \
                 answer set: the record of host old-1: a host is enrolled against it"
            ),
        ]
    );
    assert!(!waiting(&reply_to), "router mode sent a DHCP packet");
    assert!(!waiting(&firmware), "router mode sent a DHCP packet");
}

/// `hostkeeper/boot.efi` is refused with TFTP error 1 (file not found), and logged with the
/// reader's address, for an address no REQUEST named, and for a client whose REQUEST stated
/// architecture 0 (legacy BIOS), which this design does not install.
///
/// Catches a server that guesses a shim for a client it learned nothing of, or hands a BIOS
/// firmware an EFI binary.
#[test]
fn boot_efi_is_refused_with_no_lease_or_an_architecture_not_installed() {
    let kit = Kit::new("router-refuse");
    let reply_to = client();
    let mut run = kit.start(kit.args("router", reply_to.local_addr().expect("its address"), &[]));
    let reader = client();
    let at = reader.local_addr().expect("its address");
    let refused = tftp_read_from(&reader, run.tftp, "hostkeeper/boot.efi", &[]);
    assert_eq!(refused, Err((1, "file not found".to_owned())));
    assert_eq!(
        run.wait("refused"),
        format!("{NOW} tftp {at}: refused: hostkeeper/boot.efi from an address with no lease")
    );

    let firmware = client();
    firmware
        .send_to(
            &dhcp(3, RECORDED, PXE, Some(0), Some(Ipv4Addr::LOCALHOST)),
            run.dhcp,
        )
        .expect("send");
    run.wait("it takes 127.0.0.1");
    let refused = tftp_read_from(&reader, run.tftp, "hostkeeper/boot.efi", &[]);
    assert_eq!(refused, Err((1, "file not found".to_owned())));
    assert_eq!(
        run.wait("refused"),
        format!(
            "{NOW} tftp {at}: refused: hostkeeper/boot.efi for {RECORDED}, whose architecture 0 \
             is not installed"
        )
    );
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&reply_to));
}
