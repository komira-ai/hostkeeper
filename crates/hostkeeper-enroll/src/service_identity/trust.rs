//! [`PkiTrust`]: the host's id and trusted CA bundle, read from the certificate and the `ca.pem`
//! of the pki directory's `current` generation (`CurrentPaths`).

use x509_cert::Certificate;
use x509_cert::der::DecodePem;

use super::{HostTrust, Trust};
use crate::store::CurrentPaths;
use crate::verify::common_name;
use crate::wire::HostId;

/// The files of the current generation, read at every call: a renewal that switched `current`
/// is followed.
#[derive(Clone, Debug)]
pub struct PkiTrust {
    paths: CurrentPaths,
}

impl PkiTrust {
    pub fn new(paths: CurrentPaths) -> PkiTrust {
        PkiTrust { paths }
    }
}

impl HostTrust for PkiTrust {
    fn read(&self) -> Result<Trust, String> {
        let cert = std::fs::read(&self.paths.cert)
            .map_err(|e| format!("cannot read {}: {e}", self.paths.cert.display()))?;
        let ca_bundle_pem = std::fs::read_to_string(&self.paths.ca)
            .map_err(|e| format!("cannot read {}: {e}", self.paths.ca.display()))?;
        let cert = Certificate::from_pem(&cert)
            .map_err(|_| format!("{} is not a PEM certificate", self.paths.cert.display()))?;
        let host_id = common_name(&cert)
            .as_deref()
            .and_then(|name| name.strip_prefix("host:"))
            .and_then(HostId::new)
            .ok_or_else(|| {
                format!(
                    "{} is not the certificate of a host",
                    self.paths.cert.display()
                )
            })?;
        Ok(Trust {
            host_id,
            ca_bundle_pem,
        })
    }
}
