//! The serial console baseline of a Linux server (docs/design/console-broker.md, section 5.2): the
//! port its kernel console, GRUB's terminal and a login prompt use, and the two files that put
//! them there. The network install's seed writes these files at install
//! (`hostkeeper-pxe-seed`), and the daemon keeps them on a host whose daemon config names the port
//! (`serial_console`), so both write the same bytes.
//!
//! - `/etc/default/grub.d/99_hostkeeper.cfg` ([`GRUB_CONSOLE_FILE`], [`SerialConsole::grub_defaults`]):
//!   the kernel's `console=tty0 console=<port>,115200n8`, and on a `ttyS<n>` port GRUB's terminal on
//!   that port too (`GRUB_TERMINAL="console serial"` and `GRUB_SERIAL_COMMAND="serial --unit=<n>
//!   --speed=115200"`), so GRUB's output shows on the serial console as well. An Arm `ttyAMA<n>`
//!   port has no GRUB unit, and gets the kernel line alone.
//! - The getty's drop-in ([`SerialConsole::getty_drop_in_path`], [`GETTY_DROP_IN`]), which holds the
//!   login prompt's port at 115200 baud: the stock `serial-getty@.service` runs agetty with
//!   `--keep-baud 115200,57600,38400,9600`, which on a BMC's Serial over LAN drops the port to 9600
//!   at the first Enter, and the console then shows NUL bytes.

/// The GRUB defaults file that adds the serial console. GRUB reads `/etc/default/grub` and then
/// each file of `/etc/default/grub.d/` in name order; the name sorts after a cloud image's
/// `50-cloudimg-settings.cfg`.
pub const GRUB_CONSOLE_FILE: &str = "/etc/default/grub.d/99_hostkeeper.cfg";

/// The text of the getty's drop-in: an empty `ExecStart=` clears the stock unit's command, and
/// agetty at 115200 baud alone, with no `--keep-baud`, is the one that runs. `-o '-p -- \\u'` are
/// the stock unit's options for login (keep the environment; `\u`, which agetty replaces with the
/// user name, written `\\u` because systemd reads a backslash as an escape).
pub const GETTY_DROP_IN: &str =
    "[Service]\nExecStart=\nExecStart=-/sbin/agetty -o '-p -- \\\\u' 115200 - $TERM\n";

/// The file name of the getty's drop-in, in `/etc/systemd/system/serial-getty@<port>.service.d/`.
const GETTY_DROP_IN_FILE: &str = "99_hostkeeper.conf";

/// Why a port was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("is not ttyS<n> or ttyAMA<n>, with n from 0 to 99")]
pub struct NotASerialPort;

/// The serial port the kernel's console and a getty use: `ttyS<n>` (a 16550 port) or `ttyAMA<n>`
/// (an Arm PL011 port), `n` from 0 to 99 written with no leading 0, at 115200 baud, 8 bits, no
/// parity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialConsole(String);

impl SerialConsole {
    /// `text` as a port.
    ///
    /// # Errors
    /// [`NotASerialPort`] for any other text.
    pub fn parse(text: &str) -> Result<SerialConsole, NotASerialPort> {
        let number = text
            .strip_prefix("ttyAMA")
            .or_else(|| text.strip_prefix("ttyS"))
            .ok_or(NotASerialPort)?;
        let digits = (1..=2).contains(&number.len()) & number.bytes().all(|b| b.is_ascii_digit());
        // `ttyS01` would name no device: a number is written without a leading 0.
        let canonical = (number.len() == 1) | !number.starts_with('0');
        (digits & canonical)
            .then(|| SerialConsole(text.to_owned()))
            .ok_or(NotASerialPort)
    }

    /// The port's name, such as `ttyS1`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// GRUB's `serial --unit` for the port: `n` of a `ttyS<n>` (GRUB's unit `n` is the PC's port
    /// COM`n+1`, which Linux names `ttyS<n>`). `None` for a `ttyAMA<n>`, which has no such unit.
    #[must_use]
    pub fn grub_unit(&self) -> Option<&str> {
        self.0.strip_prefix("ttyS")
    }

    /// The kernel argument that puts the kernel's console on the port, as `/proc/cmdline` of a
    /// host booted with [`SerialConsole::grub_defaults`] holds it: `console=ttyS1,115200n8`.
    #[must_use]
    pub fn kernel_argument(&self) -> String {
        format!("console={},115200n8", self.0)
    }

    /// The text of [`GRUB_CONSOLE_FILE`] for the port, as the module says.
    #[must_use]
    pub fn grub_defaults(&self) -> String {
        let mut text = format!(
            "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 {}\"\n",
            self.kernel_argument()
        );
        if let Some(unit) = self.grub_unit() {
            text.push_str(&format!(
                "GRUB_TERMINAL=\"console serial\"\n\
                 GRUB_SERIAL_COMMAND=\"serial --unit={unit} --speed=115200\"\n"
            ));
        }
        text
    }

    /// The port's getty unit: `serial-getty@ttyS1.service`.
    #[must_use]
    pub fn getty_unit(&self) -> String {
        format!("serial-getty@{}.service", self.0)
    }

    /// Where the getty's drop-in ([`GETTY_DROP_IN`]) is:
    /// `/etc/systemd/system/serial-getty@ttyS1.service.d/99_hostkeeper.conf`.
    #[must_use]
    pub fn getty_drop_in_path(&self) -> String {
        format!(
            "/etc/systemd/system/{}.d/{GETTY_DROP_IN_FILE}",
            self.getty_unit()
        )
    }
}
