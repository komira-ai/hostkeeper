//! One stream copied into a writer, its bytes counted and hashed in the same pass, and stopped
//! as soon as it passes its cap. Every file `fetch` writes goes through [`copy`]: a download, a
//! file taken out of the ISO, and a file taken out of a deb.

use std::io::{self, Read, Write};

use sha2::{Digest as _, Sha256};

/// What was written: the number of bytes and their SHA-256, as 64 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Measured {
    pub bytes: u64,
    pub sha256: String,
}

/// Why a copy did not end.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CopyError {
    /// The stream held more than the cap. Nothing past the cap's next byte was read.
    #[error("more than {0} bytes")]
    TooLarge(u64),
    /// Reading the stream failed.
    #[error("{0}")]
    Read(io::Error),
    /// Writing failed (a full disk, for one).
    #[error("{0}")]
    Write(io::Error),
}

/// The writer of [`copy`]: each chunk goes to `to` whole, and is hashed and counted only then.
/// The first write error is kept, so [`copy`] can tell it from a read error.
struct Tee<'a> {
    to: &'a mut dyn Write,
    hash: Sha256,
    bytes: u64,
    failed: Option<io::Error>,
}

impl Write for Tee<'_> {
    fn write(&mut self, chunk: &[u8]) -> io::Result<usize> {
        match self.to.write_all(chunk) {
            Ok(()) => {
                self.hash.update(chunk);
                self.bytes = self
                    .bytes
                    .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                Ok(chunk.len())
            }
            Err(error) => {
                let kind = error.kind();
                self.failed = Some(error);
                Err(io::Error::from(kind))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.to.flush()
    }
}

/// Copies `from` into `to`, at most `cap` bytes: [`CopyError::TooLarge`] once one byte more has
/// been read, and the write of that byte has already happened (the caller removes what it
/// wrote).
pub(crate) fn copy(
    from: &mut dyn Read,
    to: &mut dyn Write,
    cap: u64,
) -> Result<Measured, CopyError> {
    let mut tee = Tee {
        to,
        hash: Sha256::new(),
        bytes: 0,
        failed: None,
    };
    let mut bounded = from.take(cap.saturating_add(1));
    if let Err(error) = io::copy(&mut bounded, &mut tee) {
        return Err(tee
            .failed
            .take()
            .map_or(CopyError::Read(error), CopyError::Write));
    }
    tee.flush().map_err(CopyError::Write)?;
    if tee.bytes > cap {
        return Err(CopyError::TooLarge(cap));
    }
    Ok(Measured {
        bytes: tee.bytes,
        sha256: hex::encode(tee.hash.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{BrokenReader, Failing, sha256_hex};

    /// A stream within its cap is copied whole, and its size and SHA-256 are those of its
    /// bytes; one exactly at the cap is too; an empty one is 0 bytes.
    ///
    /// Catches a copy that drops or repeats a chunk, a hash taken of something else than what
    /// was written, and an off-by-one at the cap.
    #[test]
    fn a_stream_within_its_cap_is_copied_and_measured() {
        let body = vec![7_u8; 200_000];
        for cap in [200_000, 1_000_000] {
            let mut out = Vec::new();
            let measured = copy(&mut &body[..], &mut out, cap).expect("copied");
            assert_eq!(out, body);
            assert_eq!(
                measured,
                Measured {
                    bytes: 200_000,
                    sha256: sha256_hex(&body)
                }
            );
        }
        let mut out = Vec::new();
        let empty = copy(&mut &b""[..], &mut out, 0).expect("copied");
        assert_eq!(empty.bytes, 0);
        assert_eq!(empty.sha256, sha256_hex(b""));
    }

    /// A stream one byte over its cap is refused, and no more than that one byte is read.
    ///
    /// Catches a cap that is not enforced, and one that reads the whole stream first.
    #[test]
    fn a_stream_over_its_cap_is_refused_after_one_byte_more() {
        let body = [1_u8; 100];
        let mut out = Vec::new();
        let refused = copy(&mut &body[..], &mut out, 10).expect_err("over the cap");
        assert_eq!(refused.to_string(), "more than 10 bytes");
        assert_eq!(out.len(), 11);
    }

    /// A read that fails is a read error and a write that fails is a write error, each with the
    /// error it came with; a flush that fails is a write error too.
    ///
    /// Catches a full disk reported as a network fault, and the reverse.
    #[test]
    fn read_and_write_failures_are_told_apart() {
        /// The variant of a refusal and its error's text.
        fn told(result: Result<Measured, CopyError>) -> (&'static str, String) {
            let error = result.expect_err("refused");
            let kind = match &error {
                CopyError::TooLarge(_) => "too large",
                CopyError::Read(_) => "read",
                CopyError::Write(_) => "write",
            };
            (kind, error.to_string())
        }
        let mut out = Vec::new();
        let read = copy(
            &mut BrokenReader::after(b"abc", "the link dropped"),
            &mut out,
            100,
        );
        assert_eq!(told(read), ("read", "the link dropped".to_owned()));
        let full = io::Error::from(io::ErrorKind::StorageFull).to_string();
        let write = copy(
            &mut &b"abc"[..],
            &mut Failing::writes(io::ErrorKind::StorageFull),
            100,
        );
        assert_eq!(told(write), ("write", full));
        let flush = copy(
            &mut &b"abc"[..],
            &mut Failing::flushes(io::ErrorKind::Other),
            100,
        );
        assert_eq!(told(flush).0, "write");
        assert_eq!(told(copy(&mut &b"ab"[..], &mut out, 1)).0, "too large");
    }
}
