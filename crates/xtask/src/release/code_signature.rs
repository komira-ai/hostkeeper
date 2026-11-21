//! `release check-code-signature <binary> --program <name>`: refuses a Mach-O program of the
//! release unless macOS's `codesign` says it is signed as the release signs it
//! (`artifacts.yml`, job `sign-code`). The program is looked up in [`MacProgram`]'s table first
//! (a name that is not in it is refused before any tool runs); then, through the [`CodeSign`]
//! port ([`CodesignCli`] runs `codesign`), the verb reads:
//!
//! 1. `codesign --verify --strict --verbose=2`, which must exit 0;
//! 2. `codesign --display --verbose=2`, which must show the program's `Identifier=`, the
//!    `TeamIdentifier=` of [`TEAM_ID`], the `Authority=` chain exactly
//!    [`APPLICATION_IDENTITY`], [`INTERMEDIATE`], [`ROOT`], a `Timestamp=` line (a secure
//!    timestamp) and the `runtime` flag (the hardened runtime) in the `CodeDirectory` flags;
//! 3. `codesign --display --requirements -`, which must print the line
//!    `designated => <the program's requirement>` ([`MacProgram::requirement`]) exactly.
//!
//! Every problem is named, one line each, prefixed by the binary's path: an ad hoc signature,
//! another identifier, another team, a requirement that pins a hash, no hardened runtime and no
//! timestamp each fail by name.

use std::path::Path;

use super::Error;
use super::mac_programs::{APPLICATION_IDENTITY, MacProgram, TEAM_ID};
use super::pkg_signature::{INTERMEDIATE, ROOT};
use super::ports::{CodeSign, Codesigned, Exec, PortError};

/// Checks `binary` against the program called `program` in the table, through `tools`.
pub(crate) fn check(
    tools: &dyn CodeSign,
    binary: &Path,
    program: &str,
) -> Result<Vec<String>, Error> {
    let Some(program) = MacProgram::named(program) else {
        return Err(Error::Failed(format!(
            "--program {program:?} is not a program of the release's table"
        )));
    };
    let mut problems = Vec::new();
    let verified = tools.verify(binary)?;
    if !verified.success {
        problems.push(format!("codesign --verify fails: {}", verified.text.trim()));
    }
    let displayed = tools.display(binary)?;
    if displayed.success {
        problems.extend(display_problems(&displayed.text, &program));
    } else {
        problems.push(format!(
            "codesign --display fails: {}",
            displayed.text.trim()
        ));
    }
    let required = tools.requirements(binary)?;
    if required.success {
        problems.extend(requirement_problems(&required.text, &program));
    } else {
        problems.push(format!(
            "codesign --display --requirements fails: {}",
            required.text.trim()
        ));
    }
    if !problems.is_empty() {
        return Err(Error::Refused(
            problems
                .into_iter()
                .map(|p| format!("{}: {p}", binary.display()))
                .collect(),
        ));
    }
    Ok(vec![format!(
        "{}: {} signed by {APPLICATION_IDENTITY} under {INTERMEDIATE} and {ROOT}, with the hardened runtime and a trusted timestamp",
        binary.display(),
        program.identifier
    )])
}

/// The values of every line of `text` that starts with `key` (`Authority=`, say), in order.
fn values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix(key))
        .collect()
}

/// One line per way `text`, what `codesign --display --verbose=2` printed, is not the signature
/// of `program`.
fn display_problems(text: &str, program: &MacProgram) -> Vec<String> {
    let mut problems = Vec::new();
    if values(text, "Signature=") == ["adhoc"] {
        problems.push("the signature is ad hoc, not a Developer ID signature".to_owned());
    }
    let identifiers = values(text, "Identifier=");
    if identifiers != [program.identifier] {
        problems.push(format!(
            "the identifier is {identifiers:?}, not {:?}",
            program.identifier
        ));
    }
    let teams = values(text, "TeamIdentifier=");
    if teams != [TEAM_ID] {
        problems.push(format!("the team is {teams:?}, not {TEAM_ID:?}"));
    }
    let chain = values(text, "Authority=");
    let want = [APPLICATION_IDENTITY, INTERMEDIATE, ROOT];
    if chain != want {
        problems.push(format!("the certificate chain is {chain:?}, not {want:?}"));
    }
    if values(text, "Timestamp=").is_empty() {
        problems.push("the signature has no secure timestamp (no Timestamp= line)".to_owned());
    }
    if !has_runtime(text) {
        problems.push(
            "the hardened runtime is not on (no runtime flag in the CodeDirectory flags)"
                .to_owned(),
        );
    }
    problems
}

/// Whether a `CodeDirectory` line of `text` has `flags=0x...(...)` with `runtime` among the
/// named flags.
fn has_runtime(text: &str) -> bool {
    text.lines()
        .filter(|l| l.trim().starts_with("CodeDirectory"))
        .flat_map(str::split_whitespace)
        .filter_map(|word| word.strip_prefix("flags="))
        .filter_map(|flags| flags.split_once('(')?.1.strip_suffix(')'))
        .any(|names| names.split(',').any(|name| name == "runtime"))
}

/// One line when `text`, what `codesign --display --requirements -` printed, has no line
/// `designated => <the requirement of program>`, or has another.
fn requirement_problems(text: &str, program: &MacProgram) -> Vec<String> {
    let want = program.requirement();
    let designated = values(text, "designated => ");
    if designated == [want.as_str()] {
        Vec::new()
    } else {
        vec![format!(
            "the designated requirement is {designated:?}, not {want:?}"
        )]
    }
}

/// Runs `codesign` through an [`Exec`].
pub(crate) struct CodesignCli<E> {
    exec: E,
}

impl<E> CodesignCli<E> {
    /// The adapter running `codesign` through `exec`.
    pub(crate) fn new(exec: E) -> CodesignCli<E> {
        CodesignCli { exec }
    }
}

impl<E: Exec> CodesignCli<E> {
    /// Runs `codesign` with `flags` and then the path of `binary`; a failure to start is a
    /// [`PortError`], any exit is the answer (stdout, then stderr, as one text).
    fn run(&self, flags: &[&str], binary: &Path) -> Result<Codesigned, PortError> {
        let mut args: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
        let Some(path) = binary.to_str() else {
            return Err(PortError::new(
                format!("codesign {} {}", args.join(" "), binary.display()),
                "the path is not UTF-8",
            ));
        };
        args.push(path.to_owned());
        let what = format!("codesign {}", args.join(" "));
        let ran = self
            .exec
            .spawn("codesign", &args)
            .map_err(|e| PortError::new(what, e.to_string()))?;
        Ok(Codesigned {
            success: ran.success,
            text: format!(
                "{}\n{}",
                String::from_utf8_lossy(&ran.stdout),
                String::from_utf8_lossy(&ran.stderr)
            ),
        })
    }
}

impl<E: Exec> CodeSign for CodesignCli<E> {
    fn verify(&self, binary: &Path) -> Result<Codesigned, PortError> {
        self.run(&["--verify", "--strict", "--verbose=2"], binary)
    }

    fn display(&self, binary: &Path) -> Result<Codesigned, PortError> {
        self.run(&["--display", "--verbose=2"], binary)
    }

    fn requirements(&self, binary: &Path) -> Result<Codesigned, PortError> {
        self.run(&["--display", "--requirements", "-"], binary)
    }
}

#[cfg(test)]
mod tests;
