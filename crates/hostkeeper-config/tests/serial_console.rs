//! The serial console port and the two files of its baseline (docs/design/console-broker.md,
//! section 5.2), which the seed and the daemon both write.

use hostkeeper_config::serial_console::{
    GETTY_DROP_IN, GRUB_CONSOLE_FILE, NotASerialPort, SerialConsole,
};

fn port(text: &str) -> SerialConsole {
    SerialConsole::parse(text).expect(text)
}

/// Catches a port that names no device being taken, or a real one refused: `ttyS<n>` and
/// `ttyAMA<n>` with `n` from 0 to 99 and no leading 0, and nothing else.
#[test]
fn a_port_is_ttys_or_ttyama_with_a_number_up_to_99() {
    for good in ["ttyS0", "ttyS1", "ttyS10", "ttyS99", "ttyAMA0", "ttyAMA42"] {
        assert_eq!(port(good).as_str(), good);
    }
    for bad in [
        "",
        "ttyS",
        "ttyS01",
        "ttyS100",
        "ttyS1a",
        "ttyAMA",
        "ttyAMA00",
        "ttyUSB0",
        "COM2",
        " ttyS1",
        "ttyS1 ",
        "/dev/ttyS1",
    ] {
        assert_eq!(SerialConsole::parse(bad), Err(NotASerialPort), "{bad:?}");
    }
    assert_eq!(
        NotASerialPort.to_string(),
        "is not ttyS<n> or ttyAMA<n>, with n from 0 to 99"
    );
}

/// The fleet's port, COM2 = `ttyS1`: the GRUB file puts the kernel's console and GRUB's terminal
/// on GRUB's unit 1 at 115200 baud, byte for byte as console.md's hand step writes it.
#[test]
fn the_grub_file_of_ttys1_puts_the_kernel_and_grub_on_unit_1() {
    let ttys1 = port("ttyS1");
    assert_eq!(GRUB_CONSOLE_FILE, "/etc/default/grub.d/99_hostkeeper.cfg");
    assert_eq!(
        ttys1.grub_defaults(),
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console=ttyS1,115200n8\"\n\
         GRUB_TERMINAL=\"console serial\"\n\
         GRUB_SERIAL_COMMAND=\"serial --unit=1 --speed=115200\"\n"
    );
    assert_eq!(ttys1.kernel_argument(), "console=ttyS1,115200n8");
    assert_eq!(ttys1.grub_unit(), Some("1"));
    assert_eq!(port("ttyS12").grub_unit(), Some("12"));
}

/// Catches an Arm port given a GRUB serial unit, which numbers the PC's COM ports only: its file
/// is the kernel line alone.
#[test]
fn an_arm_port_gets_the_kernel_line_alone() {
    let arm = port("ttyAMA0");
    assert_eq!(arm.grub_unit(), None);
    assert_eq!(
        arm.grub_defaults(),
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console=ttyAMA0,115200n8\"\n"
    );
}

/// The getty's drop-in is the port's own and holds agetty at 115200 baud with no `--keep-baud`,
/// with the backslash of `\u` doubled for systemd.
#[test]
fn the_getty_drop_in_is_the_ports_own_at_115200_baud() {
    let ttys1 = port("ttyS1");
    assert_eq!(ttys1.getty_unit(), "serial-getty@ttyS1.service");
    assert_eq!(
        ttys1.getty_drop_in_path(),
        "/etc/systemd/system/serial-getty@ttyS1.service.d/99_hostkeeper.conf"
    );
    assert_eq!(
        GETTY_DROP_IN,
        "[Service]\nExecStart=\nExecStart=-/sbin/agetty -o '-p -- \\\\u' 115200 - $TERM\n"
    );
    assert!(!GETTY_DROP_IN.contains("keep-baud"));
}
