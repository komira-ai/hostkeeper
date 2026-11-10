//! The params of a step: the package's `[params]` (`release`, `settings`, `boot_dir`, `unit`)
//! and the updater's `updater.target_version` and `updater.previous_version`, each checked.

use std::path::PathBuf;

use hostkeeper_hook::{Params, Scalar, Step};

use super::Failure;
use crate::cli::release_url;

/// The most bytes of a version used as a directory name, and of a unit name.
const MAX_NAME: usize = 255;

/// The params of a step, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HookParams {
    /// The `https://` URL of the release's assets.
    pub(crate) release: String,
    /// The settings file.
    pub(crate) settings: PathBuf,
    /// The directory of the boot sets.
    pub(crate) boot_dir: PathBuf,
    /// The unit that runs `serve`.
    pub(crate) unit: String,
    /// The version being installed: `updater.target_version` for the steps that name a boot set,
    /// empty for `verify` and `uninstall`, which use none.
    pub(crate) target: String,
    /// The version it replaces, when there is one.
    pub(crate) previous: Option<String>,
}

impl HookParams {
    /// The params of `step` in `params`; `updater.target_version` is required by the steps that
    /// name a boot set.
    ///
    /// # Errors
    /// Class `environment`, reason `bad_params`, naming the param.
    pub(crate) fn of(params: &Params, step: Step) -> Result<HookParams, Failure> {
        let bad = |text: String| Failure::environment("bad_params", text);
        let text = |key: &str| match params.get(key) {
            Some(Scalar::Text(text)) => Ok(text.as_str()),
            Some(_) => Err(bad(format!("param {key} must be text"))),
            None => Err(bad(format!("param {key} is missing"))),
        };
        let path = |key: &str| {
            let value = text(key)?;
            match value.starts_with('/') {
                true => Ok(PathBuf::from(value)),
                false => Err(bad(format!("param {key} must be an absolute path"))),
            }
        };
        let version = |key: &str, value: &str| match is_component(value) {
            true => Ok(value.to_owned()),
            false => Err(bad(format!(
                "{key} must be one path component of letters, digits and . _ + -"
            ))),
        };
        let unit = text("unit")?;
        if !is_unit(unit) {
            return Err(bad(
                "param unit must be a plain unit name ending .service".to_owned()
            ));
        }
        let needs_target = !matches!(step, Step::Verify | Step::Uninstall);
        let target = match (params.target_version(), needs_target) {
            (Some(value), _) => version("updater.target_version", value)?,
            (None, true) => return Err(bad("updater.target_version is missing".to_owned())),
            (None, false) => String::new(),
        };
        Ok(HookParams {
            release: release_url(text("release")?)
                .map_err(|e| bad(format!("param release: {e}")))?,
            settings: path("settings")?,
            boot_dir: path("boot_dir")?,
            unit: unit.to_owned(),
            target,
            previous: params
                .previous_version()
                .map(|value| version("updater.previous_version", value))
                .transpose()?,
        })
    }
}

/// Whether `text` is one path component of letters, digits and `. _ + -`, 1 to 255 bytes, and
/// neither `.` nor `..`.
fn is_component(text: &str) -> bool {
    (1..=MAX_NAME).contains(&text.len())
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// Whether `text` is a plain unit name: a component that starts with a letter or digit and ends
/// in `.service`.
fn is_unit(text: &str) -> bool {
    text.ends_with(".service")
        && text.starts_with(|c: char| c.is_ascii_alphanumeric())
        && is_component(text)
}
