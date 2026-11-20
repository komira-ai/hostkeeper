//! `hk` as it runs: `hostkeeper_hk::run` in process, against a `World` of fakes, from the
//! real protos' command table and from the fixture's. A call goes over TLS to a loopback server
//! of port 0 that records each request (method, path, headers, body) and answers a canned reply,
//! or to the real gateway behind it (`gateway`), or to a fake token endpoint of the runner.
//!
//! - `flags`: every kind of flag, a secret field, `--request`, and their usage errors;
//! - `answers`: `--response-file`, the exit code of every gateway code, `refused:` lines, an
//!   answer that is not the gateway's, and calls through the real gateway;
//! - `transport`: what the request is on the wire, whom it trusts, and no answer;
//! - `surface`: the hold of a `wait_secs` field, the help, the version, the names, the global
//!   flags' usage errors, and a new RPC of the fixture appearing with no edit;
//! - `secrets`: the token and a secret only in the request, and a token file others can read
//!   refused;
//! - `oidc`: `--github-oidc-audience` against a fake token endpoint;
//! - `older`: the probe of the coordinator's version after a refusal `invalid_request`;
//! - `ssh`: the ssh commands, which hand their flags and the path of the token file to the ssh
//!   module, and the methods of the service `ssh`, which stay gateway calls;
//! - `golden`: the help texts of the fixture's table against their files;
//! - `methods`: the requests of the methods of the real protos that the operator's tools use.
//! - `maintenance`: the `Maintenance` calls' requests, their enum flags' usage errors, and the
//!   `UNIMPLEMENTED` answer of every one through the real gateway.

mod answers;
mod flags;
mod gateway;
mod golden;
mod maintenance;
mod methods;
mod oidc;
mod older;
mod process;
mod rig;
mod secrets;
mod ssh;
mod surface;
mod transport;
