//! The behaviour of `hostkeeper-pxe-packets`, through its public interface, in one test
//! binary so that every arm of a function is covered by the same binary.

mod arch;
mod build;
mod malformed;
mod messages;
mod proxy;
mod router;
mod tftp_request;
mod tftp_transfer;
