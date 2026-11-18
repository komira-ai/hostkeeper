//! The `ipmitool` port (`docs/design/console-broker.md`, decision D6 and section 17.3): how the
//! coordinator runs the BMC tool, and what a run of it is to [`crate::bmc`].
//!
//! The coordinator runs `ipmitool` as one interactive session per command: `ipmitool -I lanplus
//! -H <address> -U <user> -E -C <cipher> -L <privilege> -N <timeout> -R <retries> shell`, on
//! pipes. The tool opens its RMCP+ session at start, before it reads a command, and then answers
//! one command at a time, each answer ending at its prompt (`ipmitool> `). The password reaches it
//! in the environment (`-E` reads [`PASSWORD_VARIABLE`]) and never in its arguments; the user name
//! is an argument, since the tool takes it nowhere else (`-U`; its shell's `set username` comes
//! too late, after the session is open). Nothing in this module prints either: an [`Invocation`]
//! and a [`ToolText`] print redacted.
//!
//! [`Tool`] is the port: the coordinator's implementation starts the process, and
//! [`crate::scripted`] is the double the tests script.

use std::fmt;

use crate::credential::BmcCredential;
use crate::targets::Address;

/// What the tool prints when it waits for the next command.
pub const PROMPT: &str = "ipmitool> ";

/// The environment variable `-E` reads the password from.
pub const PASSWORD_VARIABLE: &str = "IPMI_PASSWORD";

/// The privilege level the session asks for (`-L`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    /// `USER`.
    User,
    /// `OPERATOR`: enough for Serial over LAN and for power.
    Operator,
    /// `ADMINISTRATOR`, the default.
    Administrator,
}

impl Privilege {
    /// The level as `-L` takes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Privilege::User => "USER",
            Privilege::Operator => "OPERATOR",
            Privilege::Administrator => "ADMINISTRATOR",
        }
    }
}

/// How each session is run: the coordinator's `--console-ipmi-*` flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The RMCP+ cipher suite (`-C`, `--console-ipmi-cipher`).
    pub cipher: u8,
    /// The privilege level (`-L`, `--console-ipmi-privilege`).
    pub privilege: Privilege,
    /// The seconds the tool waits for each answer (`-N`, `--console-ipmi-timeout-s`).
    pub timeout_secs: u32,
    /// How often the tool sends a request again that had no answer (`-R`,
    /// `--console-ipmi-retries`).
    pub retries: u32,
}

impl Default for Settings {
    /// The design's defaults: cipher suite 3, `ADMINISTRATOR`, 1 second, 2 retries.
    fn default() -> Self {
        Self {
            cipher: 3,
            privilege: Privilege::Administrator,
            timeout_secs: 1,
            retries: 2,
        }
    }
}

/// One run of the tool: its arguments and, for its environment only, the password. Printed, it
/// shows neither the arguments (the user name is among them) nor the password.
pub struct Invocation<'a> {
    args: Vec<String>,
    credential: &'a BmcCredential,
}

impl<'a> Invocation<'a> {
    /// The shell session at `address` as `credential`, run as `settings` say; with `verbose`, the
    /// tool also says why a session could not be opened (`-v`), which is all such a run is for.
    #[must_use]
    pub fn new(
        address: &Address,
        credential: &'a BmcCredential,
        settings: &Settings,
        verbose: bool,
    ) -> Self {
        let mut args: Vec<String> = [
            "-I",
            "lanplus",
            "-H",
            &address.to_string(),
            "-U",
            credential.user(),
            "-E",
            "-C",
            &settings.cipher.to_string(),
            "-L",
            settings.privilege.as_str(),
            "-N",
            &settings.timeout_secs.to_string(),
            "-R",
            &settings.retries.to_string(),
        ]
        .map(str::to_owned)
        .to_vec();
        if verbose {
            args.push("-v".to_owned());
        }
        args.push("shell".to_owned());
        Self { args, credential }
    }

    /// The tool's arguments, after the program.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The password, for the value of [`PASSWORD_VARIABLE`] in the tool's environment and for
    /// nothing else.
    #[must_use]
    pub fn password(&self) -> &str {
        self.credential.password()
    }
}

impl fmt::Debug for Invocation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invocation { <redacted> }")
    }
}

/// Text the tool wrote to its standard error. It may hold the user name, so it is read by
/// [`crate::bmc`] to name a failure and is never printed: its `Debug` gives its length only.
#[derive(Clone, PartialEq, Eq)]
pub struct ToolText(String);

impl ToolText {
    /// The text `text`.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The text, to be matched and never shown.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ToolText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ToolText(<{} bytes>)", self.0.len())
    }
}

/// Why a run of the tool gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellError {
    /// The program could not be started.
    Start,
    /// No prompt came within the run's deadline, and the program was killed.
    Deadline,
    /// The program ended before its next prompt, having written this to its standard error.
    Ended(ToolText),
}

/// The BMC tool: one session per call of [`Tool::open`].
pub trait Tool: Send + Sync {
    /// Starts the tool as `invocation` says and waits for its first prompt, which comes once its
    /// session with the BMC is open.
    ///
    /// # Errors
    /// [`ShellError`]: it could not be started, gave no prompt in time, or ended first (no answer
    /// at the address, a refused credential, no free session).
    fn open(&self, invocation: &Invocation<'_>) -> Result<Box<dyn Shell>, ShellError>;
}

/// An open session of the tool. Dropping it ends the session and the program.
pub trait Shell {
    /// Sends `command` and answers what the tool printed for it, up to its next prompt.
    ///
    /// # Errors
    /// [`ShellError`]: no prompt came in time, or the program ended.
    fn ask(&mut self, command: &str) -> Result<String, ShellError>;
}

#[cfg(test)]
#[path = "ipmi_tests.rs"]
mod tests;
