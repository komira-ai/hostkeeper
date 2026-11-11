//! The ABM side of the Mac path, answered from the coordinator's own parts (design 4.6): the
//! table the sync keeps ([`crate::abm::Abm::serial_in_abm`]) and the settings of the setup
//! (`host_id_prefix`, `default_group`).

use std::sync::Arc;

use crate::abm::Abm;
use crate::enrollment::{AbmDevices, AbmNaming, AbmUnavailable};
use crate::mdm_settings;
use crate::mdm_setup::MdmSetup;
use crate::service::Coordinator;

/// What [`AbmDevices`] reads.
struct FromAbm {
    abm: Arc<Abm>,
    setup: Arc<MdmSetup>,
}

impl AbmDevices for FromAbm {
    fn serial_in_abm(&self, serial: &str) -> Result<bool, AbmUnavailable> {
        self.abm
            .serial_in_abm(serial)
            .map_err(|why| AbmUnavailable(why.to_string()))
    }

    fn naming(&self) -> Result<AbmNaming, AbmUnavailable> {
        let set = self
            .setup
            .settings()
            .map_err(|why| AbmUnavailable(why.to_string()))?;
        let text = |name| {
            mdm_settings::text_of(&set, name)
                .unwrap_or_default()
                .to_owned()
        };
        Ok(AbmNaming {
            host_id_prefix: text("host_id_prefix"),
            default_group: Some(text("default_group")).filter(|group| !group.is_empty()),
        })
    }
}

impl Coordinator {
    /// Hands the enrollment, if the coordinator has one and an ABM client, the ABM side of the Mac
    /// path. Called whenever one of the three is set, in whatever order.
    pub(crate) fn link_abm_approval(&self) {
        if let (Some(enrollment), Some(abm)) = (self.enrollment(), &self.abm) {
            enrollment.abm_by(Arc::new(FromAbm {
                abm: Arc::clone(abm),
                setup: Arc::clone(&self.mdm_setup),
            }));
        }
    }
}

#[cfg(test)]
#[path = "abm_approval_tests.rs"]
mod tests;
