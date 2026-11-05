//! `hostkeeper-fixture-hook`, the workload fixture of the privileged and end-to-end tests. What it
//! does and the interface it keeps are in `hostkeeper_privileged::fixture`, which decides
//! everything; this program only reads its command line through it, starts the child of `sleep`,
//! blocks where a signal must end it, and exits with the outcome it is given.

use std::error::Error;
use std::io::{self, Write};
use std::process::{Command, ExitCode, Stdio};

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
                // The child of the step's control group: `serve`, with no output of its own, so
                // that the step's output ends when the step and its child have ended.
                let mut child = Command::new(&me.exe)
                    .arg(fixture::SERVE)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                fixture::write_pid_file(&path, &[me.pid, child.id()], None)?;
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
