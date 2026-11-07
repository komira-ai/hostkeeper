//! The edges of a run: a menu whose platform has no boot set, a seed refused once its record
//! changed, a seed file or menu path that is not one, a DHCP port already taken, a keys file
//! that is not there, the program's own `serve`, and the host's stop on SIGTERM.

use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_pxe_seed::menu::LOCAL_DISK_MENU;

use super::fake;
use super::kit::{ENROLLED, Kit, NOW, RECORDED, client, http, refused, tftp_read};
use crate::serve::ServePorts;

/// In proxy mode a record with no platform installs the platform GRUB states; when that
/// platform has no boot set here, the menu is the local-disk one, and the log says so.
///
/// Catches an install menu that names a kernel and an ISO this server does not hold.
#[test]
fn a_menu_for_a_platform_with_no_boot_set_boots_the_local_disk() {
    let kit = Kit::with("edges-no-set", &[LinuxPlatform::X86_64]);
    let firmware = client();
    let mut run = kit.start(kit.args("proxy", firmware.local_addr().expect("its address"), &[]));
    let menu =
        tftp_read(run.tftp, &format!("/grub/hostkeeper/arm64/{RECORDED}"), &[]).expect("a menu");
    assert_eq!(menu, LOCAL_DISK_MENU.as_bytes());
    assert_eq!(
        run.wait("menu grub/hostkeeper/arm64/"),
        format!(
            "{NOW} menu grub/hostkeeper/arm64/{RECORDED}: local disk: linux-arm64 has no boot set here"
        )
    );
    let malformed = tftp_read(run.tftp, "/grub/hostkeeper/x86_64/not-a-mac", &[]).expect("a menu");
    assert_eq!(malformed, LOCAL_DISK_MENU.as_bytes());
    assert_eq!(
        run.wait("not-a-mac"),
        format!(
            "{NOW} menu grub/hostkeeper/x86_64/not-a-mac: local disk: the menu path's MAC is not well-formed"
        )
    );
    assert_eq!(run.stop().0, Ok(()));
}

/// A machine given its install menu whose record became enrolled before the seed's first fetch
/// is refused the seed (the read at the fetch is fresh), and the log names the reason; a seed
/// file that is not `user-data` or `meta-data` is not served.
///
/// Catches a seed served from a menu's stale answer, which would install a machine that was
/// approved meanwhile.
#[test]
fn a_seed_whose_record_changed_since_its_menu_is_refused() {
    let kit = Kit::new("edges-seed-changed");
    let firmware = client();
    let mut run = kit.start(kit.args("proxy", firmware.local_addr().expect("its address"), &[]));
    tftp_read(
        run.tftp,
        &format!("/grub/hostkeeper/x86_64/{RECORDED}"),
        &[],
    )
    .expect("a menu");
    run.wait("install linux-x86_64 as host new-1");
    assert_eq!(
        http(run.http, "GET", &format!("/seed/{RECORDED}/vendor-data")).status,
        404
    );
    kit.fake().set(|s| {
        s.records = vec![
            fake::record("new-1", &[RECORDED], true),
            fake::record("old-1", &[ENROLLED], true),
        ];
    });
    assert_eq!(
        http(run.http, "GET", &format!("/seed/{RECORDED}/user-data")).status,
        404
    );
    assert_eq!(
        run.wait("is no longer in the answer set"),
        format!(
            "{NOW} seed seed/{RECORDED}/user-data: refused: {RECORDED} is no longer in the answer \
             set as host new-1: the record of host new-1: a host is enrolled against it"
        )
    );
    assert_eq!(run.stop().0, Ok(()));
}

/// A DHCP port another socket holds, and an SSH keys file that is not there, refuse the start
/// with the line that names them.
///
/// Catches a start that serves without its DHCP socket, or with no keys for the installed
/// system's operator account.
#[test]
fn a_taken_dhcp_port_or_a_missing_keys_file_refuses_the_start() {
    let kit = Kit::new("edges-refusals");
    let reply_to = client().local_addr().expect("its address");
    let taken = client();
    let port = taken.local_addr().expect("its address");
    let mut args = kit.args("proxy", reply_to, &[]);
    let at = args
        .iter()
        .position(|a| a == "--dhcp-listen")
        .expect("the flag");
    args[at + 1] = port.to_string().into();
    let (result, lines) = refused(args);
    let why = result.expect_err("refused");
    assert!(
        why.starts_with(&format!("cannot bind the DHCP socket to {port}: ")),
        "{why}"
    );
    assert_eq!(lines, Vec::<String>::new());
    let missing = kit.scratch.path().join("no-keys");
    let mut args = kit.args("proxy", reply_to, &[]);
    let at = args
        .iter()
        .position(|a| a == "--ssh-authorized-keys")
        .expect("the flag");
    args[at + 1] = missing.clone().into();
    let (result, _) = refused(args);
    assert_eq!(
        result,
        Err(format!(
            "{}: No such file or directory (os error 2)",
            missing.display()
        ))
    );
}

/// The program's own `serve` runs with the host's ports: a daemon config that is not there is
/// refused with exit status 1 before any socket is bound.
///
/// Catches a program whose `serve` verb is not wired to the run.
#[test]
fn the_program_runs_serve() {
    let kit = Kit::new("edges-main");
    let mut args = kit.args("router", client().local_addr().expect("its address"), &[]);
    let at = args
        .iter()
        .position(|a| a == "--daemon-config")
        .expect("the flag");
    args[at + 1] = kit.scratch.path().join("missing.toml").into();
    assert_eq!(crate::cli::main(args), std::process::ExitCode::from(1));
}

/// The host's stop ends at SIGTERM: it waits until the signal comes, and the signal does not
/// end the process, whose handler the stop installed.
///
/// Catches a `serve` that SIGTERM kills without its stop line, or one that stops at once.
#[test]
fn the_host_stop_ends_at_sigterm() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let mut stop = (ServePorts::host().stop)();
        assert!(futures::poll!(&mut stop).is_pending());
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TERM)
            .expect("a signal to this process");
        stop.await;
    });
}

/// A proxy offer that cannot be sent (`--dhcp-reply-to` is an address a loopback socket cannot
/// send to) is logged with the address, and the run goes on; a TFTP transfer whose socket
/// cannot be bound at `--address` (an address of no interface here) is logged, and nothing is
/// sent to the reader.
///
/// Catches an offer that silently goes nowhere, and a TFTP read that waits for ever on a
/// server that could not answer it.
#[test]
fn a_send_or_a_transfer_socket_that_fails_is_logged() {
    let kit = Kit::new("edges-send");
    let unreachable: std::net::SocketAddr = "192.0.2.1:68".parse().expect("an address");
    let mut run = kit.start(kit.args("proxy", unreachable, &[]));
    let firmware = client();
    firmware
        .send_to(
            &super::kit::dhcp(1, RECORDED, Some("PXEClient"), Some(7), None),
            run.dhcp,
        )
        .expect("send");
    let line = run.wait("could not be sent");
    assert!(
        line.starts_with(&format!(
            "{NOW} dhcp: the answer to {unreachable} could not be sent: "
        )),
        "{line}"
    );
    assert_eq!(run.stop().0, Ok(()));

    let mut args = kit.args("proxy", client().local_addr().expect("its address"), &[]);
    let at = args
        .iter()
        .position(|a| a == "--address")
        .expect("the flag");
    args[at + 1] = "192.0.2.10".into();
    let mut run = kit.start(args);
    let reader = client();
    reader
        .send_to(&super::kit::rrq("grub/grub.cfg", &[]), run.tftp)
        .expect("send");
    let line = run.wait("no transfer socket");
    assert!(
        line.starts_with(&format!(
            "{NOW} tftp {}: no transfer socket at 192.0.2.10:0: ",
            reader.local_addr().expect("its address")
        )),
        "{line}"
    );
    assert_eq!(run.stop().0, Ok(()));
    assert!(!super::kit::waiting(&reader));
}
