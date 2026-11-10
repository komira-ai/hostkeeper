//! The `lego` and `caddy` of `tests/mdm_cert_units.rs`. The test copies this program under both
//! names into its tree; what it does depends on the name it is started by:
//!
//! - `lego <command> ... --path DIR ... --domains NAME ...`: appends one line to `DIR/calls`,
//!   its arguments separated by spaces, then a second line `token <value>` with the DNS
//!   provider's variable it was started with (the test's own value, from the unit's
//!   `EnvironmentFile=`). For `run` it then writes `DIR/certificates/NAME.crt`, as lego does.
//! - `caddy ...`: appends `start` to `$XDG_DATA_HOME/starts` and then waits until it is stopped,
//!   as a server does.
//!
//! Anything else exits 2. A program of the tests, not shipped.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The DNS provider's variable the test's `dns.env` sets.
const TOKEN: &str = "CLOUDFLARE_DNS_API_TOKEN";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let name = args
        .first()
        .and_then(|a| Path::new(a).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    match name.as_str() {
        "lego" => lego(&args[1..]),
        "caddy" => caddy(),
        _ => ExitCode::from(2),
    }
}

/// The value after `flag` in `args`.
fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn append(path: &Path, line: &str) -> bool {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"))
        .is_ok()
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test fixture reporting the secret variable the unit under test hands it"
)]
fn token() -> String {
    std::env::var(TOKEN).unwrap_or_default()
}

fn lego(args: &[String]) -> ExitCode {
    let (Some(dir), Some(domain)) = (value(args, "--path"), value(args, "--domains")) else {
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(dir);
    let calls = dir.join("calls");
    if !append(&calls, &args.join(" ")) || !append(&calls, &format!("token {}", token())) {
        return ExitCode::from(1);
    }
    if args.first().map(String::as_str) == Some("run") {
        let certificates = dir.join("certificates");
        let made = std::fs::create_dir_all(&certificates)
            .and_then(|()| std::fs::write(certificates.join(format!("{domain}.crt")), "CERT"));
        if made.is_err() {
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

#[expect(
    clippy::disallowed_methods,
    reason = "the unit under test gives the server its data directory in XDG_DATA_HOME, as it gives Caddy"
)]
fn data_home() -> Option<String> {
    std::env::var("XDG_DATA_HOME").ok()
}

fn caddy() -> ExitCode {
    let Some(home) = data_home() else {
        return ExitCode::from(2);
    };
    if !append(&Path::new(&home).join("starts"), "start") {
        return ExitCode::from(1);
    }
    loop {
        std::thread::park();
    }
}
