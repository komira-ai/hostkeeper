//! Fixtures of the `push-cert` tests: push certificates made with rcgen, and a host ready for
//! the verb.

#![allow(
    dead_code,
    reason = "each test binary uses part of the shared fixtures"
)]

use hostkeeper_mdm::MdmError;
use hostkeeper_mdm::push_cert::{PushCert, With, run};
use hostkeeper_mdm_core::profile::Topic;
use hostkeeper_mdm_step::Mode;
use hostkeeper_mdm_step::fake::FakeHost;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use time::OffsetDateTime;

use crate::common::{FakeApi, FakeNet, SVC, config, kit, random_hex, ready_host, with_env_api};

pub const STATE: &str = "/var/lib/hostkeeper-mdm";
pub const ETC: &str = "/etc/hostkeeper-mdm";
pub const CERT: &str = "/srv/keys/push.pem";
pub const KEY: &str = "/srv/keys/push.key";
pub const PROFILE: &str = "/var/lib/hostkeeper-mdm/www/enroll.mobileconfig";
pub const TOPIC_FILE: &str = "/var/lib/hostkeeper-mdm/push-topic";
pub const SUM_FILE: &str = "/var/lib/hostkeeper-mdm/push-cert.sha256";
pub const PUSHCERT: &str = "/api/v1/nanomdm/pushcert";
pub const TOPIC: &str = "com.apple.mgmt.External.0a1b2c3d-4e5f-6789-abcd-0123456789ab";
pub const NANOHUB_KEY: &str = "nanohub-api-key-for-tests";
/// The injected time of the tests, unix seconds.
pub const NOW: u64 = 1_800_000_000;
/// When the test push certificate ends.
pub const NOT_AFTER: i64 = 1_900_000_000;
const UID: [u64; 7] = [0, 9, 2342, 19_200_300, 100, 1, 1];

/// A certificate and its key, as PEM text.
#[derive(Clone)]
pub struct Pair {
    pub cert: String,
    pub key: String,
}

/// A self-signed certificate with subject UID `uid` (none when `None`), ending at the unix
/// time `not_after`, and its key.
pub fn pair(uid: Option<&str>, not_after: i64) -> Pair {
    let key = KeyPair::generate().expect("a key");
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "APSP:test");
    if let Some(uid) = uid {
        params
            .distinguished_name
            .push(DnType::CustomDnType(UID.to_vec()), uid);
    }
    params.not_before = OffsetDateTime::from_unix_timestamp(NOT_AFTER - 1_000_000).unwrap();
    params.not_after = OffsetDateTime::from_unix_timestamp(not_after).unwrap();
    let cert = params.self_signed(&key).expect("a certificate");
    Pair {
        cert: cert.pem(),
        key: key.serialize_pem(),
    }
}

/// The push certificate of [`TOPIC`], valid until [`NOT_AFTER`].
pub fn push_pair() -> Pair {
    pair(Some(TOPIC), NOT_AFTER)
}

/// A host ready for `push-cert`: installed state with `www`, the secrets, the service user,
/// and `pair` as `CERT` (0644) and `KEY` (0600) outside any repository.
pub fn host(pair: &Pair) -> FakeHost {
    let host = ready_host(&kit());
    host.put_dir(STATE, 0o700, (SVC, SVC));
    host.put_dir(&format!("{STATE}/www"), 0o700, (SVC, SVC));
    host.put_dir(ETC, 0o750, (0, SVC));
    let scep = format!("SCEP_CHALLENGE_PASSWORD={}\n", random_hex());
    host.put_file(&format!("{ETC}/scep.env"), 0o600, (0, 0), scep.as_bytes());
    let hub = format!("NANOHUB_API_KEY={NANOHUB_KEY}\n");
    host.put_file(&format!("{ETC}/nanohub.env"), 0o600, (0, 0), hub.as_bytes());
    host.put_file(CERT, 0o644, (1000, 1000), pair.cert.as_bytes());
    host.put_file(KEY, 0o600, (1000, 1000), pair.key.as_bytes());
    host
}

/// What `With::Pair` takes for the test files.
pub fn with_pair() -> With {
    With::Pair {
        cert: CERT.to_owned(),
        key: KEY.to_owned(),
    }
}

/// `push-cert` over `host` and `api` at `now`; the result and standard output.
pub fn go(
    host: &FakeHost,
    api: &FakeApi,
    now: u64,
    with: With,
    mode: Mode,
) -> (Result<(), MdmError>, String) {
    let a = PushCert {
        config: config(),
        with,
    };
    let net = FakeNet::new(Default::default());
    let mut out = Vec::new();
    let result = with_env_api(host, &net, api, now, |env| run(env, &a, mode, &mut out));
    (result, String::from_utf8(out).unwrap())
}

/// The recorded topic as a [`Topic`].
pub fn topic() -> Topic {
    Topic::new(TOPIC).unwrap()
}
