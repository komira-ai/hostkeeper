//! What the tests of the emitted script share: a scratch directory holding the scripts, a copy of
//! the fake curl as `curl` and as `find` first on PATH, the token and CA files; a run of the
//! host's `bash` on a script; what the fake recorded of each call; and loopback listeners of
//! port 0 the fake forwards to.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use hostkeeper_testkit::Scratch;

/// The fake curl (tests/bin/fake_curl.rs).
const FAKE: &str = env!("CARGO_BIN_EXE_hostkeeper-hkctl-fake-curl");

/// The version stamp the scripts of the tests print.
pub const STAMP: &str = "1.2.3+0123456789ab";

/// The operator token of the tests: what the token file holds and what the coordinator's tokens
/// file holds the SHA-256 of.
pub const TOKEN: &str = "hkctl-operator-token-of-the-test";

/// The fixture's command table, which `build.rs` generated from tests/fixtures.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists"
)]
pub mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

/// The fixture's encoded descriptor set.
pub const FIXTURE_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fixture.binpb"));

/// Which script a run starts.
#[derive(Clone, Copy)]
pub enum Script {
    /// Emitted from the real protos.
    Real,
    /// Emitted from the fixture protos.
    Fixture,
}

/// The directory of one test.
pub struct Rig {
    dir: Scratch,
}

/// One run of a script.
#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What the fake recorded of one run of `curl` or `find`.
#[derive(Debug, Clone)]
pub struct Call {
    pub program: String,
    pub argv: Vec<String>,
    pub environ: Vec<String>,
    /// What it read from each `@` argument, by the argument's index.
    pub reads: Vec<(usize, Vec<u8>)>,
    /// The script's stdout so far.
    pub stdout: String,
}

impl Call {
    /// What it read from the argument after the `nth` (from 0) `flag`.
    pub fn read_after(&self, flag: &str, nth: usize) -> Option<String> {
        let index = self
            .argv
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == flag)
            .nth(nth)
            .map(|(i, _)| i + 1)?;
        self.reads
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
    }

    /// Its arguments with each `/dev/fd/<n>` spelled `/dev/fd/N`.
    pub fn argv_shape(&self) -> Vec<String> {
        self.argv
            .iter()
            .map(|a| match a.strip_prefix("@/dev/fd/") {
                Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                    "@/dev/fd/N".to_owned()
                }
                _ => a.clone(),
            })
            .collect()
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the script runs the host's bash and find, found on the PATH the tests run with"
)]
fn host_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

impl Rig {
    /// A new directory for the test `name`, with both scripts, the fake as `curl` and `find`, the
    /// token file (0600) and a CA file.
    pub fn new(name: &str) -> Self {
        let dir = Scratch::under(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("hkctl-{name}"),
        )
        .expect("a scratch directory");
        let rig = Self { dir };
        std::fs::create_dir(rig.bin()).expect("the bin directory");
        for program in ["curl", "find"] {
            std::fs::copy(FAKE, rig.bin().join(program)).expect("the fake copied");
        }
        let real = hostkeeper_hkctl::script(hostkeeper_proto::ctl::COMMANDS, STAMP)
            .expect("the real protos make a script");
        let fixture = hostkeeper_hkctl::script(fixture::COMMANDS, STAMP)
            .expect("the fixture protos make a script");
        rig.put("real.hkctl", real.as_bytes(), 0o755);
        rig.put("fixture.hkctl", fixture.as_bytes(), 0o755);
        rig.put("token", format!("{TOKEN}\n").as_bytes(), 0o600);
        rig.put("ca.pem", b"not read: the fake curl does no TLS\n", 0o644);
        rig
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The directory of the fake: first on PATH.
    pub fn bin(&self) -> PathBuf {
        self.path("bin")
    }

    /// Writes `name` in the test's directory with `mode`; answers its path as text.
    pub fn put(&self, name: &str, bytes: &[u8], mode: u32) -> String {
        let path = self.path(name);
        // A file of a mode that refuses its owner a write is replaced, not written over.
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, bytes).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        path.display().to_string()
    }

    /// The fake curl answers `status` and `body` instead of forwarding.
    pub fn reply(&self, status: u16, body: &str) {
        std::fs::write(self.bin().join("reply"), format!("{status}\n{body}")).expect("reply");
    }

    /// The `nth` (from 1) run of `curl` is answered `status` and `body`, whatever `reply` or the
    /// forwarding would answer; the other runs are answered as before.
    pub fn reply_nth(&self, nth: usize, status: u16, body: &str) {
        std::fs::write(
            self.bin().join(format!("reply-{nth}")),
            format!("{status}\n{body}"),
        )
        .expect("reply-n");
    }

    /// The fake curl fails as when nothing listens.
    pub fn fail(&self) {
        std::fs::write(self.bin().join("fail"), b"").expect("fail");
    }

    /// The `nth` (from 1) run of `curl` fails as when nothing listens; the others do not.
    pub fn fail_nth(&self, nth: usize) {
        std::fs::write(self.bin().join(format!("fail-{nth}")), b"").expect("fail-n");
    }

    /// The global flags of a call with the token file: `--endpoint`, `--ca-file`,
    /// `--token-file`.
    pub fn globals(&self, port: u16) -> Vec<String> {
        vec![
            "--endpoint".to_owned(),
            format!("https://127.0.0.1:{port}"),
            "--ca-file".to_owned(),
            self.path("ca.pem").display().to_string(),
            "--token-file".to_owned(),
            self.path("token").display().to_string(),
        ]
    }

    /// Runs `script` with `args` under the host's `bash`, in the test's directory, with only
    /// PATH (the fake first) and `env` in its environment, `stdin` on its stdin, and its stdout
    /// in `bin/stdout`, where the fake reads it.
    pub fn run(&self, script: Script, args: &[String], stdin: &[u8], env: &[(&str, &str)]) -> Ran {
        let name = match script {
            Script::Real => "real.hkctl",
            Script::Fixture => "fixture.hkctl",
        };
        let mut path = OsString::from(self.bin());
        path.push(":");
        path.push(host_path());
        let stdout_path = self.bin().join("stdout");
        let stdout = std::fs::File::create(&stdout_path).expect("the stdout file");
        let mut child = Command::new("bash")
            .arg(self.path(name))
            .args(args)
            .env_clear()
            .env("PATH", &path)
            .envs(env.iter().copied())
            .current_dir(self.dir.path())
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(Stdio::piped())
            .spawn()
            .expect("bash starts");
        let mut input = child.stdin.take().expect("stdin");
        input.write_all(stdin).expect("stdin written");
        drop(input);
        let out = child.wait_with_output().expect("bash ends");
        Ran {
            code: out.status.code().unwrap_or(-1),
            stdout: std::fs::read_to_string(&stdout_path).expect("stdout"),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Every call the fake recorded, in order.
    pub fn calls(&self) -> Vec<Call> {
        let mut calls = Vec::new();
        for n in 0.. {
            let at = self.bin().join("calls").join(n.to_string());
            if !at.is_dir() {
                break;
            }
            let read = |file: &str| std::fs::read(at.join(file)).unwrap_or_default();
            let split = |bytes: Vec<u8>| -> Vec<String> {
                bytes
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect()
            };
            let argv = split(read("argv"));
            let reads = (0..argv.len())
                .filter_map(|i| {
                    std::fs::read(at.join(format!("read-{i}")))
                        .ok()
                        .map(|b| (i, b))
                })
                .collect();
            calls.push(Call {
                program: String::from_utf8_lossy(&read("program")).into_owned(),
                argv,
                environ: split(read("environ")),
                reads,
                stdout: String::from_utf8_lossy(&read("stdout")).into_owned(),
            });
        }
        calls
    }

    /// The calls of `curl` alone.
    pub fn curls(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.program == "curl")
            .collect()
    }

    /// Forgets the calls recorded so far, and any canned reply or failure.
    pub fn reset(&self) {
        let _ = std::fs::remove_dir_all(self.bin().join("calls"));
        let _ = std::fs::remove_file(self.bin().join("reply"));
        let _ = std::fs::remove_file(self.bin().join("fail"));
        if let Ok(entries) = std::fs::read_dir(self.bin()) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("reply-") || name.starts_with("fail-") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// `words` as owned arguments.
pub fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// One request a loopback listener answered.
#[derive(Debug, Clone)]
pub struct Seen {
    /// The request line and headers, as sent.
    pub head: String,
    pub body: Vec<u8>,
    /// The status and body it was answered.
    pub status: u16,
    pub answer: Vec<u8>,
}

impl Seen {
    /// The request line's target: the path and query.
    pub fn target(&self) -> &str {
        self.head.split(' ').nth(1).unwrap_or_default()
    }

    /// The value of the header `name`, matched without case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }
}

/// A plain HTTP/1.1 listener on 127.0.0.1, port 0, answering one request per connection with
/// its handler; it records each request.
pub struct Loopback {
    pub port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// What a handler answers: a status and a body.
pub type Answer = (u16, Vec<u8>);

/// Reads one request: its head up to the blank line, then `Content-Length` bytes.
fn read_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
        bytes.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&bytes).trim_end().to_owned();
    let length = head
        .lines()
        .find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    let _ = stream.read_exact(&mut body);
    (head, body)
}

impl Loopback {
    /// A listener whose `handler` answers each request (head, body); `start` runs first on the
    /// listener's thread, and its value is handed to every call of the handler.
    pub fn serve<S, F>(start: impl FnOnce() -> S + Send + 'static, handler: F) -> Self
    where
        S: 'static,
        F: Fn(&S, &str, &[u8]) -> Answer + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            let state = start();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (head, body) = read_request(&mut stream);
                let (status, answer) = handler(&state, &head, &body);
                let mut out = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    answer.len()
                )
                .into_bytes();
                out.extend_from_slice(&answer);
                // Recorded before it is answered, so a test that reads the record after the
                // script ends sees it.
                record.lock().expect("the record").push(Seen {
                    head,
                    body,
                    status,
                    answer,
                });
                let _ = stream.write_all(&out);
            }
        });
        Self { port, seen }
    }

    /// Every request answered so far.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the record").clone()
    }
}
