//! The caller template of [`DOC`]: the workflow a service of another repository copies to set
//! its desired version through the CI client with targets. [`problems`] reads the template out of
//! the page (the one fenced `yaml` block of the section [`HEADING`]) and holds it to every rule a
//! workflow of this repository keeps ([`lint`]), the caller rules of [`super::callers`] included:
//! under the caller entry [`super::callers::TEMPLATE`], and to the rules of `hello-desired.yml`
//! as the proof run [`super::callers::TEMPLATE_RUN`].
//!
//! The page is refused when the section is missing, holds no `yaml` block or more than one, or a
//! block is not closed; and when the template breaks a rule, each problem named after the page.

use super::lint::lint;

/// The page, from the root of the repository; also the name the template is linted under.
pub const DOC: &str = "docs/deploy/service-caller.md";
/// The heading of the section that holds the template.
pub const HEADING: &str = "## The template";
/// What opens the template's block.
const OPEN: &str = "```yaml\n";
/// What closes it: the fence on a line of its own.
const CLOSE: &str = "\n```\n";

/// The text of the section [`HEADING`] of `doc`, up to the next heading of its level.
fn section(doc: &str) -> Option<&str> {
    let start = doc.find(&format!("\n{HEADING}\n"))? + 1;
    let rest = &doc[start + HEADING.len()..];
    Some(&rest[..rest.find("\n## ").unwrap_or(rest.len())])
}

/// The template of `doc`, the text of the one `yaml` block of its section, with its final line
/// break; or the problem that there is no one such block.
pub fn template(doc: &str) -> Result<&str, String> {
    let Some(section) = section(doc) else {
        return Err(format!("{DOC} has no section `{HEADING}`"));
    };
    let mut blocks = section.split(OPEN).skip(1);
    let (Some(block), None) = (blocks.next(), blocks.next()) else {
        return Err(format!(
            "{DOC}: the section `{HEADING}` must hold exactly one ```yaml block"
        ));
    };
    match block.find(CLOSE) {
        Some(end) => Ok(&block[..=end]),
        None => Err(format!("{DOC}: the template's ```yaml block is not closed")),
    }
}

/// One line per rule the template of `doc`, the text of [`DOC`], breaks.
pub fn problems(doc: &str) -> Vec<String> {
    match template(doc) {
        Ok(text) => lint(DOC, text)
            .into_iter()
            .map(|p| format!("{DOC}, the template: {p}"))
            .collect(),
        Err(e) => vec![e],
    }
}
