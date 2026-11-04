//! Compiles hostkeeper's protos into Rust, and writes their descriptor set.
//!
//! `protox` parses the `.proto` files in pure Rust, so the build needs no `protoc`;
//! `tonic-prost-build` turns the descriptors into messages, service traits and clients. Nothing
//! generated is checked in: the code, and the descriptor set `src/lib.rs` embeds as
//! `FILE_DESCRIPTOR_SET`, land in `OUT_DIR`.

use std::path::PathBuf;

/// Every file under `proto/hostkeeper`; `tests/layout.rs` fails when one is missing here.
const FILES: &[&str] = &[
    "hostkeeper/options.proto",
    "hostkeeper/agent/v1/agent.proto",
    "hostkeeper/operator/v1/operator.proto",
    "hostkeeper/local/v1/admin.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A build script runs in its package's directory.
    let mut compiler = protox::Compiler::new(["proto"])?;
    compiler.include_imports(true).include_source_info(true);
    compiler.open_files(FILES)?;

    #[expect(
        clippy::disallowed_methods,
        reason = "Cargo tells a build script where to write only through OUT_DIR"
    )]
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo set no OUT_DIR")?);
    // The encoded set keeps custom options (`(hostkeeper.secret)`), which the decoded one drops.
    std::fs::write(
        out.join("hostkeeper.binpb"),
        compiler.encode_file_descriptor_set(),
    )?;

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        // No transport: the generated clients take any channel, and the crate needs no runtime.
        .build_transport(false)
        .compile_fds(compiler.file_descriptor_set())?;

    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
