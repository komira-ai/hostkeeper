//! `hostkeeper-fixture-hook`, the workload fixture of the privileged tests. What it does and the
//! interface it keeps are in `hostkeeper_privileged::fixture`, which decides everything and starts
//! `sleep`'s child; this program only forwards to it, blocks where a signal must end it, and exits
//! with the outcome it is given.

use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;

use hostkeeper_hook::Outcome;
use hostkeeper_privileged::fixture::{self, Ending, Me, Run};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("hostkeeper-fixture-hook: {e}");
            Outcome::Failure.into()
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let exe = std::env::current_exe()?;
    let step = match fixture::parse_args(std::env::args_os().skip(1))? {
        Run::Serve => block(),
        Run::Step(step) => step,
    };
    let me = Me {
        pid: std::process::id(),
        exe,
    };
    let ending = fixture::run_step(&step, &me, &mut io::stdout(), &mut io::stderr())?;
    io::stdout().flush()?;
    match ending {
        Ending::Exit(outcome) => Ok(outcome.into()),
        Ending::Block { pid_file } => {
            if let Some(path) = pid_file {
                let mut child = fixture::start_child(&me, &path)?;
                // Reaped if it ends before the step does.
                std::thread::spawn(move || child.wait());
            }
            block()
        }
    }
}

/// Parks for good: only a signal ends the process (`SIGTERM` from systemd, at the unit's bound
/// or its stop).
fn block() -> ! {
    loop {
        std::thread::park();
    }
}
