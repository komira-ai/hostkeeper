//! A program of a network-install test (`hostkeeper-pxe serve`, QEMU), started in a network
//! namespace with `ip netns exec`, which `exec`s it: its output is read line by line into the
//! [`Bus`] and kept in a file of the job's artifacts. It is killed when dropped; an end the
//! test did not ask for is an [`super::events::Event::Ended`], which fails every wait after it.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::events::{Bus, Source, stopping};
use super::lan::{Lan, ip_program};

/// One run of a program.
pub struct Program {
    name: String,
    child: Child,
    stopping: Arc<AtomicBool>,
}

impl Program {
    /// Starts `program` with `args` in the namespace `ns`; its standard output and error are the
    /// lines of `source`, appended to the file `log`.
    pub fn start(
        bus: &Bus,
        source: Source,
        name: &str,
        ns: &str,
        program: &str,
        args: &[String],
        log: &Path,
    ) -> Program {
        let mut child = Command::new(ip_program())
            .args(Lan::exec_in(ns, program, args))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {name} ({program}): {e}"));
        let stopping = stopping();
        let out = child.stdout.take().expect("a piped standard output");
        let err = child.stderr.take().expect("a piped standard error");
        bus.read(source, name, out, log, Arc::clone(&stopping));
        // The two streams of one program end together; one end event is enough.
        bus.read(source, name, err, log, Arc::new(AtomicBool::new(true)));
        println!("started: {name}: {program} {}", args.join(" "));
        Program {
            name: name.to_owned(),
            child,
            stopping,
        }
    }

    /// Stops the program (SIGKILL) as the test meant to, and reaps it.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // An error is a program that has already ended; `wait` reaps it either way.
        let _ = self.child.kill();
        let _ = self.child.wait();
        println!("stopped: {}", self.name);
    }
}
