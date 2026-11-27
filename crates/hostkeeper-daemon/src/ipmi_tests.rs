//! The tests of `ipmi`: the claims' request and the health look's table, each sent to a scripted
//! connection, never to a device; and a device path that names nothing in a scratch directory.

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
    let response = Response::new(Message::new_raw(0x0D, 0x02, raw.clone()), 0).expect("a response");
    assert_eq!(answer(response), raw);
    let refused = Response::new(Message::new_raw(0x0D, 0x02, vec![0xc1]), 0).expect("a code");
    assert_eq!(answer(refused), [0xc1]);
}

/// Every request of the table, one of each kind.
fn every_request() -> [BmcRequest; 5] {
    [
        BmcRequest::DeviceId,
        BmcRequest::SelInfo,
        BmcRequest::SelEntry(0x01a4),
        BmcRequest::SdrInfo,
        BmcRequest::Sdr(0x0102),
    ]
}

/// Catches a request of the health look's table encoded as another command than the one it
/// names, with its record id in the wrong byte order, or with a reservation, an offset or a
/// length other than 0, 0 and the whole record; and catches the table growing a request
/// that reserves, clears, deletes, adds or sets: the match below names every kind, so a new
/// one does not build until it is judged here, and none of the commands that change the
/// BMC's logs is any request's.
#[test]
fn the_health_look_sends_only_reads() {
    for request in every_request() {
        let expected: (u8, u8, Vec<u8>) = match request {
            BmcRequest::DeviceId => (0x06, 0x01, vec![]),
            BmcRequest::SelInfo => (0x0A, 0x40, vec![]),
            BmcRequest::SelEntry(_) => (0x0A, 0x43, vec![0, 0, 0xa4, 0x01, 0, 0xff]),
            BmcRequest::SdrInfo => (0x0A, 0x20, vec![]),
            BmcRequest::Sdr(_) => (0x0A, 0x23, vec![0, 0, 0x02, 0x01, 0, 0xff]),
        };
        let (netfn, cmd) = request.command();
        assert_eq!((netfn, cmd, request.data()), expected, "{request:?}");
        // Reserve, Add, Partial Add, Delete, Clear and Set Time of the SEL; Reserve,
        // Add, Partial Add, Delete and Clear of the SDR repository; any App command but
        // Get Device ID.
        let writes = [
            0x42, 0x44, 0x45, 0x46, 0x47, 0x49, 0x22, 0x24, 0x25, 0x26, 0x27,
        ];
        assert!(
            (netfn == NETFN_APP && cmd == 0x01)
                || (netfn == NETFN_STORAGE && !writes.contains(&cmd)),
            "{request:?} is not a read"
        );
    }
}

/// Catches a request sent to another target than the BMC on logical unit 0, or with its
/// network function, command or data changed on the way, and the answer not returned
/// completion code first.
#[test]
fn a_request_of_the_table_is_sent_as_encoded_and_answered_byte_for_byte() {
    for request in every_request() {
        let mut bmc = Scripted::answering(&[0x00, 0x51, 0x01]);
        assert_eq!(
            exchange(&mut bmc, request).expect("an answer"),
            [0x00, 0x51, 0x01]
        );
        let (netfn, cmd) = request.command();
        assert_eq!(bmc.asked, [(netfn, cmd, request.data(), true)]);
    }
    let mut silent = Scripted {
        reply: None,
        asked: Vec::new(),
    };
    let error = exchange(&mut silent, BmcRequest::DeviceId).expect_err("no answer");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

/// Catches a device that is not there read as a BMC that does not answer: both questions of
/// a reader whose device path names nothing fail `NotFound`, which the health look reads as
/// "no BMC device".
#[test]
fn no_device_is_not_found() {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    let scratch = hostkeeper_testkit::Scratch::under(&target.join("tmp"), "ipmi-absent")
        .expect("a scratch directory");
    let device = DevIpmi::new(&scratch.path().join("ipmi0"));
    let asked = BmcPort::ask(&device, BmcRequest::DeviceId).expect_err("no device");
    assert_eq!(asked.kind(), io::ErrorKind::NotFound);
    let lan = device.lan_parameter(1, 5).expect_err("no device");
    assert_eq!(lan.kind(), io::ErrorKind::NotFound);
}
