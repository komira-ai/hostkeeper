//! One HTTPS GET into a writer, through the daemon's [`Transport`]: a status other than 2xx is
//! refused before a byte is written, and the body goes through [`copy`], so it is measured as it
//! is written and stopped at its cap. The transport's own total timeout bounds the transfer.

use std::io::Write;

use hostkeeper_package::net::{Response, Timeouts, Transport};
use hostkeeper_package::ports::NetError;

use crate::copy::{CopyError, Measured, copy};

/// Why a download did not deliver its body.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DownloadError {
    /// No response: no connection, a refused handshake, the transport's timeout.
    #[error("{0}")]
    Net(NetError),
    /// A response whose status is not 2xx (after any redirect the transport follows).
    #[error("the server answered with status {0}")]
    Status(u16),
    /// The body: too large, a read that failed, or a write that failed.
    #[error("{0}")]
    Body(CopyError),
}

/// GETs `url` and copies its body into `to`, at most `cap` bytes.
pub(crate) fn download(
    transport: &dyn Transport,
    url: &str,
    timeouts: Timeouts,
    cap: u64,
    to: &mut dyn Write,
) -> Result<Measured, DownloadError> {
    let Response {
        status, mut body, ..
    } = transport.open(url, timeouts).map_err(DownloadError::Net)?;
    if !(200..300).contains(&status) {
        return Err(DownloadError::Status(status));
    }
    copy(&mut body, to, cap).map_err(DownloadError::Body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeTransport, Serve, sha256_hex, timeouts};

    /// A 200's body is written and measured; a 404 and a 302 are refused by their status with
    /// nothing written; no connection is the transport's error; a body over the cap is
    /// refused.
    ///
    /// Catches an error page saved as a boot file, and a redirect body taken for the asset.
    #[test]
    fn a_download_writes_only_a_2xx_body_within_its_cap() {
        let transport = FakeTransport::new()
            .route("https://r.example/ok", Serve::Body(b"body".to_vec()))
            .route("https://r.example/gone", Serve::Status(404))
            .route("https://r.example/moved", Serve::Status(302))
            .route("https://r.example/down", Serve::Fail(NetError::Connect));
        let mut out = Vec::new();
        let got = download(&transport, "https://r.example/ok", timeouts(), 10, &mut out)
            .expect("downloaded");
        assert_eq!(
            (got.bytes, got.sha256, out),
            (4, sha256_hex(b"body"), b"body".to_vec())
        );
        for (path, text) in [
            ("gone", "the server answered with status 404"),
            ("moved", "the server answered with status 302"),
            ("down", "could not connect, or the request was not answered"),
        ] {
            let mut out = Vec::new();
            let url = format!("https://r.example/{path}");
            let refused = download(&transport, &url, timeouts(), 10, &mut out).expect_err(path);
            assert_eq!(refused.to_string(), text);
            assert!(out.is_empty(), "{path}");
        }
        let mut out = Vec::new();
        let large = download(&transport, "https://r.example/ok", timeouts(), 3, &mut out);
        assert_eq!(
            large.expect_err("too large").to_string(),
            "more than 3 bytes"
        );
    }
}
