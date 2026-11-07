//! The real machine, the kernel's random source, the wall clock and the program's entry.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use hostkeeper_mdm_step::{Host, RealHost};

use crate::{Grant, Machine, Net, System, run};

/// The machine itself: a [`RealHost`] with the grants a verb asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealMachine;

impl Machine for RealMachine {
    fn host(&self, grants: &[Grant]) -> Box<dyn Host + '_> {
        let host = grants
            .iter()
            .fold(RealHost::new(), |host, g| host.trust_below(&g.dir, g.uid));
        Box::new(host)
    }

    fn owner(&self, path: &str) -> io::Result<Option<u32>> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(meta.uid())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// The kernel's random source.
///
/// # Errors
/// The source could not be read.
pub fn system_random(buf: &mut [u8]) -> io::Result<()> {
    getrandom::fill(buf).map_err(io::Error::from)
}

/// Runs the program: reads the arguments, acts on the machine, exits as documented.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let net = Net::new(true);
    // The wall clock names an uninstall snapshot (`MDM_STATE/removed/<unix seconds>`) and
    // nothing else: no decision reads it.
    let secs = SystemTime::now().duration_since(UNIX_EPOCH);
    let system = System {
        machine: &RealMachine,
        net: &net,
        random: &system_random,
        now: secs.map_or(0, |d| d.as_secs()),
    };
    ExitCode::from(run(
        &args,
        &system,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    ))
}
