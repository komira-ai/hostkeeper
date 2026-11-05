//! Compiles the fixture protos of this crate's tests into a descriptor set and a command table,
//! with the same generator `hostkeeper-proto`'s build script runs on the real protos. The tests
//! build `ctl` from them to show that a new RPC appears with no hand edit. Nothing generated is
//! checked in, and nothing of it is in the library: only the tests include it.

use std::path::PathBuf;

/// The generator of the command table, from `hostkeeper-proto`, whose unit tests measure it.
#[path = "../hostkeeper-proto/src/ctl_gen.rs"]
mod ctl_gen;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A build script runs in its package's directory.
    let mut compiler = protox::Compiler::new(["tests/fixtures", "../hostkeeper-proto/proto"])?;
    compiler.include_imports(true).include_source_info(true);
    compiler.open_files(["fixture.proto", "fixture_local.proto"])?;
    let encoded = compiler.encode_file_descriptor_set();

    #[expect(
        clippy::disallowed_methods,
        reason = "Cargo tells a build script where to write only through OUT_DIR"
    )]
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo set no OUT_DIR")?);
    std::fs::write(out.join("fixture.binpb"), &encoded)?;
    let pool = prost_reflect::DescriptorPool::decode(&encoded[..])?;
    let table = ctl_gen::generate(&pool).map_err(|e| e.to_string())?;
    std::fs::write(out.join("fixture_commands.rs"), table)?;

    println!("cargo:rerun-if-changed=tests/fixtures");
    println!("cargo:rerun-if-changed=../hostkeeper-proto/proto");
    println!("cargo:rerun-if-changed=../hostkeeper-proto/src/ctl_gen.rs");
    Ok(())
}
