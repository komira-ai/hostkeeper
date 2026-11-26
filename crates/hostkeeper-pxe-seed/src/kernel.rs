//! The parameters a seed adds to the installed kernel's command line (`--kernel-parameter`,
//! repeatable), each checked once when it is made, so the seed cannot fail: a setting read only
//! at boot, such as `fuse.enable_uring=1` for a kernel whose fuse is built in.
//!
//! One value is one word, `<name>` or `<name>=<value>`: 1 to 256 printable ASCII characters with
//! no space, `"`, `\`, `$` or `` ` `` (the word is written inside a double-quoted shell string),
//! and a name of `A-Za-z0-9._-`. [`KernelParameters`] holds the words of one run, in the order
//! given: at most [`MAX_KERNEL_PARAMETERS`], no name twice.
//!
//! The seed writes them to their own GRUB defaults file, [`KERNEL_PARAMETERS_FILE`], added to
//! `GRUB_CMDLINE_LINUX`, and not to the serial console's file, which the daemon keeps at the
//! console's bytes alone ([`hostkeeper_config::serial_console::GRUB_CONSOLE_FILE`]).

use std::str::FromStr;

/// The GRUB defaults file of the kernel parameters. GRUB reads each file of
/// `/etc/default/grub.d/` in name order, and each of these files adds to `GRUB_CMDLINE_LINUX`, so
/// the order of this file and the serial console's does not change the command line's words.
pub const KERNEL_PARAMETERS_FILE: &str = "/etc/default/grub.d/98_hostkeeper_parameters.cfg";

/// The most kernel parameters one seed adds.
pub const MAX_KERNEL_PARAMETERS: usize = 32;

/// The longest kernel parameter, in bytes.
pub const MAX_KERNEL_PARAMETER: usize = 256;

/// Why a `--kernel-parameter` value was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "--kernel-parameter is not <name> or <name>=<value>: 1 to 256 printable ASCII characters without a space, a quote, a backslash, a $ or a backtick, and a name of A-Za-z0-9._-"
)]
pub struct KernelParameterError;

/// One word of the kernel's command line, as [`KernelParameter::from_str`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelParameter(String);

impl KernelParameter {
    /// The word, such as `fuse.enable_uring=1`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name: the word up to its first `=`, or the whole word.
    pub fn name(&self) -> &str {
        self.0.split('=').next().unwrap_or_default()
    }
}

impl FromStr for KernelParameter {
    type Err = KernelParameterError;

    fn from_str(text: &str) -> Result<KernelParameter, KernelParameterError> {
        let printable = text
            .bytes()
            .all(|b| b.is_ascii_graphic() & !matches!(b, b'"' | b'\\' | b'$' | b'`'));
        let name = text.split('=').next().unwrap_or_default();
        let named = !name.is_empty()
            & name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'-'));
        let ok = (1..=MAX_KERNEL_PARAMETER).contains(&text.len()) & printable & named;
        ok.then(|| KernelParameter(text.to_owned()))
            .ok_or(KernelParameterError)
    }
}

/// Why the kernel parameters of a run were refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KernelParametersError {
    /// More than [`MAX_KERNEL_PARAMETERS`].
    #[error("--kernel-parameter is given {0} times, over {MAX_KERNEL_PARAMETERS}")]
    TooMany(usize),
    /// Two words of one name, whose order would decide which the kernel takes.
    #[error("--kernel-parameter's name {0} is given twice")]
    Duplicate(String),
}

/// The kernel parameters of a run, in the order given: none by default, and then the seed writes
/// no file of them, as without this input.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct KernelParameters(Vec<KernelParameter>);

impl KernelParameters {
    /// `parameters`, in their order, once there are at most [`MAX_KERNEL_PARAMETERS`] and no two
    /// share a name.
    pub fn new(
        parameters: Vec<KernelParameter>,
    ) -> Result<KernelParameters, KernelParametersError> {
        if parameters.len() > MAX_KERNEL_PARAMETERS {
            return Err(KernelParametersError::TooMany(parameters.len()));
        }
        for (index, parameter) in parameters.iter().enumerate() {
            if parameters[..index]
                .iter()
                .any(|e| e.name() == parameter.name())
            {
                return Err(KernelParametersError::Duplicate(
                    parameter.name().to_owned(),
                ));
            }
        }
        Ok(KernelParameters(parameters))
    }

    /// The parameters, in their order.
    pub fn parameters(&self) -> &[KernelParameter] {
        &self.0
    }

    /// The text of [`KERNEL_PARAMETERS_FILE`]: one line that adds the words, in their order, to
    /// `GRUB_CMDLINE_LINUX`.
    pub fn grub_defaults(&self) -> String {
        let words: Vec<&str> = self.0.iter().map(KernelParameter::as_str).collect();
        format!(
            "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX {}\"\n",
            words.join(" ")
        )
    }
}
