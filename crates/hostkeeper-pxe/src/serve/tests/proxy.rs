//! Proxy mode, end to end: the proxy offer and ACK go only to a MAC of the answer set, with the
//! boot file of the architecture the firmware states; the install menu, the seed, and the MAC
//! leaving the set at its seed fetch.

use std::net::Ipv4Addr;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_pxe_seed::inputs::{
    BootServer, DaemonConfigText, InstallDisk, IsoName, LinuxPlatform, Record,
};
use hostkeeper_pxe_seed::keys::AuthorizedKeys;
use hostkeeper_pxe_seed::menu::{LOCAL_DISK_MENU, MenuChoice, MenuRequest, Mode, install_menu};
use hostkeeper_pxe_seed::seed::{SeedInputs, seed};

use super::kit::{
    DAEMON_CONFIG, ENROLLED, Kit, NOW, RECORDED, client, dhcp, http, options, receive, tftp_read,
    waiting,
};

/// For one recorded and one enrolled MAC, a DISCOVER of each with the PXE vendor class: only
/// the recorded one gets the proxy offer, with no address, the echoed vendor class and the TFTP
/// name of the x86-64 shim for architecture 7; the enrolled one gets nothing, and the log says
/// why. A REQUEST of the recorded MAC to the proxyDHCP port gets the ACK, from that port, to
/// the address it came from; an HTTP boot client (architecture 19) gets the `http://` URL of the
/// arm64 shim on the run's HTTP port.
///
/// Catches a proxy that answers an enrolled host (and wipes it), one that gives an address and
/// fights the router, and one that names the wrong architecture's shim.
#[test]
fn proxy_mode_offers_the_boot_file_only_to_a_mac_of_the_answer_set() {
    let kit = Kit::new("proxy-offer");
    let firmware = client();
    let mut run = kit.start(kit.args("proxy", firmware.local_addr().expect("its address"), &[]));
    let pxe = Some("PXEClient:Arch:00007:UNDI:003016");
    let dhcp_port = run.dhcp;
    firmware
        .send_to(&dhcp(1, ENROLLED, pxe, Some(7), None), dhcp_port)
        .expect("send");
    let line = run.wait(&format!("from {ENROLLED}"));
    assert_eq!(
        line,
        format!(
            "{NOW} dhcp: DHCPDISCOVER from {ENROLLED}, architecture 7, PXEClient, on the DHCP port: \
             no answer: not in the answer set: the record of host old-1: a host is enrolled against it"
        )
    );
    firmware
        .send_to(&dhcp(1, RECORDED, pxe, Some(7), None), dhcp_port)
        .expect("send");
    let line = run.wait(&format!("from {RECORDED}"));
    assert_eq!(
        line,
        format!(
            "{NOW} dhcp: DHCPDISCOVER from {RECORDED}, architecture 7, PXEClient, on the DHCP port: \
             proxy offer sent, boot file hostkeeper/shimx64.efi (linux-x86_64)"
        )
    );
    // The first datagram is the recorded MAC's offer: the enrolled one's DISCOVER, sent first,
    // was answered with nothing.
    let (offer, from) = receive(&firmware);
    assert_eq!(from, dhcp_port);
    assert_eq!(&offer[16..20], &[0, 0, 0, 0], "yiaddr");
    assert_eq!(&offer[28..34], &super::kit::octets(RECORDED));
    let offered = options(&offer);
    assert_eq!(offered[&53], [2]);
    assert_eq!(offered[&60], b"PXEClient");
    assert_eq!(offered[&67], b"hostkeeper/shimx64.efi");
    assert!(offered.contains_key(&43));

    let proxy = run.proxy.expect("proxy mode binds the proxyDHCP port");
    let pxe_client = client();
    pxe_client
        .send_to(
            &dhcp(3, RECORDED, pxe, Some(7), Some(Ipv4Addr::new(10, 0, 0, 9))),
            proxy,
        )
        .expect("send");
    let (acked, from) = receive(&pxe_client);
    assert_eq!(from, proxy);
    assert_eq!(options(&acked)[&53], [5]);
    assert_eq!(options(&acked)[&67], b"hostkeeper/shimx64.efi");
    run.wait("proxy ACK sent");

    firmware
        .send_to(
            &dhcp(1, RECORDED, Some("HTTPClient"), Some(19), None),
            dhcp_port,
        )
        .expect("send");
    let (offer, _) = receive(&firmware);
    assert_eq!(
        String::from_utf8_lossy(&options(&offer)[&67]),
        format!(
            "http://127.0.0.1:{}/hostkeeper/shimaa64.efi",
            run.http.port()
        )
    );
    assert_eq!(options(&offer)[&60], b"HTTPClient");
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&firmware));
}

/// The install of the recorded MAC: GRUB reads the fixed `grub.cfg` and its menu by TFTP and
/// gets the install menu of the platform its path names, which names this server's HTTP port;
/// the enrolled MAC gets the local-disk menu. The installer then fetches the seed: `meta-data`
/// and `user-data` are served; that fetch takes the MAC out of the answer set, so its next
/// DISCOVER gets no offer and its next menu is the local-disk one; and the seed stays readable
/// for the install that started. A seed of a MAC that was given no install menu is refused 404.
///
/// Catches a reboot after the install that installs again, a seed served to a machine that was
/// never meant to install, and a menu that names another server or port than the run's.
#[test]
fn a_mac_leaves_the_answer_set_once_its_seed_is_fetched() {
    let kit = Kit::new("proxy-install");
    let firmware = client();
    let mut run = kit.start(kit.args("proxy", firmware.local_addr().expect("its address"), &[]));
    let grub_cfg = tftp_read(run.tftp, "/grub/grub.cfg", &[]).expect("grub.cfg");
    assert_eq!(
        grub_cfg,
        b"configfile ${prefix}/hostkeeper/${grub_cpu}/${net_default_mac}\n"
    );
    let menu = tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{RECORDED}"),
        &[],
    )
    .expect("the menu");
    let server = BootServer::new(std::net::SocketAddrV4::new(
        Ipv4Addr::LOCALHOST,
        run.http.port(),
    ))
    .expect("a boot server");
    let iso: IsoName = "ubuntu-24.04.5-live-server-amd64.iso"
        .parse()
        .expect("an ISO");
    let mac = MacAddress::parse(RECORDED).expect("a MAC");
    assert_eq!(
        String::from_utf8(menu).expect("text"),
        install_menu(&server, LinuxPlatform::X86_64, &iso, mac)
    );
    assert_eq!(
        run.wait("menu grub/hostkeeper/x86_64/"),
        format!("{NOW} menu grub/hostkeeper/x86_64/{RECORDED}: install linux-x86_64 as host new-1")
    );
    let other = tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{ENROLLED}"),
        &[],
    )
    .expect("the menu");
    assert_eq!(other, LOCAL_DISK_MENU.as_bytes());
    assert_eq!(
        http(run.http, "GET", &format!("/seed/{ENROLLED}/user-data")).status,
        404
    );
    assert_eq!(
        run.wait(&format!("seed seed/{ENROLLED}")),
        format!(
            "{NOW} seed seed/{ENROLLED}/user-data: refused: {ENROLLED} was given no install menu in this run"
        )
    );

    let meta = http(run.http, "GET", &format!("/seed/{RECORDED}/meta-data"));
    assert_eq!(meta.status, 200);
    assert_eq!(meta.body, b"instance-id: hostkeeper-new-1\n");
    assert_eq!(
        run.wait("its install started"),
        format!(
            "{NOW} seed seed/{RECORDED}/meta-data: fetched by {RECORDED}, host new-1: its install \
             started, and it leaves the answer set for the rest of the run"
        )
    );
    let user = http(run.http, "GET", &format!("/seed/{RECORDED}/user-data"));
    assert_eq!(user.status, 200);
    let record = Record {
        host_id: "new-1".parse().expect("a host id"),
        role: None,
        platform: None,
    };
    let path = format!("grub/hostkeeper/x86_64/{RECORDED}");
    let MenuChoice::Install(install) = MenuRequest::parse(&path)
        .expect("a menu path")
        .choose(Mode::Proxy, &|_| Some(&record))
    else {
        panic!("the install")
    };
    let want = seed(&SeedInputs {
        install: &install,
        server,
        daemon_config: &DaemonConfigText::parse(DAEMON_CONFIG).expect("a config"),
        keys: &AuthorizedKeys::parse(&std::fs::read_to_string(&kit.keys).expect("the keys file"))
            .expect("keys"),
        install_disk: &InstallDisk::default(),
        serial_console: None,
    });
    assert_eq!(String::from_utf8(user.body).expect("text"), want.user_data);
    assert!(
        want.user_data.contains(&format!(
            "http://127.0.0.1:{}/deb/hostkeeper_amd64.deb",
            run.http.port()
        )),
        "{}",
        want.user_data
    );
    firmware
        .send_to(
            &dhcp(1, RECORDED, Some("PXEClient"), Some(7), None),
            run.dhcp,
        )
        .expect("send");
    assert_eq!(
        run.wait(&format!("DHCPDISCOVER from {RECORDED}")),
        format!(
            "{NOW} dhcp: DHCPDISCOVER from {RECORDED}, architecture 7, PXEClient, on the DHCP port: \
             no answer: not in the answer set: the record of host new-1: its seed was fetched in \
             this run, so its install started"
        )
    );
    let again = tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{RECORDED}"),
        &[],
    )
    .expect("the menu");
    assert_eq!(again, LOCAL_DISK_MENU.as_bytes());
    let (result, _) = run.stop();
    assert_eq!(result, Ok(()));
    assert!(!waiting(&firmware));
}
