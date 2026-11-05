//! `hello`: the binary of the example service. It only forwards to [`hello::main`], so that
//! everything it does is in the library, where coverage measures it.

fn main() -> std::process::ExitCode {
    hello::main()
}
