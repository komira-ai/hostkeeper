//! The response, as `--output json` or `--output text` prints it on stdout, with every secret
//! taken out first; the names of the secrets taken out go to stderr.

use std::io::{self, Write};

use prost_reflect::{DynamicMessage, ExtensionDescriptor, Kind, ReflectMessage};

use crate::{json, secret};

/// `--output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One proto3 canonical JSON document.
    Json,
    /// `field: value` lines for scalars, indented canonical JSON for the rest.
    Text,
}

/// Writes `response` in `format`. The notice of each secret taken out is written to `stderr`
/// first; when that write fails, the error is returned and nothing is written to `stdout`.
pub fn write(
    response: &DynamicMessage,
    format: Format,
    ext: Option<&ExtensionDescriptor>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let (response, removed) = secret::redacted(response, ext);
    for name in &removed {
        writeln!(stderr, "redacted: {name}")?;
    }
    match format {
        Format::Json => writeln!(stdout, "{}", json::pretty(&response, 0)),
        Format::Text => text(&response, stdout),
    }
}

/// The text form: each field that is present, in the order the message declares them.
fn text(response: &DynamicMessage, out: &mut dyn Write) -> io::Result<()> {
    for field in json::declared_fields(&response.descriptor()) {
        if !response.has_field(&field) {
            continue;
        }
        let scalar =
            !field.is_list() && !field.is_map() && !matches!(field.kind(), Kind::Message(_));
        if scalar {
            let value = response.get_field(&field);
            writeln!(
                out,
                "{}: {}",
                field.name(),
                json::scalar_text(&value, &field.kind())
            )?;
        } else {
            writeln!(out, "{}:", field.name())?;
            writeln!(out, "  {}", json::pretty_field(response, &field, 1))?;
        }
    }
    Ok(())
}
