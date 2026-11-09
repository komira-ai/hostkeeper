//! What the tests of this crate share: a scratch directory a version tree can be removed from,
//! the running user, and today's archive built header by header.

#![allow(dead_code, reason = "each test binary uses a part of the support")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_config::platform::Platform;
use hostkeeper_unpack::Owner;
use sha2::{Digest, Sha256};

pub const SERVICE: &str = "web";
pub const VERSION: &str = "1.4.0";

/// A scratch directory whose read-only version trees are opened up before it is removed.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        Scratch(
            hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
                .expect("a scratch directory"),
        )
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        open_up(self.0.path());
    }
}

/// Gives every directory below `path` mode 0700 so the tree can be removed.
fn open_up(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            open_up(&entry.path());
        }
    }
}

/// The running user: what the unpack gives the tree to.
pub fn me() -> Owner {
    Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// An octal field of `width` bytes: digits, then NUL.
fn octal(value: u64, width: usize) -> Vec<u8> {
    let mut out = format!("{value:0w$o}", w = width - 1).into_bytes();
    out.push(0);
    out
}

/// One ustar entry: its header, then `data` padded to the block.
fn entry(out: &mut Vec<u8>, name: &str, flag: u8, mode: u64, data: &[u8]) {
    let mut block = [0u8; 512];
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..108].copy_from_slice(&octal(mode, 8));
    block[108..116].copy_from_slice(&octal(1000, 8));
    block[116..124].copy_from_slice(&octal(1000, 8));
    block[124..136].copy_from_slice(&octal(data.len() as u64, 12));
    block[136..148].copy_from_slice(&octal(1_800_000_000, 12));
    block[156] = flag;
    block[257..265].copy_from_slice(b"ustar\x0000");
    block[148..156].copy_from_slice(b"        ");
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    let mut field = format!("{sum:06o}").into_bytes();
    field.extend_from_slice(b"\0 ");
    block[148..156].copy_from_slice(&field);
    out.extend_from_slice(&block);
    out.extend_from_slice(data);
    out.extend(std::iter::repeat_n(0u8, (512 - data.len() % 512) % 512));
}

/// A block header of zstd: last flag, raw type, size.
fn block_header(last: bool, size: u32) -> [u8; 3] {
    let bytes = ((size << 3) | u32::from(last)).to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// One zstd frame of a 1 MiB window holding `data` in raw blocks.
fn zstd(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 10 << 3];
    for chunk in data.chunks(128 * 1024) {
        out.extend_from_slice(&block_header(false, chunk.len() as u32));
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&block_header(true, 0));
    out
}

/// Today's archive of `web` at [`VERSION`]: its `manifest.toml` and `bin/web`.
pub fn archive() -> Vec<u8> {
    let manifest = format!(
        "schema = 1\nname = \"{SERVICE}\"\nversion = \"{VERSION}\"\nplatform = \"{}\"\n\
         min_hostkeeper = \"0.0.1\"\n\n[hook]\ncommand = [\"bin/web\"]\nverify_deadline_secs = 120\n\
         [hook.timeout_secs]\nprepare = 0\ncutover = 30\nverify = 60\nrollback = 30\ncleanup = 20\n\n\
         [units]\nrun = []\n\n[params]\n",
        Platform::current().as_str()
    );
    let mut tar = Vec::new();
    entry(&mut tar, "manifest.toml", b'0', 0o644, manifest.as_bytes());
    entry(&mut tar, "bin/", b'5', 0o755, &[]);
    entry(&mut tar, "bin/web", b'0', 0o755, b"#!web\n");
    tar.extend_from_slice(&[0u8; 1024]);
    zstd(&tar)
}
