//! Exhaustive model checks of the MDM parts of the coordinator that are state machines
//! (docs/design/coordinator-mdm.md section 10, task CM4), written before the code they judge.
//!
//! [`explore`] walks every reachable state of a [`Model`](explore::Model) breadth first and
//! returns the first broken invariant with the shortest steps that reach it. [`queue`] models
//! the per-Mac command queue; [`enrollment`] models `/enroll`, SCEP, `Authenticate`,
//! `TokenUpdate`, `CheckOut`, a serial enrolling again and the approval that follows. In each,
//! the rules the implementation must follow are fields of a `Rules` value, `Rules::design()` is
//! the design, and each invariant is a public constant that is also the name of the test that
//! holds it. A test per rule turns that rule off and expects the search to find the invariant it
//! protects, so a check that cannot fail does not pass.
//!
//! The crate is plain Rust over the standard library, and nothing depends on it.

pub mod enrollment;
pub mod explore;
pub mod queue;
