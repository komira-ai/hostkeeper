//! The shape of an expanded `.pkg` (packages-repositories.md section 4.7, `stage` step 4): a
//! component package, or a product archive whose `Distribution` names one component, selected by
//! default, with no `script`, `installation-check` or `volume-check`; the component's
//! `PackageInfo` has the order's identifier and version, installs at `/`, and has no `relocate`
//! or `bundle`, so `installer` writes exactly what its `Bom` lists.

use std::fs;
use std::path::{Path, PathBuf};

use super::xml::{self, Element};

/// The elements of a `Distribution` that run or decide anything at install time.
pub const REFUSED_DISTRIBUTION: [&str; 3] = ["script", "installation-check", "volume-check"];
/// The attributes of a `choice` that select or enable it by a value other than `true`.
pub const CHOICE_SWITCHES: [&str; 4] = ["start_selected", "selected", "start_enabled", "enabled"];
/// The elements of a `PackageInfo` that make `installer` write elsewhere than the `Bom` says.
pub const REFUSED_PACKAGE_INFO: [&str; 2] = ["relocate", "bundle"];
/// The entries a product archive's expansion may hold beside its component.
pub const PRODUCT_EXTRAS: [&str; 2] = ["Distribution", "Resources"];

/// The component of an expanded package: its directory and the three entries `stage` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub dir: PathBuf,
}

impl Component {
    pub fn package_info(&self) -> PathBuf {
        self.dir.join("PackageInfo")
    }

    pub fn bom(&self) -> PathBuf {
        self.dir.join("Bom")
    }

    pub fn payload(&self) -> PathBuf {
        self.dir.join("Payload")
    }
}

/// What the shape check found wrong; each is `manifest_invalid`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShapeError {
    #[error("cannot read {0}: {1}")]
    Read(String, String),
    #[error("{0} is not XML that can be read: {1}")]
    Xml(String, xml::XmlError),
    #[error("the Distribution has a <{0}> element")]
    RefusedElement(String),
    #[error("the Distribution names {0} components, not one")]
    Components(usize),
    #[error("the Distribution names the component at {0} paths, not one")]
    Paths(usize),
    #[error("the Distribution's component path {0:?} is not #<name> of a directory beside it")]
    BadPath(String),
    #[error("a choice is not selected by default: {0}=\"{1}\"")]
    NotSelected(String, String),
    #[error("the product archive holds {0:?}, beside its one component")]
    Extra(String),
    #[error("the component has no {0}")]
    Missing(&'static str),
    #[error("the PackageInfo has no <pkg-info> element first")]
    NoPkgInfo,
    #[error("the PackageInfo's {0} is {1:?}, not {2:?}")]
    Mismatch(&'static str, String, String),
    #[error("the PackageInfo has a <{0}> element")]
    RefusedPackageInfo(String),
}

/// Checks the expansion at `expanded` against the order's `identifier` and `version`, and finds
/// its component.
pub fn check(expanded: &Path, identifier: &str, version: &str) -> Result<Component, ShapeError> {
    let distribution = expanded.join("Distribution");
    let component = match distribution.is_file() {
        true => product(expanded, &read_xml(&distribution, "the Distribution")?)?,
        false => Component {
            dir: expanded.to_owned(),
        },
    };
    for name in ["PackageInfo", "Bom"] {
        if !component.dir.join(name).is_file() {
            return Err(ShapeError::Missing(name));
        }
    }
    if !component.payload().is_dir() {
        return Err(ShapeError::Missing("Payload"));
    }
    package_info(
        &read_xml(&component.package_info(), "the PackageInfo")?,
        identifier,
        version,
    )?;
    Ok(component)
}

fn read_xml(path: &Path, what: &str) -> Result<Vec<Element>, ShapeError> {
    let text = fs::read_to_string(path).map_err(|e| ShapeError::Read(what.into(), e.to_string()))?;
    xml::elements(&text).map_err(|e| ShapeError::Xml(what.into(), e))
}

/// The one component a product archive's `Distribution` names.
fn product(expanded: &Path, elements: &[Element]) -> Result<Component, ShapeError> {
    if let Some(refused) = elements
        .iter()
        .find(|e| REFUSED_DISTRIBUTION.contains(&e.name.as_str()))
    {
        return Err(ShapeError::RefusedElement(refused.name.clone()));
    }
    for choice in elements.iter().filter(|e| e.name == "choice") {
        for switch in CHOICE_SWITCHES {
            match choice.attribute(switch) {
                Some(value) if value != "true" => {
                    return Err(ShapeError::NotSelected(switch.into(), value.into()));
                }
                _ => {}
            }
        }
    }
    let refs: Vec<&Element> = elements.iter().filter(|e| e.name == "pkg-ref").collect();
    let mut ids: Vec<&str> = refs.iter().filter_map(|e| e.attribute("id")).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != 1 {
        return Err(ShapeError::Components(ids.len()));
    }
    let mut paths: Vec<&str> = refs
        .iter()
        .map(|e| e.text.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let [path] = paths[..] else {
        return Err(ShapeError::Paths(paths.len()));
    };
    let name = path
        .strip_prefix('#')
        .and_then(percent_decoded)
        .filter(|name| is_plain_name(name))
        .ok_or_else(|| ShapeError::BadPath(path.into()))?;
    let dir = expanded.join(&name);
    if !dir.is_dir() {
        return Err(ShapeError::BadPath(path.into()));
    }
    let entries = fs::read_dir(expanded)
        .map_err(|e| ShapeError::Read("the expansion".into(), e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| ShapeError::Read("the expansion".into(), e.to_string()))?;
        let found = entry.file_name().to_string_lossy().into_owned();
        if found != name && !PRODUCT_EXTRAS.contains(&found.as_str()) {
            return Err(ShapeError::Extra(found));
        }
    }
    Ok(Component { dir })
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// `text` with its `%XX` escapes decoded, when they are all whole and the result is UTF-8.
fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn package_info(elements: &[Element], identifier: &str, version: &str) -> Result<(), ShapeError> {
    let info = elements
        .first()
        .filter(|e| e.name == "pkg-info")
        .ok_or(ShapeError::NoPkgInfo)?;
    for (key, want) in [
        ("identifier", identifier),
        ("version", version),
        ("install-location", "/"),
    ] {
        let found = info.attribute(key).unwrap_or_default();
        if found != want {
            return Err(ShapeError::Mismatch(key, found.into(), want.into()));
        }
    }
    match elements
        .iter()
        .find(|e| REFUSED_PACKAGE_INFO.contains(&e.name.as_str()))
    {
        Some(refused) => Err(ShapeError::RefusedPackageInfo(refused.name.clone())),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "shape_tests.rs"]
mod tests;
