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
            .and_then(|mut device| ask(&mut device, channel, parameter))
    }
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
mod tests {
    use super::*;

    /// What a request asked: network function, command, data, and whether it went to the BMC
    /// itself on logical unit 0.
    type Asked = (u8, u8, Vec<u8>, bool);

    /// A BMC connection that keeps each request it is sent and answers with `reply`, or fails
    /// with `reply`'s error.
    struct Scripted {
        reply: Option<Vec<u8>>,
        asked: Vec<Asked>,
    }

    impl Scripted {
        fn answering(raw: &[u8]) -> Scripted {
            Scripted {
                reply: Some(raw.to_vec()),
                asked: Vec::new(),
            }
        }
    }

    impl IpmiConnection for Scripted {
        type SendError = io::Error;
        type RecvError = io::Error;
        type Error = io::Error;

        fn send(&mut self, request: &mut Request) -> io::Result<()> {
            let to_bmc = matches!(
                request.target(),
                RequestTargetAddress::Bmc(LogicalUnit::Zero)
            );
            self.asked.push((
                request.netfn_raw(),
                request.cmd(),
                request.data().to_vec(),
                to_bmc,
            ));
            Ok(())
        }

        fn recv(&mut self) -> io::Result<Response> {
            let raw = self
                .reply
                .clone()
                .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
            Response::new(Message::new_raw(0x0D, 0x02, raw), 0)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
        }

        fn send_recv(&mut self, request: &mut Request) -> io::Result<Response> {
            self.send(request)?;
            self.recv()
        }
    }

    /// Catches a reader whose exchange with the BMC never returns the BMC's answer (an adapter
    /// that always fails), sends another request than `Get LAN Configuration Parameters` for the
    /// channel and parameter asked, or returns the answer without its completion code: one
    /// request, Transport (0x0C) command 0x02 with `[channel, parameter, 0, 0]` to the BMC, and
    /// the answer returned byte for byte, completion code first.
    #[test]
    fn a_bmc_that_answers_gives_its_answer_byte_for_byte() {
        let raw = [0x00, 0x11, 0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x01];
        let mut bmc = Scripted::answering(&raw);
        assert_eq!(ask(&mut bmc, 2, 5).expect("an answer"), raw);
        assert_eq!(bmc.asked, [(0x0C, 0x02, vec![2, 5, 0, 0], true)]);
        assert_eq!(
            hostkeeper_enroll::parse_lan_mac(&ask(&mut bmc, 2, 5).expect("again")).as_deref(),
            Some("0c:c4:7a:00:00:01")
        );
        assert_eq!(bmc.asked.len(), 2, "each question is one request");
    }

    /// Catches a refusal turned into an error, or a BMC that answers nothing turned into an
    /// empty answer: a refusal is its completion code alone, which claims no MAC, and no answer
    /// is an error, after the request was sent.
    #[test]
    fn a_refusal_is_its_code_and_no_answer_is_an_error() {
        let mut refusing = Scripted::answering(&[0xc1]);
        let refused = ask(&mut refusing, 1, 5).expect("a refusal is an answer");
        assert_eq!(refused, [0xc1]);
        assert_eq!(hostkeeper_enroll::parse_lan_mac(&refused), None);

        let mut silent = Scripted {
            reply: None,
            asked: Vec::new(),
        };
        let error = ask(&mut silent, 1, 5).expect_err("no answer");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(silent.asked, [(0x0C, 0x02, vec![1, 5, 0, 0], true)]);

        let mut empty = Scripted::answering(&[]);
        let error = ask(&mut empty, 1, 5).expect_err("an empty answer");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

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
