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
//! - [`HeldHalts`]: the halts the host holds, and the generation each is held under;
//! - [`RenewalSource`]: what the enrollment loop last said of the host's certificate;
//! - [`TrustedRootSource`]: the daemon's report of the Sigstore trusted root it checks packages
//!   with;
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
//! [`CoordinatorGet`] ([`fetch`]) reads a path of the coordinator's agent listener over the same
//! mutual TLS and streams the body to the caller's sink, through the same [`Transport`]: the
//! daemon's mirror of a private repository reads its files with it. `get_from` reads a file from
//! a given byte on (a 206), to resume a download.
//!
//! The coordinator's repository sets ([`PushedSet`]) go to the daemon's [`RepositorySink`], which
//! the loop calls only while the coordinator advertises [`Feature::RepositoriesV1`]. Its host
//! facts ([`PushedFacts`]) go to the daemon's [`FactsSink`], which writes the host's facts file,
//! only while the coordinator advertises [`Feature::FactsV1`], and before the order of the same
//! reply. Its operator keys ([`PushedKeys`]) go to the daemon's [`OperatorKeysSink`], which writes
//! the managed block of the login account's `authorized_keys`, only while the coordinator
//! advertises [`Feature::OperatorKeysV1`]. Its host prerequisites ([`PushedPrerequisites`]) go to
//! the daemon's [`PrerequisitesSink`], which makes sure of them on the host, only while the
//! coordinator advertises [`Feature::PrerequisitesV1`].
//!
//! [`GrpcMtlsLink`] ([`grpc`]) is the same call over gRPC (`Coordinator.Heartbeat` of
//! `hostkeeper.agent.v1`) and HTTP/2 on the same mutual TLS, reaching the coordinator through a
//! [`GrpcDial`], with its conversions in the private module `proto_map`. [`NegotiatedLink`]
//! ([`negotiated`]) holds one of each and speaks gRPC only while the coordinator's last reply
//! advertised [`Feature::GrpcV1`].

pub mod beat;
pub mod bmc;
pub mod deferral;
pub mod dial;
pub mod fetch;
pub mod full_disk_access;
pub mod grpc;
pub mod heartbeater;
pub mod host_facts;
pub mod host_ssh;
mod maintenance;
pub mod mtls;
pub mod names;
pub mod negotiated;
pub mod operator_keys;
pub mod order;
pub mod pki;
pub mod ports;
pub mod prerequisites;
pub mod probe;
mod proto_map;
pub mod repositories;
pub mod serial_console;
#[cfg(feature = "testing")]
pub mod testing;
mod wire_map;

pub use beat::{
    ANNOUNCED, FACTS, Feature, Features, Fitted, Health, Heartbeat, Liveness, LocalHalt,
    MAINTENANCE, MAX_HEARTBEAT_BYTES, Observed, OptionalFact, Oversize, PRIVATE_REPOSITORY,
    Platform, REPOSITORIES, Renewal, RootReason, SSH, ServiceFacts, TrustedRootFacts, fit,
};
pub use bmc::BmcReport;
pub use deferral::{Deferral, DeferralError, Deferrals};
pub use dial::{CALL_SECS, TcpDial, TcpTransport};
pub use fetch::{CoordinatorGet, GetError, Got, Limit};
pub use full_disk_access::{FullDiskAccess, FullDiskAccessSource};
pub use grpc::GRPC_REPLY_MAX;
pub use grpc::{AsyncConn, GrpcDial, GrpcLinkError, GrpcMtlsLink};
pub use heartbeater::{
    ACKED_KEPT, BeatOutcome, DEFAULT_HEARTBEAT_SECS, Heartbeater, LoopConfig, LoopFlags, Ports,
};
pub use host_facts::PushedFacts;
pub use host_ssh::{PASSWORD_KEEP, PASSWORD_OFF, PushedSsh, SshReport};
pub use mtls::{Conn, EndpointError, HEAD_MAX, JsonMtlsLink, REPLY_MAX, Transport};
pub use names::{OrderId, PackageDigest, Reason, WordError};
pub use negotiated::NegotiatedLink;
pub use operator_keys::{KeysRefusal, KeysReport, MAX_KEYS_REASON, PushedKeys};
pub use order::{
    Acknowledged, DeferredReport, Entry, MaintainOrder, Order, OrderRef, OrderResult, Outcome,
    Reply, Target, Verb,
};
pub use pki::PkiCertSource;
pub use ports::{
    BmcSource, CertSource, CertState, ClientIdentity, ClockSleeper, CoordinatorLink, Dispatch,
    Drift, FactsSink, HeldHalt, HeldHalts, Job, LinkError, Log, MaintainSink, MaintenanceSource,
    Observation, OperatorKeysSink, OrderSink, PrerequisitesSink, RecordedService,
    RemoteLoginProbeSource, RenewalSource, ReplyFault, RepositorySink, SerialConsoleSource,
    ServiceObserver, ServiceRecords, Sleeper, SshSink, StopFlag, ThreadDispatch, Tick, TrustFault,
    TrustedRootSource, UnixClock, Unsendable,
};
pub use prerequisites::{
    MAX_PREREQUISITE_REASON, MAX_PREREQUISITE_VERSION, PrerequisiteState, PrerequisiteStatus,
    PrerequisitesRefusal, PrerequisitesReport, PushedPrerequisites,
};
pub use probe::{ProbeStep, RemoteLoginProbe};
pub use repositories::{MAX_REFUSAL_REASON, PushedSet, RepositoriesReport, RepositoryRefusal};
pub use serial_console::SerialConsoleReport;
