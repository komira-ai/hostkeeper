//! The kit of the tests of `serve`: the files a run reads (a sealed directory, the daemon config,
//! the SSH keys, the token, the coordinator's CA), the packets of a client (DHCP, TFTP and HTTP,
//! over real loopback sockets), and a run of `serve` on its own thread whose log lines the test
//! reads as they come: each wait is for a line, an event, never for a time.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use clap::Parser as _;
use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_pxe_seed::layout::{deb_path, initrd_path, kernel_path};
use hostkeeper_testkit::Scratch;

use super::fake::{self, Coordinator, Fake, Pki};
use crate::cli::{Cli, Verb};
use crate::seal::{Seal, SealedDeb, SealedFile, SealedNetboot, SealedSet};
use crate::serve::platforms::loaders_of;
use crate::serve::{ServePorts, serve_with};
use crate::testing::{scratch, sha256_hex};

/// The operator token of the tests.
pub const TOKEN: &str = "operator-token-0123456789abcdef-0123456789";

/// The time every log line is stamped with.
pub const NOW: u64 = 1_800_000_000;

/// The MAC of the recorded machine, and of the enrolled one.
pub const RECORDED: &str = "52:54:00:00:00:01";
pub const ENROLLED: &str = "52:54:00:00:00:02";

/// A daemon config the daemon's parser accepts.
pub const DAEMON_CONFIG: &str = "\
coordinator     = \"https://192.0.2.1:8443\"
ca_sha256       = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"
release_channel = \"https://releases.example\"

[trusted_signers]
organisation  = \"komira-ai\"
workflow      = \".github/workflows/release.yml\"
refs          = [\"refs/tags/*\"]
hosted_runner = true
";

/// The ISO name of a platform's set.
pub fn iso_name(platform: LinuxPlatform) -> String {
    format!("ubuntu-24.04.5-live-server-{}.iso", platform.deb_arch())
}

/// The bytes of a sealed file of `platform`: its part's name repeated to `len` bytes.
pub fn content(platform: LinuxPlatform, part: &str, len: usize) -> Vec<u8> {
    format!("{part} of {platform}; ")
        .bytes()
        .cycle()
        .take(len)
        .collect()
}

/// The sizes the parts of a sealed set are made with: the shim takes three TFTP blocks of the
/// default size and a short one, the ISO more than one HTTP chunk.
pub const SHIM_BYTES: usize = 1500;
pub const ISO_BYTES: usize = (1 << 20) + 4096;

/// Writes the sealed directory of `platforms` at `dir`, and its seal.
pub fn seal_dir(dir: &Path, platforms: &[LinuxPlatform]) -> Seal {
    let mut sets = BTreeMap::new();
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(&path, bytes).expect("write");
        SealedFile {
            bytes: bytes.len() as u64,
            name: name.to_owned(),
            sha256: sha256_hex(bytes),
        }
    };
    for &platform in platforms {
        let loaders = loaders_of(platform);
        let deb = write(
            deb_path(platform).trim_start_matches('/'),
            &content(platform, "deb", 300),
        );
        let set = SealedSet {
            deb: SealedDeb {
                bytes: deb.bytes,
                commit: "c0ffee".to_owned(),
                name: deb.name,
                sha256: deb.sha256,
                source_ref: "refs/tags/v1.2.3".to_owned(),
            },
            grub: write(loaders.grub, &content(platform, "grub", 700)),
            initrd: write(
                initrd_path(platform).trim_start_matches('/'),
                &content(platform, "initrd", 400),
            ),
            iso: write(&iso_name(platform), &content(platform, "iso", ISO_BYTES)),
            kernel: write(
                kernel_path(platform).trim_start_matches('/'),
                &content(platform, "kernel", 200),
            ),
            shim: write(loaders.shim, &content(platform, "shim", SHIM_BYTES)),
        };
        sets.insert(platform.as_str().to_owned(), set);
    }
    let seal = Seal {
        netboot: SealedNetboot {
            commit: "c0ffee".to_owned(),
            sha256: "0".repeat(64),
            source_ref: "refs/tags/v1.2.3".to_owned(),
        },
        platforms: sets,
        seal: 1,
        ubuntu: "24.04.5".to_owned(),
        version: "1.2.3".to_owned(),
    };
    std::fs::write(dir.join("seal.json"), seal.to_json()).expect("write the seal");
    seal
}

/// The base64 blob of an ed25519 public key whose key bytes are `filler`.
fn key_blob(filler: u8) -> String {
    use base64::Engine as _;
    let kind = b"ssh-ed25519";
    let mut bytes = (kind.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend(std::iter::repeat_n(filler, 32));
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The files of a run, and the fake coordinator it reads.
pub struct Kit {
    pub scratch: Scratch,
    pub dir: PathBuf,
    pub config: PathBuf,
    pub keys: PathBuf,
    pub ca: PathBuf,
    pub token: PathBuf,
    pub pki: Pki,
    pub coordinator: Coordinator,
}

impl Kit {
    /// A kit named `name` whose directory seals both platforms, whose fake coordinator holds
    /// the record of [`RECORDED`] (`new-1`, not enrolled) and of [`ENROLLED`] (`old-1`,
    /// enrolled, its host id held as approved).
    pub fn new(name: &str) -> Kit {
        Kit::with(name, &LinuxPlatform::ALL)
    }

    /// The kit of [`Kit::new`] sealing `platforms`.
    pub fn with(name: &str, platforms: &[LinuxPlatform]) -> Kit {
        let scratch = scratch(name);
        let root = scratch.path().to_owned();
        let dir = root.join("boot");
        seal_dir(&dir, platforms);
        let file = |name: &str, text: &[u8], mode: u32| {
            let path = root.join(name);
            std::fs::write(&path, text).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            path
        };
        let config = file("hostkeeper.toml", DAEMON_CONFIG.as_bytes(), 0o644);
        let keys = file(
            "authorized_keys",
            format!("ssh-ed25519 {} operator@farm\n", key_blob(7)).as_bytes(),
            0o644,
        );
        let pki = fake::pki();
        let ca = file("ca.pem", pki.ca.as_bytes(), 0o644);
        let token = file("token", format!("{TOKEN}\n").as_bytes(), 0o600);
        let fake = Fake::default();
        fake.set(|s| {
            s.records = vec![
                fake::record("new-1", &[RECORDED], false),
                fake::record("old-1", &[ENROLLED], true),
            ];
        });
        let coordinator = Coordinator::start(&pki, fake);
        Kit {
            scratch,
            dir,
            config,
            keys,
            ca,
            token,
            pki,
            coordinator,
        }
    }

    /// The fake coordinator's state.
    pub fn fake(&self) -> &Fake {
        &self.coordinator.fake
    }

    /// The command line of a run in `mode` whose proxy offers go to `reply_to`, with `extra`
    /// after it.
    pub fn args(&self, mode: &str, reply_to: SocketAddr, extra: &[&str]) -> Vec<OsString> {
        let me = rustix::process::getuid().as_raw().to_string();
        let group = rustix::process::getgid().as_raw().to_string();
        let mut args: Vec<OsString> = [
            "pxe",
            "serve",
            "--mode",
            mode,
            "--address",
            "127.0.0.1",
            "--dhcp-listen",
            "127.0.0.1:0",
            "--proxy-listen",
            "127.0.0.1:0",
            "--tftp-listen",
            "127.0.0.1:0",
            "--http-listen",
            "127.0.0.1:0",
            "--dhcp-reply-to",
            &reply_to.to_string(),
            "--coordinator",
            &self.coordinator.endpoint(),
            "--user-id",
            &me,
            "--group-id",
            &group,
            "--tftp-timeout-ms",
            "50",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        for (flag, path) in [
            ("--ca-file", &self.ca),
            ("--token-file", &self.token),
            ("--dir", &self.dir),
            ("--daemon-config", &self.config),
            ("--ssh-authorized-keys", &self.keys),
        ] {
            args.push(flag.into());
            args.push(path.into());
        }
        args.extend(extra.iter().map(OsString::from));
        args
    }

    /// A run of `serve` with `args`, as the test's own user.
    pub fn start(&self, args: Vec<OsString>) -> Running {
        Running::start(args)
    }
}

/// A run of `serve` on its own thread.
pub struct Running {
    lines: mpsc::Receiver<String>,
    pub seen: Vec<String>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), String>>>,
    pub dhcp: SocketAddr,
    pub proxy: Option<SocketAddr>,
    pub tftp: SocketAddr,
    pub http: SocketAddr,
}

/// The address after `label ` in the start line.
fn address_in(line: &str, label: &str) -> Option<SocketAddr> {
    let rest = line.split(&format!("{label} ")).nth(1)?;
    let word = rest.split([',', ' ']).next()?;
    word.parse().ok()
}

/// Runs `args` through the command's own parse and `serve_with`, as the test's own user, on a
/// thread; the log lines go to `lines`.
fn spawn(
    args: Vec<OsString>,
    notify_socket: Option<OsString>,
) -> (
    mpsc::Receiver<String>,
    tokio::sync::oneshot::Sender<()>,
    JoinHandle<Result<(), String>>,
) {
    let (tx, lines) = mpsc::channel();
    let tx = Arc::new(Mutex::new(tx));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let owner = rustix::process::getuid().as_raw();
    let thread = std::thread::spawn(move || {
        let cli = Cli::try_parse_from(args).map_err(|e| e.to_string())?;
        let Verb::Serve(flags) = cli.verb else {
            return Err("not serve".to_owned());
        };
        let ports = ServePorts {
            now: Arc::new(|| NOW),
            sink: Arc::new(move |line| {
                let _ = tx.lock().expect("the log").send(line.to_owned());
            }),
            stop: Box::new(move || {
                Box::pin(async move {
                    let _ = stopped.await;
                })
            }),
            notify_socket,
        };
        serve_with(&flags, owner, ports)
    });
    (lines, stop, thread)
}

impl Running {
    /// Starts `args` and waits for the start line.
    pub fn start(args: Vec<OsString>) -> Running {
        Running::start_notifying(args, None)
    }

    /// Starts `args` with `notify_socket` as the socket systemd named, and waits for the start
    /// line.
    pub fn start_notifying(args: Vec<OsString>, notify_socket: Option<OsString>) -> Running {
        let (lines, stop, thread) = spawn(args, notify_socket);
        let mut running = Running {
            lines,
            seen: Vec::new(),
            stop: Some(stop),
            thread: Some(thread),
            dhcp: SocketAddr::from(([0, 0, 0, 0], 0)),
            proxy: None,
            tftp: SocketAddr::from(([0, 0, 0, 0], 0)),
            http: SocketAddr::from(([0, 0, 0, 0], 0)),
        };
        let started = running.wait("started in ");
        running.dhcp = address_in(&started, "DHCP").expect("the DHCP address");
        running.proxy = address_in(&started, "proxyDHCP");
        running.tftp = address_in(&started, "TFTP").expect("the TFTP address");
        running.http = address_in(&started, "HTTP").expect("the HTTP address");
        running
    }

    /// The next log line that holds `needle`; the lines before it are kept in `seen`. A run
    /// that ends first fails the test with what it said.
    pub fn wait(&mut self, needle: &str) -> String {
        loop {
            match self.lines.recv() {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if line.contains(needle) {
                        return line;
                    }
                }
                Err(_) => {
                    let ended = self.thread.take().map(|t| t.join().expect("the run"));
                    panic!(
                        "the run ended before {needle:?}: {ended:?}; it said {:#?}",
                        self.seen
                    );
                }
            }
        }
    }

    /// Stops the run and waits for its end: its result, and every line it logged.
    pub fn stop(mut self) -> (Result<(), String>, Vec<String>) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let result = self
            .thread
            .take()
            .expect("running")
            .join()
            .expect("the run");
        self.seen.extend(self.lines.try_iter());
        (result, self.seen)
    }
}

/// Runs `args` to its end, for a start that is refused: its result and its lines.
pub fn refused(args: Vec<OsString>) -> (Result<(), String>, Vec<String>) {
    refused_notifying(args, None)
}

/// Runs `args` to its end with `notify_socket` as the socket systemd named: its result and its
/// lines.
pub fn refused_notifying(
    args: Vec<OsString>,
    notify_socket: Option<OsString>,
) -> (Result<(), String>, Vec<String>) {
    let (lines, _stop, thread) = spawn(args, notify_socket);
    let result = thread.join().expect("the run");
    (result, lines.try_iter().collect())
}

/// A client's UDP socket on loopback.
pub fn client() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").expect("a client socket")
}

/// The octets of a MAC written `aa:bb:cc:dd:ee:ff`.
pub fn octets(mac: &str) -> [u8; 6] {
    let mut out = [0; 6];
    for (i, part) in mac.split(':').enumerate() {
        out[i] = u8::from_str_radix(part, 16).expect("a hex octet");
    }
    out
}

/// A client's DHCP packet: the message type `kind` (1 DISCOVER, 3 REQUEST), from `mac`, with
/// the vendor class, the architecture (option 93), the requested address (option 50) and the
/// server identifier (option 54) given.
pub fn dhcp(
    kind: u8,
    mac: &str,
    class: Option<&str>,
    arch: Option<u16>,
    requested: Option<Ipv4Addr>,
) -> Vec<u8> {
    let mut p = vec![0_u8; 236];
    p[0] = 1;
    p[1] = 1;
    p[2] = 6;
    p[4..8].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    p[28..34].copy_from_slice(&octets(mac));
    p.extend_from_slice(&[99, 130, 83, 99]);
    p.extend_from_slice(&[53, 1, kind]);
    if let Some(class) = class {
        p.push(60);
        p.push(class.len() as u8);
        p.extend_from_slice(class.as_bytes());
    }
    if let Some(arch) = arch {
        p.extend_from_slice(&[93, 2]);
        p.extend_from_slice(&arch.to_be_bytes());
    }
    if let Some(address) = requested {
        p.extend_from_slice(&[50, 4]);
        p.extend_from_slice(&address.octets());
    }
    p.push(255);
    p
}

/// The options of a DHCP reply, by code.
pub fn options(reply: &[u8]) -> BTreeMap<u8, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut i = 240;
    while i < reply.len() && reply[i] != 255 {
        if reply[i] == 0 {
            i += 1;
            continue;
        }
        let len = reply[i + 1] as usize;
        out.insert(reply[i], reply[i + 2..i + 2 + len].to_vec());
        i += 2 + len;
    }
    out
}

/// The next datagram on `socket`.
pub fn receive(socket: &UdpSocket) -> (Vec<u8>, SocketAddr) {
    let mut buffer = vec![0; 2048];
    let (n, from) = socket.recv_from(&mut buffer).expect("a datagram");
    buffer.truncate(n);
    (buffer, from)
}

/// Whether a datagram waits on `socket`, without waiting for one.
pub fn waiting(socket: &UdpSocket) -> bool {
    socket.set_nonblocking(true).expect("non-blocking");
    let mut buffer = [0; 2048];
    let any = socket.recv_from(&mut buffer).is_ok();
    socket.set_nonblocking(false).expect("blocking");
    any
}

/// A TFTP read request of `name` with `options`.
pub fn rrq(name: &str, options: &[(&str, &str)]) -> Vec<u8> {
    let mut p = vec![0, 1];
    p.extend_from_slice(name.as_bytes());
    p.push(0);
    p.extend_from_slice(b"octet\0");
    for (key, value) in options {
        p.extend_from_slice(key.as_bytes());
        p.push(0);
        p.extend_from_slice(value.as_bytes());
        p.push(0);
    }
    p
}

/// A TFTP ACK of `block`.
pub fn ack(block: u16) -> Vec<u8> {
    let mut p = vec![0, 4];
    p.extend_from_slice(&block.to_be_bytes());
    p
}

/// What a TFTP read came to: the file, or the error code and message.
pub type TftpResult = Result<Vec<u8>, (u16, String)>;

/// Reads `name` from the TFTP server at `server`, acknowledging every block (and an OACK),
/// from a new socket.
pub fn tftp_read(server: SocketAddr, name: &str, options: &[(&str, &str)]) -> TftpResult {
    let socket = client();
    tftp_read_from(&socket, server, name, options)
}

/// [`tftp_read`] from `socket`.
pub fn tftp_read_from(
    socket: &UdpSocket,
    server: SocketAddr,
    name: &str,
    options: &[(&str, &str)],
) -> TftpResult {
    socket.send_to(&rrq(name, options), server).expect("send");
    let mut file = Vec::new();
    loop {
        let (packet, from) = receive(socket);
        match u16::from_be_bytes([packet[0], packet[1]]) {
            3 => {
                let block = u16::from_be_bytes([packet[2], packet[3]]);
                file.extend_from_slice(&packet[4..]);
                socket.send_to(&ack(block), from).expect("send");
                let size = options
                    .iter()
                    .find(|(k, _)| *k == "blksize")
                    .map_or(512, |(_, v)| v.parse().expect("a size"));
                if packet.len() - 4 < size {
                    return Ok(file);
                }
            }
            5 => {
                let code = u16::from_be_bytes([packet[2], packet[3]]);
                let message = String::from_utf8_lossy(&packet[4..packet.len() - 1]).into_owned();
                return Err((code, message));
            }
            6 => {
                socket.send_to(&ack(0), from).expect("send");
            }
            op => panic!("TFTP opcode {op}"),
        }
    }
}

/// An HTTP answer: its status, its head and its body.
pub struct HttpAnswer {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

/// `method path` of the HTTP server at `server`, on a connection of its own.
pub fn http(server: SocketAddr, method: &str, path: &str) -> HttpAnswer {
    let mut stream = TcpStream::connect(server).expect("connect");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: boot\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .expect("write");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read");
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a head");
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let status = head[9..12].parse().expect("a status");
    HttpAnswer {
        status,
        head,
        body: bytes[split + 4..].to_vec(),
    }
}
