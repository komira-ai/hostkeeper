//! A program whose standard output is read line by line as it is written, never kept whole: for a
//! long log (`pmset -g log`) of which only some lines matter. The `launchctl` port
//! ([`hostkeeper_launchd::Launchctl`]) keeps the first 64 KiB of a stream, which of a log are its
//! oldest lines.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// A program that hands each line of its standard output to a reader.
pub trait LineTool: Send + Sync {
    /// Runs the program with `args`, its environment empty and its standard input and error
    /// closed, hands each line of its standard output (bytes that are not UTF-8 replaced, the
    /// line end dropped) to `line`, and returns its exit code: none when it was ended by a
    /// signal.
    ///
    /// # Errors
    /// Why it could not be started, waited for, or was killed at its deadline.
    fn run_lines(&self, args: &[String], line: &mut dyn FnMut(&str))
    -> Result<Option<i32>, String>;
}

/// The real [`LineTool`]: `program`, killed when it runs past `deadline`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineProcess {
    program: PathBuf,
    deadline: Duration,
}

impl LineProcess {
    pub fn new(program: &Path, deadline: Duration) -> LineProcess {
        LineProcess {
            program: program.to_path_buf(),
            deadline,
        }
    }
}

fn lock(child: &Mutex<Child>) -> std::sync::MutexGuard<'_, Child> {
    child.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LineTool for LineProcess {
    fn run_lines(
        &self,
        args: &[String],
        line: &mut dyn FnMut(&str),
    ) -> Result<Option<i32>, String> {
        let mut child = Command::new(&self.program)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take();
        let child = Arc::new(Mutex::new(child));
        let (done, finished) = mpsc::channel::<()>();
        let watched = Arc::clone(&child);
        let deadline = self.deadline;
        // Kills the child at the deadline, which closes its output and so ends the read below.
        let watchdog = thread::spawn(move || {
            let late = finished.recv_timeout(deadline) == Err(RecvTimeoutError::Timeout);
            if late {
                let _ = lock(&watched).kill();
            }
            late
        });
        if let Some(stdout) = stdout {
            let mut reader = BufReader::new(stdout);
            let mut bytes = Vec::new();
            while reader.read_until(b'\n', &mut bytes).unwrap_or(0) > 0 {
                let text = String::from_utf8_lossy(&bytes);
                line(text.trim_end_matches(['\n', '\r']));
                bytes.clear();
            }
        }
        let _ = done.send(());
        let killed = watchdog.join().unwrap_or(false);
        let status = lock(&child).wait().map_err(|e| e.to_string())?;
        if killed {
            return Err(format!(
                "killed after {} seconds",
                self.deadline.as_secs_f64()
            ));
        }
        Ok(status.code())
    }
}
