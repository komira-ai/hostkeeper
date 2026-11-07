//! `hostkeeper-mdm push-cert`: NanoHUB given the APNs push certificate and its key, and the
//! enrollment profile the proxy serves at `/enroll` written, as root.

use std::io::Write;

use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::profile::Topic;
use hostkeeper_mdm_step::Mode;

use crate::{Env, MdmError};

/// Where the topic comes from.
pub enum With {
    /// A push certificate and its key (PEM files), given to NanoHUB.
    Pair {
        /// The certificate.
        cert: String,
        /// Its private key.
        key: String,
    },
    /// `--profile-only`: the profile alone, for `--topic` or the recorded topic.
    ProfileOnly(Option<Topic>),
}

/// What `push-cert` was asked for.
pub struct PushCert {
    /// The settings.
    pub config: MdmConfig,
    /// Where the topic comes from.
    pub with: With,
}

/// Why `push-cert` refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PushCertError {}

impl PushCertError {
    /// The exit code: 1 when a call or a step failed, 2 when the input was refused.
    pub fn exit_code(&self) -> u8 {
        match *self {}
    }
}

/// Check, then print or carry out the upload of the push certificate and the profile.
///
/// # Errors
/// [`MdmError`].
pub fn run(env: &Env<'_>, a: &PushCert, mode: Mode, out: &mut dyn Write) -> Result<(), MdmError> {
    let _ = (env, a, mode, out);
    Ok(())
}
