//! The fixtures of the `dep` tests: a root host with NanoDEP's key file and the state
//! directory, runs of a verb, and test certificates.

#![allow(
    dead_code,
    reason = "each test binary uses part of the shared fixtures"
)]

use super::common;

use common::{FakeApi, FakeNet, SVC, config, kit, ready_host, with_env_api};
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm::dep::{Dep, run};
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_step::Mode;
use hostkeeper_mdm_step::fake::FakeHost;

pub const KEY: &str = "dep-api-key-1234";
pub const NANODEP_ENV: &str = "/etc/hostkeeper-mdm/nanodep.env";
pub const STATE: &str = "/var/lib/hostkeeper-mdm";
pub const UUID_FILE: &str = "/var/lib/hostkeeper-mdm/dep-profile-uuid";
pub const SUM_FILE: &str = "/var/lib/hostkeeper-mdm/dep-profile.sha256";
pub const UUID: &str = "0123456789abcdef0123456789abcdef";

/// A root host with NanoDEP's key file (0600, root's) and the state directory.
pub fn dep_host() -> FakeHost {
    let host = ready_host(&kit());
    host.put_file(
        NANODEP_ENV,
        0o600,
        (0, 0),
        format!("NANODEP_API={KEY}\n").as_bytes(),
    );
    host.put_dir(STATE, 0o700, (SVC, SVC));
    host
}

/// Runs `d` on `host` with `api` answering, as the settings `c` say.
pub fn go_with(
    host: &FakeHost,
    api: &FakeApi,
    c: &MdmConfig,
    d: &Dep,
    mode: Mode,
) -> (Result<bool, MdmError>, String) {
    let net = FakeNet::new(Default::default());
    let mut out = Vec::new();
    let result = with_env_api(host, &net, api, 1_700_000_000, |env| {
        run(env, c, d, mode, &mut out)
    });
    (result, String::from_utf8(out).unwrap())
}

/// Runs `d` on `host` with the example settings.
pub fn go(host: &FakeHost, api: &FakeApi, d: &Dep, mode: Mode) -> (Result<bool, MdmError>, String) {
    go_with(host, api, &config(), d, mode)
}

/// A writer that fails on every write after the first `n`.
pub struct FailsAt {
    pub n: usize,
    pub seen: usize,
}

impl std::io::Write for FailsAt {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.seen += 1;
        match self.seen > self.n {
            true => Err(std::io::Error::other("the output is closed")),
            false => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `d` on `host` with the output failing after its first `n` writes.
pub fn go_failing(
    host: &FakeHost,
    api: &FakeApi,
    d: &Dep,
    mode: Mode,
    n: usize,
) -> Result<bool, MdmError> {
    let net = FakeNet::new(Default::default());
    let mut out = FailsAt { n, seen: 0 };
    with_env_api(host, &net, api, 1_700_000_000, |env| {
        run(env, &config(), d, mode, &mut out)
    })
}

/// A self-signed PEM certificate.
pub fn certificate() -> String {
    let made = rcgen::generate_simple_self_signed(vec!["nanodep.example.com".to_owned()])
        .expect("a test certificate");
    made.cert.pem()
}
