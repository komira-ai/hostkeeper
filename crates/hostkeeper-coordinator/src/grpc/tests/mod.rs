//! The tests of the gRPC services and the HTTP/JSON gateway of the coordinator, each driven in
//! process through `crate::grpc::route` with an explicit peer certificate and bearer: no socket.

mod adapters;
mod agent;
mod credentials;
mod ctl;
mod gateway;
mod heartbeat;
mod limits;
mod listeners;
mod operator;
mod pending;
mod refusal;
mod report;
mod support;
