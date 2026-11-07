//! The port through which the Mac path asks the MDM policy gate about a device (design 5.9).
//!
//! The gate is the crate `hostkeeper-mdm-gate` (docs/design/hostkeeper-mdm.md), linked into the
//! coordinator; [`crate::mdm`] answers this port from its device records. A coordinator started
//! without the gate (no `--mdm-api-url`) uses [`NoDeviceState`], which knows no device, so no
//! Mac is approved by its nonce.

/// What the gate says of a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceStatus {
    /// The device is enrolled in the MDM.
    pub enrolled: bool,
    /// The device is supervised.
    pub supervised: bool,
    /// When the device's current MDM enrollment began, unix seconds by the coordinator's clock:
    /// its first `TokenUpdate`, under a new enrollment id or after a `CheckOut`. `None` when the
    /// gate does not know. A nonce approval takes the host id of the same Mac's key approved
    /// before this time over (docs/design/hostkeeper-mdm.md, section 5.1); with `None` it
    /// takes none over.
    pub enrolled_since_unix: Option<u64>,
}

/// The gate could not say: it does not know the device, or could not be asked. Nothing is
/// approved on it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no device state: {0}")]
pub struct DeviceStateError(pub String);

/// The gate's answer about the device of a Mac serial number.
pub trait DeviceState: Send + Sync {
    /// The status of the device whose serial number is `serial`.
    ///
    /// # Errors
    /// [`DeviceStateError`] when the gate does not know the device or cannot be asked.
    fn status(&self, serial: &str) -> Result<DeviceStatus, DeviceStateError>;
}

/// The port of a coordinator with no gate: every device is unknown.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDeviceState;

impl DeviceState for NoDeviceState {
    fn status(&self, _serial: &str) -> Result<DeviceStatus, DeviceStateError> {
        Err(DeviceStateError(
            "no device-state gate is configured".to_owned(),
        ))
    }
}

/// A gate the tests set: the status of each serial, or an error for it; unknown otherwise.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeDeviceState(
    std::sync::Mutex<std::collections::BTreeMap<String, Result<DeviceStatus, DeviceStateError>>>,
);

#[cfg(test)]
impl FakeDeviceState {
    /// From now on, `serial` is answered `answer`.
    pub fn set(&self, serial: &str, answer: Result<DeviceStatus, DeviceStateError>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(serial.to_owned(), answer);
    }
}

#[cfg(test)]
impl DeviceState for FakeDeviceState {
    fn status(&self, serial: &str) -> Result<DeviceStatus, DeviceStateError> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(serial)
            .cloned()
            .unwrap_or_else(|| Err(DeviceStateError("unknown device".to_owned())))
    }
}
