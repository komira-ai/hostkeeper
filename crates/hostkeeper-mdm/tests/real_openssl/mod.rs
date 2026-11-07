//! A host whose openssl is the real one, reading the bytes the fake host holds, and the SCEP
//! CAs the pinned `scepserver ca -init` made (`tests/fixtures/scepserver-2.3.0`, see its
//! README): what the depot check reads on a real machine, from what the real tool writes.

#![allow(dead_code, reason = "each test binary uses part of the shared helpers")]

use std::cell::Cell;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_mdm_step::{Cmd, Host, Kind, Meta, Output, RealHost, Way};
use hostkeeper_testkit::Scratch;

/// The program the kit runs as openssl.
pub const OPENSSL: &str = "/usr/bin/openssl";

/// A CA as `scepserver ca -init` of the pinned release writes it: `ca.key`, then `ca.pem`.
pub struct Ca {
    /// `ca.key`.
    pub key: &'static [u8],
    /// `ca.pem`.
    pub pem: &'static [u8],
}

/// A CA made with the arguments the kit passes (no `-key-password`).
pub const CA_A: Ca = Ca {
    key: include_bytes!("../fixtures/scepserver-2.3.0/a/ca.key"),
    pem: include_bytes!("../fixtures/scepserver-2.3.0/a/ca.pem"),
};

/// Another CA made so: its key is not the one of [`CA_A`]'s certificate.
pub const CA_B: Ca = Ca {
    key: include_bytes!("../fixtures/scepserver-2.3.0/b/ca.key"),
    pem: include_bytes!("../fixtures/scepserver-2.3.0/b/ca.pem"),
};

/// A CA made with `-key-password other`, which the kit never passes.
pub const CA_OTHER_PASSWORD: Ca = Ca {
    key: include_bytes!("../fixtures/scepserver-2.3.0/password/ca.key"),
    pem: include_bytes!("../fixtures/scepserver-2.3.0/password/ca.pem"),
};

/// `inner`, except that openssl is run for real: each argument that names a regular file of
/// `inner` is replaced by a scratch copy of that file's bytes (mode 0400), and the command is
/// run as [`RealHost`] runs a command (no environment, stdin closed).
pub struct RealOpenssl<'a> {
    inner: &'a dyn Host,
    dir: Scratch,
    copies: Cell<usize>,
}

impl<'a> RealOpenssl<'a> {
    /// The wrapper, with its scratch directory named after `name`.
    pub fn new(inner: &'a dyn Host, name: &str) -> RealOpenssl<'a> {
        let dir = Scratch::under(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("hostkeeper-mdm-openssl-{name}"),
        )
        .expect("a scratch directory");
        RealOpenssl {
            inner,
            dir,
            copies: Cell::new(0),
        }
    }

    /// `arg`, or the path of a scratch copy of the regular file of `inner` it names.
    fn copied(&self, arg: &str) -> io::Result<String> {
        let regular = arg.starts_with('/')
            && self
                .inner
                .stat(arg)?
                .is_some_and(|meta| meta.kind == Kind::File);
        if !regular {
            return Ok(arg.to_owned());
        }
        let bytes = self.inner.read(arg)?.unwrap_or_default();
        let n = self.copies.get() + 1;
        self.copies.set(n);
        let copy = self.dir.path().join(n.to_string());
        fs::write(&copy, bytes)?;
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o400))?;
        Ok(copy.to_str().expect("a UTF-8 scratch path").to_owned())
    }
}

impl Host for RealOpenssl<'_> {
    fn euid(&self) -> u32 {
        self.inner.euid()
    }
    fn egid(&self) -> u32 {
        self.inner.egid()
    }
    fn canonical(&self, path: &str) -> io::Result<String> {
        self.inner.canonical(path)
    }
    fn stat(&self, path: &str) -> io::Result<Option<Meta>> {
        self.inner.stat(path)
    }
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.read(path)
    }
    fn read_limited(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_limited(path, limit)
    }
    fn list(&self, path: &str) -> io::Result<Option<Vec<String>>> {
        self.inner.list(path)
    }
    fn write(&self, path: &str, mode: u32, owner: (u32, u32), bytes: &[u8]) -> io::Result<()> {
        self.inner.write(path, mode, owner, bytes)
    }
    fn mkdir(&self, path: &str, mode: u32, owner: (u32, u32)) -> io::Result<()> {
        self.inner.mkdir(path, mode, owner)
    }
    fn remove(&self, path: &str) -> io::Result<()> {
        self.inner.remove(path)
    }
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        if cmd.program != OPENSSL {
            return self.inner.run(cmd);
        }
        let mut real = Cmd::new(OPENSSL);
        for arg in &cmd.args {
            real = real.arg(&self.copied(arg)?);
        }
        RealHost::new().run(&real)
    }
    fn way(&self, path: &str) -> io::Result<Option<Way>> {
        self.inner.way(path)
    }
}
