//! The command line: what `hello` was asked to be.

use std::ffi::OsString;

/// The port the server listens on unless `--port` says otherwise.
pub const DEFAULT_PORT: u16 = 9310;

/// The words that start the update hook without `update-hook` before them: the launcher of a
/// hostkeeper package may run `<hook> <step> ...` and not the manifest's `update-hook`.
pub const STEPS: [&str; 6] = [
    "prepare", "cutover", "verify", "rollback", "cleanup", "uninstall",
];

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Serve on `127.0.0.1:<port>`.
    Serve { port: u16, fail_ready: bool },
    /// Print `hello <version>`.
    Version,
    /// Run the update hook; the arguments start at the step.
    Hook(Vec<OsString>),
}

/// Parses the arguments after the program name. The error is the message to print after
/// `hello: `.
pub fn parse(args: &[OsString]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Serve {
            port: DEFAULT_PORT,
            fail_ready: false,
        });
    };
    let word = first.to_str();
    if word == Some("update-hook") {
        return Ok(Command::Hook(args[1..].to_vec()));
    }
    if word.is_some_and(|word| STEPS.contains(&word)) {
        return Ok(Command::Hook(args.to_vec()));
    }
    parse_flags(args)
}

fn parse_flags(args: &[OsString]) -> Result<Command, String> {
    let mut port = None;
    let mut fail_ready = false;
    let mut version = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--port") => {
                let value = args.next().ok_or("--port needs a port number")?;
                if port.replace(parse_port(value)?).is_some() {
                    return Err("--port given twice".to_owned());
                }
            }
            Some("--fail-ready") => fail_ready = true,
            Some("--version") => version = true,
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    if version {
        return Ok(Command::Version);
    }
    Ok(Command::Serve {
        port: port.unwrap_or(DEFAULT_PORT),
        fail_ready,
    })
}

/// A port number: decimal digits only, 1 to 65535.
fn parse_port(value: &OsString) -> Result<u16, String> {
    let text = value.to_str().unwrap_or_default();
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    match text.parse::<u16>() {
        Ok(port @ 1..) if digits => Ok(port),
        _ => Err(format!("--port needs a number from 1 to 65535, not {value:?}")),
    }
}

#[cfg(test)]
mod tests;
