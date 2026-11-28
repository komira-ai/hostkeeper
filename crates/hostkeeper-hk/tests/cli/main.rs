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
//! - `methods`: the requests of the methods of the real protos that the operator's tools use;
//! - `hardware`: `hk hardware log`'s request, its enum flags' usage errors, and its answer
//!   through the real gateway of a coordinator whose log holds no record;
//! - `maintenance`: the `Maintenance` calls' requests, their enum flags' usage errors, and the
//!   answer of every one through the real gateway of an empty coordinator;
//! - `xcode`: the commands of the service `xcode`: their paths, bodies, answers and a refusal;
//! - `xcode_push`: `hk xcode-push`, the second name of `hk xcode add`, on stand-ins for the Mac's
//!   programs and a fake Xcode store;
//! - `add_rig`: what the next three share, stand-ins of the Mac's programs and a fake store that
//!   answers `Xcode.List`;
//! - `xcode_add_find`: which archive `hk xcode add` takes, and the lock;
//! - `xcode_add`: the run: store first, the record, progress, the refusals of the store, the token;
//! - `xcode_add_resume`: the upload that resumes, against a fake store that answers the query and
//!   takes `Content-Range` parts, with the waits recorded by the world's wait;
//! - `xcode_add_checks`: the checks before the upload, one by one;
//! - `xcode_add_summary`: the Macs summary that ends it, from a fake gateway's `Hosts.List`,
//!   `HostGroups.List`, `Maintenance.List`, `Maintenance.Get` and `Maintenance.Schedule`;
//! - `summary_rig`: the fake gateway of the summary's tests, whose answers are the real messages
//!   written as the coordinator's gateway writes them;
//! - `verify_rig`: what the next two share, the stand-ins of the Mac's programs for `hk xcode verify`, a
//!   clock of the tests' own and a root certificate of their own making;
//! - `xcode_verify`: `hk xcode verify`'s record, arguments, refusals and help;
//! - `xcode_verify_checks`: its verdict, check by check;
//! - `tailnet_wait`: `hk tailnet-wait`, its probe of a loopback server, its recorded waits and its
//!   rejoin through a stand-in `tailscale`;
//! - `farm_rig`: what the next five share, a fake coordinator that answers the maintenance run, the
//!   host's probe and the removal of the check on a farm Mac from a script, whose answers are the
//!   real messages written as the gateway writes them, and a wait that records what it is asked;
//! - `xcode_farm_choose`: off macOS, which Mac checks (`--mac`, `xcode_mac`, their usage errors, and
//!   `--mac` on macOS) and the release when no program can read the archive;
//! - `xcode_farm_add`: `hk xcode add` off macOS: the run on the farm Mac, each end of its turn, the
//!   record and the refusals;
//! - `xcode_farm_answers`: the answers the check takes as they come: a reason that is no token, an
//!   answer that is no JSON, a number written as a number, and a call with no answer;
//! - `xcode_farm_verify`: `hk xcode verify` off macOS: the archive held by the store, checked on the
//!   farm Mac, and the record printed.
//! - `xcode_farm_record`: a record of the farm Mac's check that cannot be kept: `hk xcode add` still
//!   refuses an archive that failed, `hk xcode verify` exits 1.

mod add_rig;
mod answers;
mod farm_rig;
mod flags;
mod gateway;
mod golden;
mod hardware;
mod maintenance;
mod methods;
mod oidc;
mod older;
mod process;
mod rig;
mod secrets;
mod settings;
mod ssh;
mod summary_rig;
mod surface;
mod tailnet_wait;
mod transport;
mod verify_rig;
mod xcode;
mod xcode_add;
mod xcode_add_checks;
mod xcode_add_find;
mod xcode_add_resume;
mod xcode_add_summary;
mod xcode_farm_add;
mod xcode_farm_answers;
mod xcode_farm_choose;
mod xcode_farm_record;
mod xcode_farm_verify;
mod xcode_push;
mod xcode_verify;
mod xcode_verify_checks;
