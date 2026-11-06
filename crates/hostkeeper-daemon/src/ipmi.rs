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

use std::io;
use std::path::{Path, PathBuf};
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
        File::new(&self.device, ANSWER_TIMEOUT)
            .and_then(|mut device| device.send_recv(&mut lan_request(channel, parameter)))
            .map(answer)
    }
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
mod tests {
    use super::*;

    /// Catches the request sent to another network function or command than Transport's
    /// `Get LAN Configuration Parameters`, to another target than the BMC, or with the channel
    /// and the parameter swapped or a selector other than 0.
    #[test]
    fn the_request_is_get_lan_configuration_parameters_to_the_bmc() {
        let request = lan_request(3, 5);
        assert_eq!(request.netfn_raw(), 0x0C);
        assert_eq!(request.cmd(), 0x02);
        assert_eq!(request.data(), [3, 5, 0, 0]);
        assert!(matches!(
            request.target(),
            RequestTargetAddress::Bmc(LogicalUnit::Zero)
        ));
    }

    /// Catches the completion code dropped from the answer, or the data cut or reordered: the
    /// port's answer is the completion code first, then every byte of the data.
    #[test]
    fn the_answer_is_the_completion_code_then_the_data() {
        let raw = vec![0x00, 0x11, 0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x01];
        let response =
            Response::new(Message::new_raw(0x0D, 0x02, raw.clone()), 0).expect("a response");
        assert_eq!(answer(response), raw);
        let refused = Response::new(Message::new_raw(0x0D, 0x02, vec![0xc1]), 0).expect("a code");
        assert_eq!(answer(refused), [0xc1]);
    }
}
