//! What the coordinator sends a daemon to set sshd up (`HeartbeatResponse.ssh` of
//! `hostkeeper.agent.v1`, docs/design/ssh-through-coordinator.md sections 3 and 9) and what the
//! daemon says back (`HeartbeatRequest.ssh`), as plain data for the loop. The loop judges none
//! of it: [`crate::SshSink`], the daemon's, applies it and says which generation it holds.

/// How sshd treats passwords, as the coordinator wants it.
pub const PASSWORD_KEEP: &str = "keep";
/// Passwords off: only where every account with a password has a key path as well.
pub const PASSWORD_OFF: &str = "off";

/// The host's SSH access, whole, as the coordinator sent it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushedSsh {
    /// Raised by the coordinator at each change of the rest; the daemon applies a record only
    /// when it differs from the one it last applied.
    pub generation: u64,
    /// False is the revert: the daemon removes what it wrote.
    pub enabled: bool,
    /// The CA public keys the host trusts, in `authorized_keys` syntax, one to two, of the
    /// host's class only.
    pub ca_public_keys: Vec<String>,
    /// The account that takes certificates; empty: none.
    pub login_user: String,
    /// The one principal a certificate for this host carries.
    pub principal: String,
    /// The certificate serials the coordinator has revoked.
    pub revoked_serials: Vec<u64>,
    /// [`PASSWORD_KEEP`] or [`PASSWORD_OFF`]; any other text is read as keep.
    pub password_auth: String,
}

/// What the daemon reports of its SSH setup: sent in every heartbeat of a daemon that has a
/// [`crate::SshSink`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshReport {
    /// The generation of the last record the daemon took up, whether or not it came to
    /// `applied`; 0 before any.
    pub applied_generation: u64,
    /// One of the closed set of states of the design's section 9.
    pub state: String,
    /// The `SHA256:` fingerprints of sshd's host keys.
    pub host_key_fingerprints: Vec<String>,
    /// The failing keyword or account class of the state; no secret, no account name.
    pub detail: String,
}
