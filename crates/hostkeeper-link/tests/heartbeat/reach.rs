//! The coordinator's reach ([`Reach`]) under both links: where the coordinator's name is dialed
//! when it looks up, when its lookup fails after a first success or from the start, and when it
//! looks up to an address that does not answer as the coordinator; and the lookups the daemon
//! gives it ([`BackgroundLookup`], [`SystemLookup`]). The lookup is a fake, so nothing here asks a
//! resolver: the system's lookup is given addresses alone, which it reads without one.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_link::testing::{FakeClock, FakeLookup, LOOKUP_FAILED, RecordingLog};
use hostkeeper_link::{
    BackgroundLookup, CoordinatorLink, GrpcMtlsLink, JsonMtlsLink, LinkError, Lookup, Reach, Reply,
    SystemLookup, TrustFault,
};

use crate::grpc::{DuplexDial, Server, fresh, web_beat};
use crate::link::beat;
use crate::tls::{
    COORDINATOR, COORDINATOR_IP, Close, Farm, PipeTransport, Pki, config_text, http, serve,
    server_config,
};

/// The address the name looks up to once the coordinator has moved.
const MOVED_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));

/// The daemon config's `coordinator_ip` in the tests that set one.
const CONFIGURED_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

/// `ip` on the coordinator's port, as a line of the reach writes it.
fn at(ip: IpAddr) -> SocketAddr {
    SocketAddr::new(ip, 8443)
}

/// `ip` on the coordinator's port, as the transport is asked for it.
fn dialed(ip: IpAddr) -> (String, u16) {
    (ip.to_string(), 8443)
}

/// The words of a reach that dialed past the name because the lookup failed.
fn not_looked_up() -> String {
    format!("its name cannot be looked up: {LOOKUP_FAILED}")
}

/// The daemon config of the tests, with `extra` (whole lines) before it: the coordinator at
/// `https://coordinator.test:8443`, the CA `farm`'s.
fn config_with(farm: &Farm, extra: &str) -> DaemonConfig {
    let text = config_text(
        &format!("https://{COORDINATOR}:8443"),
        &farm.ca.sha256_hex(),
    );
    DaemonConfig::parse(&format!("{extra}{text}")).expect("a daemon config")
}

/// A JSON link whose reach looks up through `lookup` and logs to `log`, which is the reach's
/// alone: the link's own lines go elsewhere.
struct Bench {
    farm: Farm,
    _pki: Pki,
    transport: Arc<PipeTransport>,
    lookup: Arc<FakeLookup>,
    log: Arc<RecordingLog>,
    reach: Arc<Reach>,
    link: JsonMtlsLink,
}

impl Bench {
    fn new(name: &str, extra: &str, lookup: FakeLookup) -> Bench {
        Bench::remembering(name, extra, lookup, None)
    }

    /// The bench whose reach remembers `remembered` from the start, as one an earlier run kept.
    fn remembering(
        name: &str,
        extra: &str,
        lookup: FakeLookup,
        remembered: Option<IpAddr>,
    ) -> Bench {
        let farm = Farm::new();
        let pki = Pki::with_bundle(name, &farm.ca.pem);
        let config = config_with(&farm, extra);
        let lookup = Arc::new(lookup);
        let log = Arc::new(RecordingLog::default());
        let reach = Reach::new(&config, lookup.clone(), log.clone()).expect("a reach");
        let reach = Arc::new(match remembered {
            Some(ip) => reach.remembering(at(ip)),
            None => reach,
        });
        let transport = Arc::new(PipeTransport::default());
        let link = JsonMtlsLink::new(
            &config,
            &pki.root(),
            reach.clone(),
            transport.clone(),
            Arc::new(FakeClock::new(0, 0)),
            Arc::new(RecordingLog::default()),
        );
        Bench {
            farm,
            _pki: pki,
            transport,
            lookup,
            log,
            reach,
            link,
        }
    }

    /// A heartbeat that the next connection takes to the coordinator, which answers it; what the
    /// link gives back, and whether the coordinator read the heartbeat.
    fn to_coordinator(&self) -> (Result<Reply, LinkError>, bool) {
        let server = serve(&self.transport, self.farm.server(), Close::Notify, |_| {
            http(200, r#"{"reply":"nothing"}"#)
        });
        let reply = self.link.heartbeat(&self.farm.host.identity(), &beat());
        (reply, server.join().expect("the server").is_some())
    }

    /// A heartbeat whose first connection is taken by a server that is not the coordinator: its
    /// certificate is of the pinned CA, for another name. The connection after it, if the link
    /// makes one, is the coordinator's. What the link gives back, and whether the impostor and
    /// the coordinator read the heartbeat.
    fn past_an_impostor(&self) -> (Result<Reply, LinkError>, bool, bool) {
        let impostor = server_config(&self.farm.ca.server("other.test"), &self.farm.ca);
        let first = serve(&self.transport, impostor, Close::Notify, |_| {
            http(200, r#"{"reply":"nothing"}"#)
        });
        let second = serve(&self.transport, self.farm.server(), Close::Notify, |_| {
            http(200, r#"{"reply":"nothing"}"#)
        });
        let reply = self.link.heartbeat(&self.farm.host.identity(), &beat());
        let read_by_impostor = first.join().expect("the impostor").is_some();
        let read_by_coordinator = second.join().expect("the coordinator").is_some();
        (reply, read_by_impostor, read_by_coordinator)
    }

    /// A heartbeat with no server: every connection is refused, or none is made.
    fn to_nobody(&self) -> Result<Reply, LinkError> {
        self.link.heartbeat(&self.farm.host.identity(), &beat())
    }
}

/// Catches a daemon whose heartbeats stop when the coordinator's name stops looking up (a
/// resolver behind an uplink that is down), that logs at every heartbeat while it is so, or that
/// keeps dialing the remembered address once the name looks up again: after a first heartbeat
/// through the name, a lookup that fails sends each heartbeat to the address that heartbeat
/// reached, with one line that says so; a lookup that answers again is said once more, and the
/// first reach through the name, the state a daemon starts in, says nothing.
#[test]
fn heartbeats_go_to_the_last_address_reached_while_the_name_cannot_be_looked_up() {
    let bench = Bench::new(
        "reach-lookup-fails",
        "",
        FakeLookup::answering(&[COORDINATOR_IP]),
    );
    let (first, read) = bench.to_coordinator();
    assert!(first.is_ok() && read, "{first:?}");
    assert_eq!(bench.reach.remembered(), Some(at(COORDINATOR_IP)));
    assert_eq!(bench.log.lines(), Vec::<String>::new());

    bench.lookup.fail(io::ErrorKind::Other);
    for _ in 0..2 {
        assert_eq!(bench.to_coordinator(), (first.clone(), true));
    }
    bench.lookup.answer(&[COORDINATOR_IP]);
    assert_eq!(bench.to_coordinator(), (first.clone(), true));

    assert_eq!(bench.transport.dialed(), vec![dialed(COORDINATOR_IP); 4]);
    assert_eq!(
        bench.lookup.asked(),
        vec![(COORDINATOR.to_owned(), 8443); 4],
        "the name is looked up at every heartbeat"
    );
    assert_eq!(
        bench.log.lines(),
        [
            format!(
                "the coordinator {COORDINATOR} answers at {}, the last address that answered \
                 as it ({})",
                at(COORDINATOR_IP),
                not_looked_up()
            ),
            format!(
                "the coordinator {COORDINATOR} answers at {}",
                at(COORDINATOR_IP)
            ),
        ]
    );
}

/// Catches a daemon that cannot reach its coordinator at all while its name does not look up
/// from its start: the config's `coordinator_ip` is dialed on the URL's port, its certificate
/// still checked against the name (the coordinator's certificate is for the name, not the
/// address), and said once, though it is then also the address remembered. Without the key and
/// without an address remembered nothing is dialed, and the heartbeat fails with the lookup's
/// error; a name that looks up to no address fails as not found.
#[test]
fn a_name_that_cannot_be_looked_up_from_the_start_is_dialed_at_coordinator_ip() {
    let bench = Bench::new(
        "reach-configured",
        &format!("coordinator_ip = \"{CONFIGURED_IP}\"\n"),
        FakeLookup::failing(io::ErrorKind::Other),
    );
    let (first, read) = bench.to_coordinator();
    assert!(first.is_ok() && read, "{first:?}");
    assert_eq!(bench.to_coordinator(), (first, true));
    assert_eq!(bench.transport.dialed(), vec![dialed(CONFIGURED_IP); 2]);
    assert_eq!(bench.reach.remembered(), Some(at(CONFIGURED_IP)));
    assert_eq!(
        bench.log.lines(),
        [format!(
            "the coordinator {COORDINATOR} answers at {}, the daemon config's coordinator_ip \
             ({})",
            at(CONFIGURED_IP),
            not_looked_up()
        )]
    );

    let bare = Bench::new(
        "reach-unconfigured",
        "",
        FakeLookup::failing(io::ErrorKind::TimedOut),
    );
    assert_eq!(
        bare.to_nobody(),
        Err(LinkError::Transport(io::ErrorKind::TimedOut))
    );
    assert_eq!(bare.transport.dialed(), Vec::<(String, u16)>::new());
    assert_eq!(bare.reach.remembered(), None);
    assert_eq!(bare.log.lines(), Vec::<String>::new());

    let empty = Bench::new("reach-no-address", "", FakeLookup::answering(&[]));
    assert_eq!(
        empty.to_nobody(),
        Err(LinkError::Transport(io::ErrorKind::NotFound))
    );
    assert_eq!(empty.transport.dialed(), Vec::<(String, u16)>::new());
}

/// Catches an address taken from the lookup before it proved to be the coordinator (a record
/// that moved to a server that is not it): a new address of the name is dialed first, and when
/// its certificate is refused the heartbeat goes on to the address remembered, which stays
/// remembered, said once; the new address is remembered only once a handshake with it completes,
/// said once again. When no address answers as the coordinator the heartbeat fails with the last
/// one's error and the address remembered does not change.
#[test]
fn a_new_address_of_the_name_is_taken_only_once_its_handshake_completes() {
    let bench = Bench::new("reach-moved", "", FakeLookup::answering(&[COORDINATOR_IP]));
    let (first, _) = bench.to_coordinator();
    assert!(first.is_ok(), "{first:?}");

    bench.lookup.answer(&[MOVED_IP]);
    assert_eq!(bench.past_an_impostor(), (first.clone(), false, true));
    assert_eq!(bench.reach.remembered(), Some(at(COORDINATOR_IP)));
    assert_eq!(bench.to_coordinator(), (first.clone(), true));
    assert_eq!(bench.reach.remembered(), Some(at(MOVED_IP)));
    assert_eq!(
        bench.transport.dialed(),
        [
            dialed(COORDINATOR_IP),
            dialed(MOVED_IP),
            dialed(COORDINATOR_IP),
            dialed(MOVED_IP),
        ]
    );
    assert_eq!(
        bench.log.lines(),
        [
            format!(
                "the coordinator {COORDINATOR} answers at {}, the last address that answered \
                 as it (no address its name looks up to answered as it)",
                at(COORDINATOR_IP)
            ),
            format!("the coordinator {COORDINATOR} answers at {}", at(MOVED_IP)),
        ]
    );

    let lost = Bench::remembering(
        "reach-moved-nowhere",
        "",
        FakeLookup::answering(&[MOVED_IP]),
        Some(COORDINATOR_IP),
    );
    let impostor = server_config(&lost.farm.ca.server("other.test"), &lost.farm.ca);
    let servers = [0, 1].map(|_| {
        serve(&lost.transport, impostor.clone(), Close::Notify, |_| {
            http(200, r#"{"reply":"nothing"}"#)
        })
    });
    assert_eq!(
        lost.to_nobody(),
        Err(LinkError::Untrusted(TrustFault::ServerCertificate))
    );
    for server in servers {
        assert!(server.join().expect("the impostor").is_none());
    }
    assert_eq!(
        lost.transport.dialed(),
        [dialed(MOVED_IP), dialed(COORDINATOR_IP)]
    );
    assert_eq!(lost.reach.remembered(), Some(at(COORDINATOR_IP)));
    assert_eq!(lost.log.lines(), Vec::<String>::new());
}

/// Catches a reach that dials an address twice, in another order than the name's first, or that
/// follows a resolver's changing order from one address of the name to another: the addresses
/// the name looks up to are dialed in the lookup's order, but for the address remembered, which
/// goes first when it is one of them, then the address remembered, then `coordinator_ip`, each
/// once at its first place; a name whose two addresses come back in the other order keeps being
/// reached at the one it was, and says nothing; an address remembered from an earlier run is
/// dialed when the name cannot be looked up, and said as remembered.
#[test]
fn addresses_are_dialed_name_first_then_remembered_then_configured_each_once() {
    let two = [COORDINATOR_IP, MOVED_IP];
    let order = |name: &str, remembered: IpAddr, configured: IpAddr| {
        let bench = Bench::remembering(
            name,
            &format!("coordinator_ip = \"{configured}\"\n"),
            FakeLookup::answering(&two),
            Some(remembered),
        );
        assert_eq!(
            bench.to_nobody(),
            Err(LinkError::Transport(io::ErrorKind::ConnectionRefused))
        );
        bench.transport.dialed()
    };
    let remembered = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 9));
    assert_eq!(
        order("reach-order", remembered, CONFIGURED_IP),
        [
            dialed(COORDINATOR_IP),
            dialed(MOVED_IP),
            dialed(remembered),
            dialed(CONFIGURED_IP),
        ]
    );
    assert_eq!(
        order("reach-order-once", MOVED_IP, COORDINATOR_IP),
        [dialed(MOVED_IP), dialed(COORDINATOR_IP)]
    );

    let turning = Bench::new("reach-round-robin", "", FakeLookup::answering(&two));
    let (reply, read) = turning.to_coordinator();
    assert!(reply.is_ok() && read, "{reply:?}");
    turning.lookup.answer(&[MOVED_IP, COORDINATOR_IP]);
    assert_eq!(turning.to_coordinator(), (reply, true));
    assert_eq!(turning.transport.dialed(), vec![dialed(COORDINATOR_IP); 2]);
    assert_eq!(turning.reach.remembered(), Some(at(COORDINATOR_IP)));
    assert_eq!(turning.log.lines(), Vec::<String>::new());

    let earlier = Bench::remembering(
        "reach-earlier-run",
        "",
        FakeLookup::failing(io::ErrorKind::Other),
        Some(remembered),
    );
    let (reply, read) = earlier.to_coordinator();
    assert!(reply.is_ok() && read, "{reply:?}");
    assert_eq!(earlier.transport.dialed(), [dialed(remembered)]);
    assert_eq!(
        earlier.log.lines(),
        [format!(
            "the coordinator {COORDINATOR} answers at {}, the last address that answered as it \
             ({})",
            at(remembered),
            not_looked_up()
        )]
    );
}

/// Catches a gRPC heartbeat that stops with the lookup, or that keeps an address of its own the
/// JSON link does not see: over the coordinator's listener, the gRPC link reaches the address
/// remembered once the name cannot be looked up, says so once through the reach, and a JSON link
/// over the same reach dials that address too.
#[test]
fn the_grpc_link_reaches_the_last_address_while_the_name_cannot_be_looked_up() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("reach-grpc", &farm.ca.pem);
    let (_dir, coordinator) = fresh("reach-grpc-coordinator");
    let server = Server::of(&farm, &coordinator);
    let config = config_with(&farm, "");
    let lookup = Arc::new(FakeLookup::answering(&[COORDINATOR_IP]));
    let log = Arc::new(RecordingLog::default());
    let reach = Arc::new(Reach::new(&config, lookup.clone(), log.clone()).expect("a reach"));
    let dial = Arc::new(DuplexDial::default());
    let grpc = GrpcMtlsLink::new(
        &config,
        &pki.root(),
        reach.clone(),
        dial.clone(),
        Arc::new(RecordingLog::default()),
    )
    .expect("a link");
    let identity = farm.host.identity();

    server.accept(&dial);
    let first = grpc.heartbeat(&identity, &web_beat());
    assert!(first.is_ok(), "{first:?}");
    lookup.fail(io::ErrorKind::Other);
    server.accept(&dial);
    assert_eq!(grpc.heartbeat(&identity, &web_beat()), first);
    assert_eq!(dial.dialed(), vec![dialed(COORDINATOR_IP); 2]);
    assert_eq!(
        log.lines(),
        [format!(
            "the coordinator {COORDINATOR} answers at {}, the last address that answered as it \
             ({})",
            at(COORDINATOR_IP),
            not_looked_up()
        )]
    );

    let transport = Arc::new(PipeTransport::default());
    let json = JsonMtlsLink::new(
        &config,
        &pki.root(),
        reach,
        transport.clone(),
        Arc::new(FakeClock::new(0, 0)),
        Arc::new(RecordingLog::default()),
    );
    assert_eq!(
        json.heartbeat(&identity, &web_beat()),
        Err(LinkError::Transport(io::ErrorKind::ConnectionRefused))
    );
    assert_eq!(transport.dialed(), [dialed(COORDINATOR_IP)]);
}

/// Catches a gRPC link that stops at the first address whose certificate is refused: with the
/// name moved to an address whose server is not the coordinator, the heartbeat goes on to the
/// address remembered, which answers, and the reach says so once.
#[test]
fn the_grpc_link_goes_on_to_the_next_address_when_a_certificate_is_refused() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("reach-grpc-next", &farm.ca.pem);
    let (_dir, coordinator) = fresh("reach-grpc-next-coordinator");
    let impostor = Server::new(&farm.ca.server("other.test"), &farm.ca, &coordinator);
    let server = Server::of(&farm, &coordinator);
    let config = config_with(&farm, "");
    let lookup = Arc::new(FakeLookup::answering(&[MOVED_IP]));
    let log = Arc::new(RecordingLog::default());
    let reach = Reach::new(&config, lookup, log.clone()).expect("a reach");
    let dial = Arc::new(DuplexDial::default());
    let grpc = GrpcMtlsLink::new(
        &config,
        &pki.root(),
        Arc::new(reach.remembering(at(COORDINATOR_IP))),
        dial.clone(),
        Arc::new(RecordingLog::default()),
    )
    .expect("a link");

    impostor.accept(&dial);
    server.accept(&dial);
    let reply = grpc.heartbeat(&farm.host.identity(), &web_beat());
    assert!(reply.is_ok(), "{reply:?}");
    assert_eq!(dial.dialed(), [dialed(MOVED_IP), dialed(COORDINATOR_IP)]);
    assert_eq!(
        log.lines(),
        [format!(
            "the coordinator {COORDINATOR} answers at {}, the last address that answered as it \
             (no address its name looks up to answered as it)",
            at(COORDINATOR_IP)
        )]
    );
}

/// A lookup that tells the test each name it is asked for, then waits until the test lets it
/// answer (by a message, or by dropping the sender, which fails it).
struct HeldLookup {
    asked: Mutex<Sender<String>>,
    release: Mutex<Receiver<()>>,
}

impl Lookup for HeldLookup {
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let _ = self.asked.lock().expect("the asked").send(host.to_owned());
        match self.release.lock().expect("the release").recv() {
            Ok(()) => Ok(vec![SocketAddr::new(COORDINATOR_IP, port)]),
            Err(_) => Err(io::Error::from(io::ErrorKind::Interrupted)),
        }
    }
}

/// Catches a lookup that holds the heartbeat for as long as the resolver does, or that starts a
/// new lookup at every heartbeat while one hangs: a lookup still running at the bound fails the
/// call as timed out, with words that say so, and the next call for the same name waits for that
/// lookup again instead of starting another; a call for another name starts its own.
#[test]
fn a_lookup_still_running_at_its_bound_fails_the_call_and_is_not_started_again() {
    let (asked_send, asked) = mpsc::channel();
    let (release, release_receive) = mpsc::channel();
    let inner = Arc::new(HeldLookup {
        asked: Mutex::new(asked_send),
        release: Mutex::new(release_receive),
    });
    let background = BackgroundLookup::new(inner, Duration::ZERO);
    let still_running = |host: &str| {
        let error = background.lookup(host, 8443).expect_err("still running");
        assert_eq!(
            (error.kind(), error.to_string()),
            (
                io::ErrorKind::TimedOut,
                "the lookup is still running".to_owned()
            ),
            "{host}"
        );
    };
    still_running(COORDINATOR);
    assert_eq!(asked.recv().expect("the first lookup"), COORDINATOR);
    still_running(COORDINATOR);
    still_running("other.test");
    // The second call for the coordinator's name started no lookup: the next one to report is
    // the other name's.
    assert_eq!(asked.recv().expect("the other lookup"), "other.test");
    drop(release);
}

/// A lookup that answers what it is given at once, on the caller's thread.
struct Answering(io::Result<Vec<SocketAddr>>);

impl Lookup for Answering {
    fn lookup(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        match &self.0 {
            Ok(addresses) => Ok(addresses.clone()),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }
}

/// A lookup whose thread ends with no answer.
struct Broken;

impl Lookup for Broken {
    fn lookup(&self, _host: &str, _port: u16) -> io::Result<Vec<SocketAddr>> {
        panic!("a lookup that ends with no answer")
    }
}

/// Catches a background lookup that changes what its lookup answered, or that waits forever for
/// a thread that ended with no answer: an answer within the bound is the lookup's, addresses or
/// error with its kind and words; a lookup that ends with no answer fails the call by saying so.
#[test]
fn a_background_lookup_gives_what_its_lookup_answered_within_its_bound() {
    let hour = Duration::from_secs(3600);
    let addresses = vec![at(COORDINATOR_IP), at(MOVED_IP)];
    let answering = BackgroundLookup::new(Arc::new(Answering(Ok(addresses.clone()))), hour);
    assert_eq!(
        answering.lookup(COORDINATOR, 8443).expect("the addresses"),
        addresses
    );
    let failed = io::Error::new(io::ErrorKind::NotFound, "no such name");
    let failing = BackgroundLookup::new(Arc::new(Answering(Err(failed))), hour);
    let error = failing.lookup(COORDINATOR, 8443).expect_err("the failure");
    assert_eq!(
        (error.kind(), error.to_string()),
        (io::ErrorKind::NotFound, "no such name".to_owned())
    );
    let broken = BackgroundLookup::new(Arc::new(Broken), hour);
    let error = broken.lookup(COORDINATOR, 8443).expect_err("no answer");
    assert_eq!(
        (error.kind(), error.to_string()),
        (
            io::ErrorKind::Other,
            "the lookup ended with no answer".to_owned()
        )
    );
}

/// Catches a system lookup that drops the port or an address family: an address, IPv4 or IPv6,
/// is its own one address on the port asked (read without a resolver).
#[test]
fn the_system_lookup_reads_an_address_as_itself() {
    assert_eq!(
        SystemLookup.lookup("127.0.0.1", 9).expect("an address"),
        [SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9)]
    );
    assert_eq!(
        SystemLookup.lookup("::1", 9).expect("an address"),
        [SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9)]
    );
}
