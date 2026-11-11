//! `hkctl` as it runs: the script `hostkeeper_hkctl::script` emits, from the real protos and from
//! the fixture's, run by the host's `bash` with a fake `curl` and `find` first on PATH
//! (tests/bin/fake_curl.rs). The fake records its arguments, its environment and what it reads
//! from each `@` argument, then forwards the request to the real gateway on a loopback listener
//! of port 0 (`gateway`), to a fake token endpoint of the runner, or answers a canned reply.
//!
//! - `flags`: every kind of flag, a secret field, `--request`, and their usage errors;
//! - `answers`: `--response-file`, the exit code of every gateway code, `refused:` lines, an
//!   answer that is not the gateway's, and calls through the real gateway;
//! - `host_groups`: the `HostGroups` calls, `--host-group`, and an `UNIMPLEMENTED` answer;
//! - `secrets`: the token never in an argument list, a child's environment or stderr, and a
//!   token file others can read refused;
//! - `mdm`: the MDM setup commands, their bodies, the `session_required` refusal of a write by bearer and the reads;
//! - `oidc`: `--github-oidc-audience` against a fake token endpoint;
//! - `older`: the probe of the coordinator's version after a refusal `invalid_request`;
//! - `surface`: the curl arguments, the hold of a `wait_secs` field, the help, the version, the
//!   global flags' usage errors, and a new RPC of the fixture appearing with no edit.

mod accounts;
mod answers;
mod flags;
mod gateway;
mod host_groups;
mod mdm;
mod oidc;
mod older;
mod rig;
mod secrets;
mod services_repository;
mod surface;
