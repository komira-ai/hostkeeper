//! The heartbeat loop of the hostkeeper daemon (docs/design/hostkeeper-design.md, sections 5.2,
//! 8.2 to 8.4 and 11).
//!
//! Every period the [`Heartbeater`] sends the coordinator the facts of every service installed
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
//! - [`UnixClock`], [`Log`] and [`Sleeper`].
//!
//! Its own types are in [`beat`] (what is sent, and the bound it keeps), [`order`] (the order,
//! the verdict and the reply) and [`names`] (the typed words of an order). The one file it
//! writes is the deferral of [`deferral`]. The fakes of every port are in `testing`, behind the
//! feature `testing`.

pub mod beat;
pub mod deferral;
pub mod heartbeater;
pub mod names;
pub mod order;
pub mod ports;
#[cfg(feature = "testing")]
pub mod testing;

pub use beat::{
    ANNOUNCED, Feature, Features, Fitted, Health, Heartbeat, Liveness, MAX_HEARTBEAT_BYTES,
    Observed, OptionalFact, Oversize, Platform, ServiceFacts, fit,
};
pub use deferral::{Deferral, DeferralError, Deferrals};
pub use heartbeater::{BeatOutcome, Heartbeater, LoopConfig, LoopFlags, Ports};
pub use names::{OrderId, PackageDigest, Reason, WordError};
pub use order::{DeferredReport, Order, OrderRef, OrderResult, Outcome, Reply, Target, Verb};
pub use ports::{
    CertSource, CertState, ClientIdentity, ClockSleeper, CoordinatorLink, Dispatch, HeldHalts, Job,
    LinkError, Log, Observation, OrderSink, RecordedService, ReplyFault, ServiceObserver,
    ServiceRecords, Sleeper, StopFlag, ThreadDispatch, Tick, UnixClock,
};
