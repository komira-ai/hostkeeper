//! `hostkeeper`: the binary of the daemon. It only forwards to [`hostkeeper_daemon::cli::main`],
//! so that everything it does is in the library, where coverage measures it.

fn main() -> std::process::ExitCode {
    hostkeeper_daemon::cli::main()
}
