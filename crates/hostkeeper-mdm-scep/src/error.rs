/// Why no reply could be formed, or why a certificate could not be issued or the CA not built.
#[derive(Debug, PartialEq, Eq, Clone, thiserror::Error)]
pub enum Error {
    /// The body is not a SCEP message that names a transaction: the caller answers with an HTTP
    /// error, since a FAILURE reply needs the `transactionID` to be echoed.
    #[error("not a PKIOperation message: {0}")]
    Malformed(&'static str),
    /// The CA certificate or key is unusable.
    #[error("the CA {0} is unusable")]
    CaMaterial(&'static str),
    /// A certificate or a reply could not be built or encoded.
    #[error("building {0}")]
    Build(&'static str),
    /// A serial the subject cannot carry: empty, longer than 64 or not letters and digits.
    #[error("a serial of {len} bytes is not a Mac serial number")]
    BadSerial {
        /// Its length.
        len: usize,
    },
}
