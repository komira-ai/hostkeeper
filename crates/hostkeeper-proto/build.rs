//! Compiles hostkeeper's protos into Rust, and writes their descriptor set and the command
//! table.
//!
//! `protox` parses the `.proto` files in pure Rust, so the build needs no `protoc`;
//! `tonic-prost-build` turns the descriptors into messages, service traits and clients, and
//! `src/ctl_gen.rs` turns them into the command table. Nothing generated is checked in: the
//! code, the descriptor set `src/lib.rs` embeds as `FILE_DESCRIPTOR_SET`, and the table
//! `src/ctl.rs` includes land in `OUT_DIR`.
//!
//! [`FROZEN`] is in the descriptor set, which the compatibility test reads, and nothing else:
//! no Rust code is generated from it, and the command generator skips its package.

use std::path::PathBuf;

/// The generator of the command table; the unit tests of this crate run it on fixture protos.
#[path = "src/ctl_gen.rs"]
mod ctl_gen;

/// Every file under `proto/hostkeeper`; `tests/layout.rs` fails when one is missing here.
const FILES: &[&str] = &[
    "hostkeeper/options.proto",
    "hostkeeper/agent/v1/agent.proto",
    "hostkeeper/operator/v1/operator.proto",
    "hostkeeper/operator/v1/enrollment.proto",
    "hostkeeper/operator/v1/entry.proto",
    "hostkeeper/operator/v1/rollouts.proto",
    "hostkeeper/operator/v1/mdm.proto",
    "hostkeeper/operator/v1/repositories.proto",
    "hostkeeper/operator/v1/hostgroups.proto",
    "hostkeeper/local/v1/admin.proto",
    "hostkeeper/release/v1/release.proto",
];

/// The files of [`FILES`] that no program serves any more: kept in the descriptor set, so the
/// compatibility test still holds them to the last release, with no code generated from them.
const FROZEN: &[&str] = &["hostkeeper/local/v1/admin.proto"];

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
    let encoded = compiler.encode_file_descriptor_set();
    std::fs::write(out.join("hostkeeper.binpb"), &encoded)?;

    // Read back from the encoded set, so the generator sees the options too.
    let pool = prost_reflect::DescriptorPool::decode(&encoded[..])?;
    let table = ctl_gen::generate(&pool).map_err(|e| e.to_string())?;
    std::fs::write(out.join("ctl_commands.rs"), table)?;

    let mut served = compiler.file_descriptor_set();
    served.file.retain(|file| !FROZEN.contains(&file.name()));
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        // No transport: the generated clients take any channel, and the crate needs no runtime.
        .build_transport(false)
        // A release's maps (a manifest's params) are ordered, so a manifest encodes to the same
        // bytes on every run: the audit entry of a set records the SHA-256 of that encoding.
        .btree_map(".hostkeeper.release.v1")
        // The targets of a set and of a release are ordered by platform too, so the coordinator
        // judges, audits and answers them in the order of their spellings.
        .btree_map(".hostkeeper.operator.v1.SetDesiredRequest.targets")
        .btree_map(".hostkeeper.operator.v1.GetReleaseResponse.targets")
        .compile_fds(served)?;

    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
