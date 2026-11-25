//! Compiles the vendored Remote Execution API protos `mac-farm` speaks (`proto/third_party`,
//! each file pinned by SHA-256 in its `PINS`) into Rust: the messages, the clients and, for the
//! fake farm of the verb's tests, the servers.
//!
//! `protox` parses the `.proto` files in pure Rust, so the build needs no `protoc`; the
//! well-known `google.protobuf` types map to `prost-types`. Nothing generated is checked in: the
//! code lands in `OUT_DIR`.

/// The files code is generated for; their imports are resolved from [`INCLUDES`] and generated
/// too.
const FILES: &[&str] = &[
    "build/bazel/remote/execution/v2/remote_execution.proto",
    "google/bytestream/bytestream.proto",
    "google/longrunning/operations.proto",
    "google/rpc/error_details.proto",
    "google/rpc/status.proto",
];

/// The include roots, relative to this package (a build script runs in its package's
/// directory).
const INCLUDES: &[&str] = &[
    "proto/third_party/remote-apis",
    "proto/third_party/googleapis",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let descriptors = protox::compile(FILES, INCLUDES)?;
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        // A fake farm writes only the methods it serves; the rest answer UNIMPLEMENTED.
        .generate_default_stubs(true)
        // The farm's metadata of a failed job, read by key.
        .btree_map(".google.rpc.ErrorInfo")
        .compile_fds(descriptors)?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
