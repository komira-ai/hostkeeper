//! The host's BMC as the enrollment claims ask it ([`hostkeeper_enroll::Ipmi`]): `Get LAN
//! Configuration Parameters` sent to the Linux kernel's IPMI device, `/dev/ipmi0`
//! ([`DevIpmi::host`]), whose answer for parameter 5 is the BMC's MAC address
//! ([`hostkeeper_enroll::parse_lan_mac`] reads it).
//!
//! The device is asked with two `ioctl`s, one that sends the request and one that receives the
//! answer, which the workspace's crates cannot make, since they forbid `unsafe`. They are made by
//! `ipmi_rs::File`, which wraps them in safe calls, and nothing of it leaves this module: the
//! port's answer is plain bytes.
//!
//! A host with no BMC has no device (or a device that does not answer): every question is then
//! an `Err`, which the claims take as an absent BMC MAC, never as a failed enrollment. Only root
//! can open the device; a daemon that is not root claims no BMC MAC either.
//!
//! The health look reads the BMC through the same device with [`BmcPort`]
//! (docs/design/host-health-alerts.md section 4.5): `Get Device ID`, `Get SEL Info`,
//! `Get SEL Entry`, `Get SDR Repository Info` and `Get SDR`, each with reservation 0, and nothing
//! else. Those requests are one closed table, [`BmcRequest`], so `Reserve SEL` (which can cancel
//! another tool's reservation), `Clear SEL` and `Delete SEL Entry` (which destroy evidence) cannot
//! be sent. Every question of this module, the claims' and the health look's, is asked under one
//! lock ([`DEVICE`]), one at a time.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_enroll::Ipmi;
use ipmi_rs::File;
use ipmi_rs::connection::{
    IpmiConnection, LogicalUnit, Message, NetFn, Request, RequestTargetAddress, Response,
};

/// The Linux kernel's IPMI device of the host's first BMC interface.
pub const IPMI_DEVICE: &str = "/dev/ipmi0";

/// How long one question waits for the BMC's answer. A BMC answers a LAN parameter in
/// milliseconds, and a channel it does not have with a completion code, not with silence; one
/// that has not answered after this is taken as one that cannot be asked. The claims ask at most
/// eleven channels, so a BMC that answers nothing holds a first enrollment step at most eleven
/// times this.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(1);

/// The command `Get LAN Configuration Parameters` of the network function Transport (0x0C).
const GET_LAN_CONFIGURATION_PARAMETERS: u8 = 0x02;

/// The lock every question to the device is asked under, so the claims' reads of the BMC's
/// address and the health look's reads of its event log never cross on the device.
static DEVICE: Mutex<()> = Mutex::new(());

/// The network function App (IPMI 2.0 section 5.1).
pub const NETFN_APP: u8 = 0x06;

/// The network function Storage (IPMI 2.0 section 5.1).
pub const NETFN_STORAGE: u8 = 0x0A;

/// What a whole record is read with in `Get SEL Entry` and `Get SDR`: offset 0 and "the whole
/// record" (0xFF), which need no reservation, so the reservation id sent is 0.
const WHOLE_RECORD: [u8; 2] = [0x00, 0xFF];

/// Every request the health look may send the BMC (docs/design/host-health-alerts.md section
/// 4.5). Each reads; none reserves, clears, deletes or sets anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcRequest {
    /// `Get Device ID` (App, 0x01): whether a BMC answers at all.
    DeviceId,
    /// `Get SEL Info` (Storage, 0x40): the event log's count, last addition and last erase.
    SelInfo,
    /// `Get SEL Entry` (Storage, 0x43) of the record id, whole, reservation 0.
    SelEntry(u16),
    /// `Get SDR Repository Info` (Storage, 0x20): the sensor records' last addition and erase.
    SdrInfo,
    /// `Get SDR` (Storage, 0x23) of the record id, whole, reservation 0.
    Sdr(u16),
}

impl BmcRequest {
    /// Its network function and command.
    #[must_use]
    pub fn command(self) -> (u8, u8) {
        match self {
            BmcRequest::DeviceId => (NETFN_APP, 0x01),
            BmcRequest::SelInfo => (NETFN_STORAGE, 0x40),
            BmcRequest::SelEntry(_) => (NETFN_STORAGE, 0x43),
            BmcRequest::SdrInfo => (NETFN_STORAGE, 0x20),
            BmcRequest::Sdr(_) => (NETFN_STORAGE, 0x23),
        }
    }

    /// Its request data: none, or reservation 0, the record id (least significant byte first),
    /// offset 0 and the whole record.
    #[must_use]
    pub fn data(self) -> Vec<u8> {
        match self {
            BmcRequest::DeviceId | BmcRequest::SelInfo | BmcRequest::SdrInfo => Vec::new(),
            BmcRequest::SelEntry(id) | BmcRequest::Sdr(id) => {
                let [low, high] = id.to_le_bytes();
                vec![0x00, 0x00, low, high, WHOLE_RECORD[0], WHOLE_RECORD[1]]
            }
        }
    }

    fn message(self) -> Message {
        let (netfn, cmd) = self.command();
        Message::new_request(NetFn::from(netfn), cmd, self.data())
    }
}

/// The BMC as the health look asks it: one [`BmcRequest`], answered with the completion code,
/// then the response data. `Err` when it cannot be asked: no device (`NotFound`), a daemon that
/// may not open it (`PermissionDenied`), or no answer in time.
pub trait BmcPort: Send + Sync {
    /// The BMC's answer to `request`.
    ///
    /// # Errors
    /// The device's own, as above.
    fn ask(&self, request: BmcRequest) -> io::Result<Vec<u8>>;
}

/// The BMC behind the IPMI device at a path, opened afresh for each question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevIpmi {
    device: PathBuf,
}

impl DevIpmi {
    /// The BMC behind the IPMI device at `device`.
    pub fn new(device: &Path) -> DevIpmi {
        DevIpmi {
            device: device.to_path_buf(),
        }
    }

    /// The host's BMC, behind [`IPMI_DEVICE`].
    pub fn host() -> DevIpmi {
        DevIpmi::new(Path::new(IPMI_DEVICE))
    }
}

impl Ipmi for DevIpmi {
    /// The device is opened, asked and closed. `Err` when it cannot be opened (absent, not
    /// root), when it refuses the request (a file that is no IPMI device), or when no answer
    /// comes within [`ANSWER_TIMEOUT`].
    ///
    /// Ownership: `ipmi_rs::File` owns the device's descriptor for this one call and closes it
    /// when dropped; the answer is copied into the owned `Response`, so nothing of the device
    /// outlives the call.
    fn lan_parameter(&self, channel: u8, parameter: u8) -> io::Result<Vec<u8>> {
        let _one = DEVICE.lock().unwrap_or_else(PoisonError::into_inner);
        File::new(&self.device, ANSWER_TIMEOUT)
            .and_then(|mut device| ask(&mut device, channel, parameter))
    }
}

impl BmcPort for DevIpmi {
    /// The device is opened, asked and closed under the module's lock, as
    /// [`Ipmi::lan_parameter`] is; an answer waits at most [`ANSWER_TIMEOUT`].
    fn ask(&self, request: BmcRequest) -> io::Result<Vec<u8>> {
        let _one = DEVICE.lock().unwrap_or_else(PoisonError::into_inner);
        File::new(&self.device, ANSWER_TIMEOUT)
            .and_then(|mut device| exchange(&mut device, request))
    }
}

/// The answer of the BMC behind `connection` to `request`: the completion code, then the data.
fn exchange(connection: &mut Connection, request: BmcRequest) -> io::Result<Vec<u8>> {
    connection
        .send_recv(&mut Request::new(
            request.message(),
            RequestTargetAddress::Bmc(LogicalUnit::Zero),
        ))
        .map(answer)
}

/// A connection to a BMC whose failures are I/O errors: the device's `File`, or a test's double.
type Connection =
    dyn IpmiConnection<SendError = io::Error, RecvError = io::Error, Error = io::Error>;

/// The answer of the BMC behind `connection` to `Get LAN Configuration Parameters` for
/// `parameter` on `channel`: the completion code, then the response data.
fn ask(connection: &mut Connection, channel: u8, parameter: u8) -> io::Result<Vec<u8>> {
    connection
        .send_recv(&mut lan_request(channel, parameter))
        .map(answer)
}

/// `Get LAN Configuration Parameters` for `parameter` on `channel`, set and block selectors 0,
/// to the BMC itself.
fn lan_request(channel: u8, parameter: u8) -> Request {
    let message = Message::new_request(
        NetFn::Transport,
        GET_LAN_CONFIGURATION_PARAMETERS,
        vec![channel, parameter, 0, 0],
    );
    Request::new(message, RequestTargetAddress::Bmc(LogicalUnit::Zero))
}

/// The answer as the port gives it: the completion code, then the response data.
fn answer(response: Response) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + response.data().len());
    bytes.push(response.cc());
    bytes.extend_from_slice(response.data());
    bytes
}

#[cfg(test)]
#[path = "ipmi_tests.rs"]
mod tests;
