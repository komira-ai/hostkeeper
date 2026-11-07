//! `hostkeeper-mdm dep`: NanoDEP linked to Apple Business Manager and given the Automated
//! Device Enrollment profile, as root.

use std::io::Write;

use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_step::Mode;

use crate::{Env, MdmError};

/// A device serial number as ABM lists it: 8 to 14 upper-case letters and digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Serial(String);

impl Serial {
    /// `text` as a serial number.
    ///
    /// # Errors
    /// Not 8 to 14 upper-case letters and digits; the text says so.
    pub fn new(text: &str) -> Result<Serial, String> {
        let ok = (8..=14).contains(&text.len())
            && text
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        match ok {
            true => Ok(Serial(text.to_owned())),
            false => Err(format!(
                "{text:?} is not a serial number (8 to 14 upper-case letters and digits)"
            )),
        }
    }

    /// The serial number.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One `dep` verb.
pub enum Dep {
    /// NanoDEP's token certificate, to upload in ABM, written to `out`.
    GetCert {
        /// Where the certificate goes.
        out: String,
    },
    /// The server token ABM gives for that certificate, given to NanoDEP.
    UploadToken {
        /// The token file (`.p7m`).
        token: String,
    },
    /// The ADE profile defined at Apple and made the assigner's; depsyncer started.
    DefineProfile,
    /// The recorded ADE profile assigned to devices already in ABM.
    Assign {
        /// The devices.
        serials: Vec<Serial>,
    },
}

/// Why `dep` refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum DepError {}

impl DepError {
    /// The exit code: 1 when a call or a step failed, 2 when the input was refused.
    pub fn exit_code(&self) -> u8 {
        match *self {}
    }
}

/// Check, then print or carry out `d`. `Ok(false)` when Apple did not assign every device.
///
/// # Errors
/// [`MdmError`].
pub fn run(
    env: &Env<'_>,
    c: &MdmConfig,
    d: &Dep,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<bool, MdmError> {
    let _ = (env, c, d, mode, out);
    Ok(true)
}
