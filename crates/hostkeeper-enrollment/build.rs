//! Compiles the store's own messages (`proto/`) into Rust.
//!
//! `protox` parses the `.proto` file in pure Rust, so the build needs no `protoc`;
//! `tonic-prost-build` turns the descriptors into messages. There is no service. Nothing
//! generated is checked in: the code lands in `OUT_DIR`, and `src/enroll/records/mod.rs` includes it.

/// The one file under `proto/`.
const FILE: &str = "hostkeeper/enrollment/store/v1/store.proto";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A build script runs in its package's directory.
    let mut compiler = protox::Compiler::new(["proto"])?;
    compiler.open_files([FILE])?;
    tonic_prost_build::configure()
        .build_client(false)
        .build_server(false)
        .build_transport(false)
        .compile_fds(compiler.file_descriptor_set())?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
