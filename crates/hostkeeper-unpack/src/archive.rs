//! The archive reader: one zstd frame ([`Stream`]) holding a ustar archive with pax and GNU
//! long-name extensions ([`Tar`]), read once, front to back, under the bounds of design
//! section 5.5. Nothing here writes: it hands the unpack one header at a time and the bytes of
//! a file in pieces of at most [`BUFFER`].

use std::fs::File;
use std::io::{self, Read};

use ruzstd::decoding::errors::FrameDecoderError;
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
use sha2::{Digest, Sha256};

use crate::error::Rule;

/// The largest zstd window a frame may declare: 8 MiB, the window of `zstd -19` and of every
/// lower level, and the least a decoder is asked to support. A frame that declares more is
/// refused before the decoder allocates anything for it.
pub const MAX_WINDOW_BYTES: u64 = 8 * 1024 * 1024;
/// A tar block.
pub(crate) const BLOCK: usize = 512;
/// The most decompressed bytes read at once, so the most read past `max_unpacked_bytes` before
/// the bound stops the stream.
pub const BUFFER: usize = 64 * 1024;
/// The most bytes of a pax extended header's records.
pub(crate) const MAX_EXTENDED_BYTES: u64 = 8 * 1024;

/// Why the pass stopped: a broken rule (the unpack names the entry), or an I/O error of the
/// archive file itself.
pub(crate) enum Stop {
    Rule(Rule),
    Io(io::Error),
}

/// The archive file, hashed as it is read.
struct Source {
    file: File,
    sha: Sha256,
    eof: bool,
    failed: Option<io::Error>,
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.file.read(buf) {
            Ok(n) => {
                self.eof |= n == 0;
                self.sha.update(&buf[..n]);
                Ok(n)
            }
            Err(e) => {
                let told = io::Error::new(e.kind(), e.to_string());
                self.failed = Some(e);
                Err(told)
            }
        }
    }
}

/// The decompressed bytes of the one zstd frame of the archive, counted as they leave the
/// decoder.
pub(crate) struct Stream {
    frame: FrameDecoder,
    source: Source,
    produced: u64,
    limit: u64,
}

impl Stream {
    /// Reads the frame header of `file`: a frame whose window is above [`MAX_WINDOW_BYTES`] is
    /// [`Rule::Window`]. At most `limit` decompressed bytes are read ([`Rule::TooLarge`]).
    pub(crate) fn open(file: File, limit: u64) -> Result<Stream, Stop> {
        let mut source = Source {
            file,
            sha: Sha256::new(),
            eof: false,
            failed: None,
        };
        let mut frame = FrameDecoder::new();
        frame.set_max_window_size(MAX_WINDOW_BYTES);
        if let Err(e) = frame.init(&mut source) {
            return Err(match e {
                FrameDecoderError::WindowSizeTooBig { requested, .. } => {
                    Stop::Rule(Rule::Window { requested })
                }
                other => refused(&mut source, &other),
            });
        }
        Ok(Stream {
            frame,
            source,
            produced: 0,
            limit,
        })
    }

    /// The decompressed bytes read so far.
    pub(crate) fn produced(&self) -> u64 {
        self.produced
    }

    /// Reads some decompressed bytes; 0 at the end of the frame.
    pub(crate) fn read(&mut self, buf: &mut [u8]) -> Result<usize, Stop> {
        while (self.frame.can_collect() == 0) & !self.frame.is_finished() {
            let strategy = BlockDecodingStrategy::UptoBlocks(1);
            if let Err(e) = self.frame.decode_blocks(&mut self.source, strategy) {
                return Err(refused(&mut self.source, &e));
            }
        }
        let n = self.frame.read(buf).map_err(Stop::Io)?;
        self.produced += n as u64;
        if self.produced > self.limit {
            return Err(Stop::Rule(Rule::TooLarge));
        }
        Ok(n)
    }

    /// After the frame ended: the archive file must end too ([`Rule::SecondFrame`]). Returns
    /// the SHA-256 of every byte of the file.
    pub(crate) fn finish(&mut self) -> Result<[u8; 32], Stop> {
        let mut one = [0u8; 1];
        if self.source.read(&mut one).map_err(Stop::Io)? != 0 {
            return Err(Stop::Rule(Rule::SecondFrame));
        }
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&self.source.sha.clone().finalize());
        Ok(sha)
    }
}

/// What a decoder error means: the archive file failed to read, ended early, or is not zstd.
fn refused(source: &mut Source, error: &FrameDecoderError) -> Stop {
    match (source.failed.take(), source.eof) {
        (Some(e), _) => Stop::Io(e),
        (None, true) => Stop::Rule(Rule::Truncated),
        (None, false) => Stop::Rule(Rule::Decoder(error.to_string())),
    }
}

/// The tar archive inside the stream.
pub(crate) struct Tar {
    stream: Stream,
}

/// A tar header, before the extended headers before it are applied.
pub(crate) struct Header {
    pub name: Vec<u8>,
    pub mode: u64,
    pub size: u64,
    pub flag: u8,
}

/// What pax records said about the next entry.
#[derive(Default)]
pub(crate) struct Extended {
    pub path: Option<Vec<u8>>,
    pub size: Option<u64>,
}

impl Tar {
    pub(crate) fn new(stream: Stream) -> Tar {
        Tar { stream }
    }

    pub(crate) fn produced(&self) -> u64 {
        self.stream.produced()
    }

    /// Fills `buf`; the stream ending first is [`Rule::Truncated`].
    pub(crate) fn fill(&mut self, buf: &mut [u8]) -> Result<(), Stop> {
        let mut at = 0;
        while at < buf.len() {
            let n = self.stream.read(&mut buf[at..])?;
            if n == 0 {
                return Err(Stop::Rule(Rule::Truncated));
            }
            at += n;
        }
        Ok(())
    }

    /// The next block: a header, or the first block of the end marker (all zeros).
    pub(crate) fn block(&mut self) -> Result<[u8; BLOCK], Stop> {
        let mut block = [0u8; BLOCK];
        self.fill(&mut block)?;
        Ok(block)
    }

    /// Skips the padding after `size` bytes of entry data, to the next block.
    pub(crate) fn pad(&mut self, size: u64) -> Result<(), Stop> {
        let mut pad = [0u8; BLOCK];
        let used = (size % BLOCK as u64) as usize;
        let len = (BLOCK - used) % BLOCK;
        self.fill(&mut pad[..len])
    }

    /// The data of an extended header of `size` bytes, with its padding.
    pub(crate) fn extended(&mut self, size: u64) -> Result<Vec<u8>, Stop> {
        let mut data = vec![0u8; size as usize];
        self.fill(&mut data)?;
        self.pad(size)?;
        Ok(data)
    }

    /// After the first zero block: the second block of the end marker, then nothing but zeros
    /// to the end of the frame, a whole number of blocks in all ([`Rule::Trailing`]); then the
    /// end of the file. Returns the SHA-256 of the archive file.
    pub(crate) fn end(&mut self) -> Result<[u8; 32], Stop> {
        let second = self.block()?;
        let mut buf = vec![0u8; BUFFER];
        let mut zero = second.iter().all(|&b| b == 0);
        loop {
            let n = self.stream.read(&mut buf)?;
            zero &= buf[..n].iter().all(|&b| b == 0);
            if !zero {
                return Err(Stop::Rule(Rule::Trailing));
            }
            if n == 0 {
                break;
            }
        }
        if !self.stream.produced().is_multiple_of(BLOCK as u64) {
            return Err(Stop::Rule(Rule::Trailing));
        }
        self.stream.finish()
    }

    /// Reads entry data into `buf` (at most its length, at least one byte).
    pub(crate) fn data(&mut self, buf: &mut [u8]) -> Result<(), Stop> {
        self.fill(buf)
    }
}

/// Reads a ustar (POSIX `ustar\000`) or GNU (`ustar  \0`) header block with a correct checksum.
/// The owner, the group and the times are not read.
pub(crate) fn header(block: &[u8; BLOCK]) -> Result<Header, Rule> {
    let all: u64 = block.iter().map(|&b| u64::from(b)).sum();
    let field: u64 = block[148..156].iter().map(|&b| u64::from(b)).sum();
    let computed = all - field + 8 * u64::from(b' ');
    let posix = &block[257..265] == b"ustar\x0000";
    let gnu = &block[257..265] == b"ustar  \x00";
    let fields = (
        octal(&block[148..156]),
        octal(&block[100..108]),
        octal(&block[124..136]),
    );
    let (Some(stored), Some(mode), Some(size)) = fields else {
        return Err(Rule::Header);
    };
    if (stored != computed) | !(posix | gnu) {
        return Err(Rule::Header);
    }
    let name = cstr(&block[0..100]);
    // The prefix field is POSIX's; GNU puts other fields there.
    let prefix = if posix { cstr(&block[345..500]) } else { &[] };
    let name = if prefix.is_empty() {
        name.to_vec()
    } else {
        [prefix, b"/", name].concat()
    };
    Ok(Header {
        name,
        mode,
        size,
        flag: block[156],
    })
}

/// The bytes of a NUL-terminated field.
fn cstr(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

/// An octal number field: digits between optional spaces, ended by NUL or the field's end. A
/// field that is not (the GNU base-256 form included) is `None`; an empty one is 0.
fn octal(field: &[u8]) -> Option<u64> {
    let text = cstr(field).trim_ascii();
    text.iter().try_fold(0u64, |n, &b| {
        let digit = b.checked_sub(b'0').filter(|d| *d < 8)?;
        n.checked_mul(8)?.checked_add(u64::from(digit))
    })
}

/// A decimal number of a pax record: one or more digits.
fn decimal(text: &[u8]) -> Option<u64> {
    let digits = (!text.is_empty()).then_some(text)?;
    digits.iter().try_fold(0u64, |n, &b| {
        let digit = b.checked_sub(b'0').filter(|d| *d < 10)?;
        n.checked_mul(10)?.checked_add(u64::from(digit))
    })
}

/// Reads the records of a pax extended header: `path` and `size` are taken, `mtime`, `uid`,
/// `gid`, `uname` and `gname` are ignored, an extended attribute or ACL is
/// [`Rule::ExtendedAttribute`] and any other record [`Rule::PaxRecord`].
pub(crate) fn pax(mut data: &[u8]) -> Result<Extended, Rule> {
    let mut out = Extended::default();
    while !data.is_empty() {
        let space = data.iter().position(|&b| b == b' ').ok_or(Rule::Header)?;
        let len = decimal(&data[..space]).ok_or(Rule::Header)? as usize;
        if (len <= space + 1) | (len > data.len()) {
            return Err(Rule::Header);
        }
        let Some((&b'\n', body)) = data[space + 1..len].split_last() else {
            return Err(Rule::Header);
        };
        let eq = body.iter().position(|&b| b == b'=').ok_or(Rule::Header)?;
        let (key, value) = (&body[..eq], &body[eq + 1..]);
        match key {
            b"path" => out.path = Some(value.to_vec()),
            b"size" => out.size = Some(decimal(value).ok_or(Rule::Header)?),
            b"mtime" | b"uid" | b"gid" | b"uname" | b"gname" => {}
            key if is_xattr(key) => return Err(Rule::ExtendedAttribute),
            _ => return Err(Rule::PaxRecord),
        }
        data = &data[len..];
    }
    Ok(out)
}

fn is_xattr(key: &[u8]) -> bool {
    [
        "SCHILY.xattr.",
        "LIBARCHIVE.xattr.",
        "SCHILY.acl.",
        "security.",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix.as_bytes()))
}

/// The most bytes of a name, as ustar's prefix and name fields hold.
pub const MAX_NAME_BYTES: usize = 255;
/// The most components of a name.
pub const MAX_DEPTH: usize = 16;

/// Checks the name of an entry and returns its components. A directory's name may end with one
/// `/`, which is not part of the name.
pub(crate) fn name(raw: &[u8], dir: bool) -> Result<Vec<&str>, Rule> {
    let raw = match raw.strip_suffix(b"/") {
        Some(stripped) if dir => stripped,
        _ => raw,
    };
    if raw.len() > MAX_NAME_BYTES {
        return Err(Rule::NameTooLong);
    }
    if raw.contains(&0) | raw.starts_with(b"/") | raw.is_empty() {
        return Err(Rule::BadPath);
    }
    let text = std::str::from_utf8(raw).map_err(|_| Rule::NotUtf8)?;
    let parts: Vec<&str> = text.split('/').collect();
    if parts.iter().any(|p| matches!(*p, "" | "." | "..")) {
        return Err(Rule::BadPath);
    }
    if parts.len() > MAX_DEPTH {
        return Err(Rule::TooDeep);
    }
    Ok(parts)
}

/// The simple lower-case fold of `name`, char by char. U+0130 is the one character whose
/// lower case is two characters; its simple lower case is `i`.
pub(crate) fn fold(name: &str) -> String {
    name.chars()
        .map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(one), None) => one,
                _ => 'i',
            }
        })
        .collect()
}
