//! The heartbeat loop of the hostkeeper daemon (docs/design/hostkeeper-design.md, sections 5.2,
//! 8.2 to 8.4 and 11).
//!
//! Every period, and again as soon as a running order ends, the [`Heartbeater`] sends the coordinator the facts of every service installed
//! on the host, announces its features, hands the order of the reply to the executor, and puts
//! the verdict of an order in every following heartbeat until the coordinator acknowledges it by
//! order id and try. It is a function of ports only, each a trait of [`ports`]:
//!
//! - [`CoordinatorLink`]: the one outbound call, [`CoordinatorLink::heartbeat`];
//! - [`CertSource`]: the host's client certificate and key, or "not enrolled";
//! - [`ServiceRecords`] and [`ServiceObserver`]: what is installed, at which version, with which
//!   last verdict, and how each service is now;
//! - [`OrderSink`] and [`Dispatch`]: the executor, run on a worker the dispatcher starts, so
//!   the loop keeps beating while a step runs;
//! - [`HeldHalts`]: the persisted halt generations;
//! - [`RenewalSource`]: what the enrollment loop last said of the host's certificate;
//! - [`UnixClock`], [`Log`] and [`Sleeper`].
//!
//! Its own types are in [`beat`] (what is sent, and the bound it keeps), [`order`] (the order,
//! the verdict and the reply) and [`names`] (the typed words of an order). The one file it
//! writes is the deferral of [`deferral`]. [`PkiCertSource`] is the file-based [`CertSource`]:
//! the pair under the `current` link of the pki directory, read at every heartbeat. The fakes of
//! every port are in `testing`, behind the feature `testing`.
//!
//! [`JsonMtlsLink`] ([`mtls`]) is the [`CoordinatorLink`] the daemon gives the loop: the frozen
//! JSON heartbeat route of `hostkeeper-wire` over rustls mutual TLS, with the conversions between
//! the loop's types and the wire's in one private module (`wire_map`). It reaches the
//! coordinator through a [`Transport`]; [`TcpTransport`] ([`dial`]) is the production one and
//! the only code of the crate that opens a socket, with [`TcpDial`], its async counterpart.
//!
//! [`GrpcMtlsLink`] ([`grpc`]) is the same call over gRPC (`Coordinator.Heartbeat` of
//! `hostkeeper.agent.v1`) and HTTP/2 on the same mutual TLS, reaching the coordinator through a
//! [`GrpcDial`], with its conversions in the private module `proto_map`. [`NegotiatedLink`]
//! ([`negotiated`]) holds one of each and speaks gRPC only while the coordinator's last reply
//! advertised [`Feature::GrpcV1`].

pub mod beat;
pub mod deferral;
pub mod dial;
pub mod grpc;
pub mod heartbeater;
pub mod mtls;
pub mod names;
pub mod negotiated;
pub mod order;
pub mod pki;
pub mod ports;
mod proto_map;
#[cfg(feature = "testing")]
pub mod testing;
mod wire_map;

pub use beat::{
    ANNOUNCED, Feature, Features, Fitted, Health, Heartbeat, Liveness, MAX_HEARTBEAT_BYTES,
    Observed, OptionalFact, Oversize, Platform, Renewal, ServiceFacts, fit,
};
pub use deferral::{Deferral, DeferralError, Deferrals};
pub use dial::{CALL_SECS, TcpDial, TcpTransport};
pub use grpc::{AsyncConn, GrpcDial, GrpcLinkError, GrpcMtlsLink};
pub use heartbeater::{
    ACKED_KEPT, BeatOutcome, DEFAULT_HEARTBEAT_SECS, Heartbeater, LoopConfig, LoopFlags, Ports,
};
pub use mtls::{Conn, EndpointError, HEAD_MAX, JsonMtlsLink, REPLY_MAX, Transport};
pub use names::{OrderId, PackageDigest, Reason, WordError};
pub use negotiated::NegotiatedLink;
pub use order::{
    Acknowledged, DeferredReport, Order, OrderRef, OrderResult, Outcome, Reply, Target, Verb,
};
pub use pki::PkiCertSource;
pub use ports::{
    CertSource, CertState, ClientIdentity, ClockSleeper, CoordinatorLink, Dispatch, HeldHalts, Job,
    LinkError, Log, Observation, OrderSink, RecordedService, RenewalSource, ReplyFault,
    ServiceObserver, ServiceRecords, Sleeper, StopFlag, ThreadDispatch, Tick, TrustFault,
    UnixClock, Unsendable,
};
