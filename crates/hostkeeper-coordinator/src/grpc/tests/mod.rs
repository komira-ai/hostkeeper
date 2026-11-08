//! The tests of the gRPC services and the HTTP/JSON gateway of the coordinator, each driven in
//! process through `crate::grpc::route` with an explicit peer certificate and bearer: no socket.

mod acks;
mod adapters;
mod agent;
mod alerts;
mod channel;
mod credentials;
mod enrollment;
mod force;
mod gateway;
mod heartbeat;
mod hosts;
mod install;
mod limits;
mod listeners;
mod mdm;
mod operator;
mod operator_desired;
mod pending;
mod readme_status;
mod refusal;
mod release_index;
mod report;
mod rollouts;
mod support;
mod targets;
mod trusted_root;
mod waits;
mod wires;
