use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_enroll::{LAN_IP_PARAMETER, LAN_MAC_PARAMETER};

use super::*;

/// A BMC on channel 1 whose answers a test sets, counting the questions it is asked; with no
/// answers it cannot be asked.
#[derive(Default)]
struct FakeIpmi {
    answers: Mutex<BTreeMap<u8, Vec<u8>>>,
    asked: AtomicU64,
}

impl FakeIpmi {
    /// The BMC at `192.0.2.<last>` with the MAC `0c:c4:7a:00:00:01`; none at all with `None`.
    fn at(&self, last: Option<u8>) {
        let mut answers = self.answers.lock().expect("answers");
        answers.clear();
        if let Some(last) = last {
            answers.insert(LAN_MAC_PARAMETER, vec![0, 0x11, 0x0c, 0xc4, 0x7a, 0, 0, 1]);
            answers.insert(LAN_IP_PARAMETER, vec![0, 0x11, 192, 0, 2, last]);
        }
    }

    fn asked(&self) -> u64 {
        self.asked.load(Ordering::SeqCst)
    }
}

impl Ipmi for FakeIpmi {
    fn lan_parameter(&self, channel: u8, parameter: u8) -> io::Result<Vec<u8>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let answers = self.answers.lock().expect("answers");
        (channel == 1)
            .then(|| answers.get(&parameter).cloned())
            .flatten()
            .ok_or_else(|| io::Error::other("no such channel"))
    }
}

/// A unix clock a test sets.
#[derive(Default)]
struct Clock(AtomicU64);

impl UnixClock for Clock {
    fn unix_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn report(last: u8) -> BmcReport {
    BmcReport {
        ip: format!("192.0.2.{last}"),
        mac: "0c:c4:7a:00:00:01".to_owned(),
    }
}

fn source() -> (Arc<FakeIpmi>, Arc<Clock>, HostBmc) {
    let ipmi = Arc::new(FakeIpmi::default());
    let clock = Arc::new(Clock::default());
    clock.0.store(1_000, Ordering::SeqCst);
    let bmc = HostBmc::new(ipmi.clone(), clock.clone());
    (ipmi, clock, bmc)
}

/// Catches a BMC read at every heartbeat (each question may hold the heartbeat a second), never
/// read again (an address its lease moved never reported), or read again only after more than
/// [`REREAD_SECS`]: the first heartbeat reads it, the next ones within ten minutes ask nothing,
/// and the first at ten minutes reads the new address.
#[test]
fn the_bmc_is_read_at_the_first_heartbeat_then_every_ten_minutes() {
    let (ipmi, clock, bmc) = source();
    ipmi.at(Some(12));
    assert_eq!(bmc.bmc(), Some(report(12)));
    assert_eq!(ipmi.asked(), 2, "the MAC, then the address");
    ipmi.at(Some(40));
    clock.0.store(1_000 + REREAD_SECS - 1, Ordering::SeqCst);
    assert_eq!(bmc.bmc(), Some(report(12)));
    assert_eq!(ipmi.asked(), 2, "not read again yet");
    clock.0.store(1_000 + REREAD_SECS, Ordering::SeqCst);
    assert_eq!(bmc.bmc(), Some(report(40)));
    assert_eq!(ipmi.asked(), 4);
    assert_eq!(REREAD_SECS, 600);
}

/// Catches a read that finds nothing reported as no BMC after one was found (a BMC busy for one
/// read reported gone), and a host that never had one reporting one.
#[test]
fn a_read_that_finds_nothing_keeps_the_report_before_it() {
    let (ipmi, clock, bmc) = source();
    assert_eq!(bmc.bmc(), None, "no BMC");
    assert_eq!(ipmi.asked(), 11, "every channel asked for a MAC");
    ipmi.at(Some(12));
    clock.0.store(1_000 + REREAD_SECS, Ordering::SeqCst);
    assert_eq!(bmc.bmc(), Some(report(12)));
    ipmi.at(None);
    clock.0.store(1_000 + 2 * REREAD_SECS, Ordering::SeqCst);
    assert_eq!(bmc.bmc(), Some(report(12)), "kept");
}

/// Catches a host clock set back (after a read at a later time) leaving the BMC unread until the
/// clock catches up again.
#[test]
fn a_clock_set_back_reads_the_bmc_again() {
    let (ipmi, clock, bmc) = source();
    ipmi.at(Some(12));
    assert_eq!(bmc.bmc(), Some(report(12)));
    ipmi.at(Some(40));
    clock.0.store(999, Ordering::SeqCst);
    assert_eq!(bmc.bmc(), Some(report(40)));
}

/// Catches a Mac given a BMC reader, and a Linux host (under systemd or OpenRC) given none. The
/// Linux reader is not asked: the test opens no device.
#[test]
fn only_a_linux_host_has_a_bmc_reader() {
    let clock: Arc<dyn UnixClock> = Arc::new(Clock::default());
    assert!(at_host(PlatformKind::Macos, clock.clone()).is_none());
    assert!(at_host(PlatformKind::Linux, clock.clone()).is_some());
    assert!(at_host(PlatformKind::Openrc, clock).is_some());
}
