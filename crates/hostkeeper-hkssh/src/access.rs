//! What a connection to a host needs before ssh starts: the key, a certificate for the host that
//! is still valid, and the host's pinned key fingerprints.

use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::operator::v1::{HostKeysRequest, SignUserCertRequest};

use crate::cli::Global;
use crate::client::{Client, from_status, unary};
use crate::error::Error;
use crate::programs::{Programs, normalize_public_key};
use crate::state::{CertInfo, State};

/// A certificate with fewer seconds left than this is replaced, so that a connection that takes
/// a moment to start does not present one that expires on the way.
const RENEW_BEFORE_SECS: u64 = 120;

/// The key the certificate is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `<type> <base64>`.
    pub public_key: String,
    /// What `ssh -i` takes: the private key, or the public key file when the agent holds the
    /// private one.
    pub ssh_identity: PathBuf,
}

fn read_public(path: &Path) -> Result<String, Error> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
        what: "a public key",
        path: path.to_owned(),
        source,
    })?;
    text.lines()
        .next()
        .and_then(normalize_public_key)
        .ok_or_else(|| Error::Unusable(format!("{} holds no public key", path.display())))
}

/// The key to use: the file `--identity` names (its `.pub` beside it); else the agent's first
/// key; else the key of the state directory, made now when it is not there yet.
pub fn identity(
    state: &State,
    programs: &dyn Programs,
    requested: Option<&Path>,
) -> Result<Identity, Error> {
    if let Some(private) = requested {
        let mut public = private.as_os_str().to_owned();
        public.push(".pub");
        return Ok(Identity {
            public_key: read_public(Path::new(&public))?,
            ssh_identity: private.to_owned(),
        });
    }
    state.ensure()?;
    if let Some(public_key) = programs.agent_key() {
        let file = state.agent_public();
        state.write(&file, &format!("{public_key}\n"))?;
        return Ok(Identity {
            public_key,
            ssh_identity: file,
        });
    }
    let private = state.key();
    if !private.exists() {
        programs.generate_key(&private)?;
    }
    let mut public = private.as_os_str().to_owned();
    public.push(".pub");
    Ok(Identity {
        public_key: read_public(Path::new(&public))?,
        ssh_identity: private,
    })
}

/// The serial of a certificate whose key id is `<operator name>/<serial>`.
fn serial_of(key_id: &str) -> Result<u64, Error> {
    key_id
        .rsplit_once('/')
        .and_then(|(_, serial)| serial.parse().ok())
        .ok_or_else(|| Error::Unusable("the certificate's key id has no serial".into()))
}

/// A certificate of `identity` for `host` that is valid for at least two more minutes: the one
/// held when there is one, else a new one from the coordinator (kept for the next call).
pub async fn certificate(
    client: &mut Client,
    global: &Global,
    state: &State,
    identity: &Identity,
    host: &str,
    reason: &str,
    now: u64,
) -> Result<CertInfo, Error> {
    if let Some(held) = state.cert_info(host)
        && held.public_key == identity.public_key
        && held.expires > now + RENEW_BEFORE_SECS
        && state.cert_file(host).exists()
    {
        return Ok(held);
    }
    let request = SignUserCertRequest {
        public_key: identity.public_key.clone(),
        reason: reason.to_owned(),
        host_id: host.to_owned(),
    };
    let reply = client
        .sign_user_cert(unary(request, global))
        .await
        .map_err(|status| from_status(&status))?
        .into_inner();
    let kind = reply.certificate.split_whitespace().next().unwrap_or("");
    if !kind.ends_with("-cert-v01@openssh.com") {
        return Err(Error::Unusable(
            "the coordinator's answer is not an OpenSSH certificate".into(),
        ));
    }
    let info = CertInfo {
        serial: serial_of(&reply.key_id)?,
        expires: reply.expires_unix,
        login_user: reply.login_user,
        key_id: reply.key_id,
        public_key: identity.public_key.clone(),
    };
    state.store_cert(host, reply.certificate.trim_end(), &info)?;
    Ok(info)
}

/// The host keys pinned for `host`, as the coordinator holds them.
#[derive(Debug, PartialEq, Eq)]
pub struct Pins {
    /// `pinned`, `no_pin` or `changed`.
    pub state: String,
    pub fingerprints: Vec<String>,
}

/// Reads the pins of `host` from the coordinator.
pub async fn pins(client: &mut Client, global: &Global, host: &str) -> Result<Pins, Error> {
    let request = HostKeysRequest {
        host_id: host.to_owned(),
    };
    let reply = client
        .host_keys(unary(request, global))
        .await
        .map_err(|status| from_status(&status))?
        .into_inner();
    Ok(Pins {
        state: reply.state,
        fingerprints: reply.fingerprints,
    })
}

/// Keeps the pins of `host` for ssh's known-hosts command, and says which warning to give. A host
/// with no pin is refused: connecting would trust whatever answers, and only an operator's
/// approval pins a host. A `changed` host keeps its old pins, so a changed key fails in ssh; the
/// caller says so.
pub fn keep_pins(state: &State, host: &str, pins: &Pins) -> Result<(), Error> {
    match pins.state.as_str() {
        "pinned" | "changed" if !pins.fingerprints.is_empty() => {
            state.store_pins(host, &pins.fingerprints)
        }
        "no_pin" => Err(Error::Unusable(format!(
            "no host key is pinned for {host}: an operator approves what the host reports \
             before this command connects (docs/operations/enrollment.md)"
        ))),
        other => Err(Error::Unusable(format!(
            "the coordinator gave no usable pins for {host} (state {other:?})"
        ))),
    }
}

/// The known-hosts line ssh may trust: `host` and the key when `fingerprint` is among the pins
/// in `pins_text`, else nothing.
pub fn known_host_line(
    pins_text: &str,
    host: &str,
    kind: &str,
    key: &str,
    fingerprint: &str,
) -> Option<String> {
    pins_text
        .lines()
        .any(|pin| pin == fingerprint)
        .then(|| format!("{host} {kind} {key}"))
}
