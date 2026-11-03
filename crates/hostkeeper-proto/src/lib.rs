//! hostkeeper's protobuf services: the messages, service traits and clients `build.rs` generates
//! from the files under `proto/hostkeeper`, and the descriptor set they were generated from.
//!
//! Every hostkeeper API is one of these services, and the command line is generated from them.
//! Each `.proto` file opens with a header that says who serves it, how a caller is
//! authenticated, how refusals are expressed, its units and how it may be extended:
//!
//! - `hostkeeper.agent.v1`: `Coordinator`, the heartbeat an agent sends the coordinator.
//! - `hostkeeper.operator.v1`: `Hosts` and `Releases`, what an operator and CI ask of the
//!   coordinator.
//! - `hostkeeper.local.v1`: `Root`, the agent's channel to the host's root part, and `Admin`,
//!   what only root on the host may ask of it.
//!
//! Modules mirror the proto packages, because generated code names other packages by relative
//! paths (`super::super::...`).
//!
//! The protos change additively only. `proto/compat/baseline.binpb` is the descriptor set of
//! the last release, and a test of this crate fails when the current descriptors remove,
//! renumber, rename or retype anything in it (`compat`).

/// `hostkeeper.*`: hostkeeper's own packages.
#[allow(
    clippy::all,
    reason = "generated code follows the protos' shapes and comments, not clippy's style"
)]
pub mod hostkeeper {
    pub mod agent {
        /// The heartbeat a host's agent sends the coordinator, and the reply.
        pub mod v1 {
            tonic::include_proto!("hostkeeper.agent.v1");
        }
    }
    pub mod operator {
        /// What an operator and CI ask of the coordinator: `Hosts` and `Releases`.
        pub mod v1 {
            tonic::include_proto!("hostkeeper.operator.v1");
        }
    }
    pub mod local {
        /// The host-local services of the root part: `Root` (the agent's channel) and `Admin`
        /// (root's).
        pub mod v1 {
            tonic::include_proto!("hostkeeper.local.v1");
        }
    }
}

/// The `FileDescriptorSet` every file under `proto/hostkeeper` compiles to, with the files they
/// import and their comments, encoded. Custom options (`(hostkeeper.secret)`) are kept in it.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/hostkeeper.binpb"));

#[cfg(test)]
mod compat;
