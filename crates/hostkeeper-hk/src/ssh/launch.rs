//! The production launcher of the ssh verbs: a runtime of this thread, the process's streams,
//! the machine's OpenSSH and the clock.

use std::ffi::OsString;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ssh::programs::System as Programs;
use crate::ssh::{Coordinator, Env, Launch, Verb, run};

/// The state directory under the operator's home.
const STATE_UNDER_HOME: &str = ".hostkeeper/ssh";

/// The HTTP/2 PING interval of a tunnel, in seconds.
const KEEPALIVE_SECS: u64 = 30;

/// Runs the verbs for real: `ssh`, `ssh-keygen` and `ssh-add` from the PATH with the agent socket
/// the process inherits, standard input and output as the byte streams, the state in
/// `<home>/.hostkeeper/ssh`.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Launch for System {
    fn launch(
        &self,
        verb: Verb,
        coordinator: Coordinator,
        args: Vec<OsString>,
        name: &str,
        home: &Path,
        stderr: &mut dyn std::io::Write,
    ) -> u8 {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = writeln!(stderr, "{name}: cannot start the runtime: {error}");
                return 1;
            }
        };
        let programs = Programs {
            ssh: "ssh".into(),
            keygen: "ssh-keygen".into(),
            add: "ssh-add".into(),
            agent_socket: None,
        };
        let env = Env {
            programs: &programs,
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            exe: std::env::current_exe().unwrap_or_else(|_| name.into()),
            state_dir: home.join(STATE_UNDER_HOME),
            keepalive_secs: KEEPALIVE_SECS,
            name,
        };
        let code = runtime.block_on(run(
            verb,
            &coordinator,
            &args,
            &env,
            tokio::io::stdin(),
            tokio::io::stdout(),
            stderr,
        ));
        // The blocking read of stdin may still be waiting for a byte; do not wait for it.
        runtime.shutdown_background();
        code
    }
}
