//! hostkeeper's protobuf services: the messages, service traits and clients `build.rs` generates
//! from the files under `proto/hostkeeper`, and the descriptor set they were generated from.
//!
//! Every hostkeeper API is one of these services, and the command line is generated from them.
//! Each `.proto` file opens with a header that says who serves it, how a caller is
//! authenticated, how refusals are expressed, its units and how it may be extended:
//!
//! - `hostkeeper.agent.v1`: `Coordinator`, the heartbeat an agent sends the coordinator, and
//!   how a host gets and keeps its certificate (`Enroll`, `Poll`, `Renew`).
//! - `hostkeeper.operator.v1`: `Hosts` and `Releases`, what an operator and CI ask of the
//!   coordinator.
//! - `hostkeeper.operator.v1` also holds `Services`: which hosts run each service, and the
//!   operator's holds; `Enrollment`, the hosts waiting for an operator's approval; `Audit`,
//!   the log of every change; `Alerts`, the conditions that need a person; `Rollouts`, the
//!   update plans; and `Mdm`, the devices the MDM policy gate knows.
//! - `hostkeeper.local.v1`: `Admin`, what root on the host may ask of its daemon with the
//!   coordinator down (status, abort, clear a corrupt step record).
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
        /// The heartbeat a host's agent sends the coordinator, and the reply; enrollment and
        /// renewal.
        pub mod v1 {
            tonic::include_proto!("hostkeeper.agent.v1");
        }
    }
    pub mod operator {
        /// What an operator and CI ask of the coordinator: `Hosts`, `Releases`, `Services`,
        /// `Enrollment`, `Audit`, `Alerts`, `Rollouts` and `Mdm`.
        pub mod v1 {
            tonic::include_proto!("hostkeeper.operator.v1");
        }
    }
    pub mod local {
        /// The host-local service of the daemon: `Admin`, root's socket.
        pub mod v1 {
            tonic::include_proto!("hostkeeper.local.v1");
        }
    }
}

/// The most services one heartbeat lists, as the header of `hostkeeper/agent/v1/agent.proto`
/// says ("A heartbeat lists at most 16 services.").
pub const MAX_SERVICES_PER_HEARTBEAT: usize = 16;

/// The `FileDescriptorSet` every file under `proto/hostkeeper` compiles to, with the files they
/// import and their comments, encoded. Custom options (`(hostkeeper.secret)`) are kept in it.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/hostkeeper.binpb"));

/// The command table `hkctl` and the UI client are generated from, itself generated from the descriptors.
pub mod ctl;

#[cfg(test)]
mod compat;
#[cfg(test)]
mod ctl_gen;
