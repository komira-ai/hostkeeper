//! The slots of a plan's text: `{name}` in a call's arguments, its request, an operator's
//! instruction and a watch's service and version, and `{measure.stat}` in what the report states.
//!
//! A text is split into literal pieces and slots; a `{` opens a slot and the next `}` closes it.
//! A slot is 1 to 64 bytes of `[a-z0-9.:-]`, so a brace of JSON or prose is refused rather than
//! read as a slot. What a slot means is the caller's: [`Slot`] reads the slots of a step.

use thiserror::Error;

/// One piece of a text: literal text, or the name between a pair of braces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Slot(String),
}

/// Why a text's slots cannot be read.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemplateError {
    #[error("{text:?}: a `{{` with no `}}` after it")]
    Unclosed { text: String },
    #[error("{text:?}: a `}}` with no `{{` before it")]
    Unopened { text: String },
    #[error("{text:?}: the slot {{{slot}}} is not 1 to 64 bytes of [a-z0-9.:-]")]
    BadSlot { text: String, slot: String },
}

/// Splits `text` into its pieces.
pub fn pieces(text: &str) -> Result<Vec<Piece>, TemplateError> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let open = rest.find('{');
        let close = rest.find('}');
        match (open, close) {
            (None, None) => {
                if !rest.is_empty() {
                    out.push(Piece::Text(rest.to_owned()));
                }
                return Ok(out);
            }
            (Some(o), Some(c)) if o < c => {
                if o > 0 {
                    out.push(Piece::Text(rest[..o].to_owned()));
                }
                let slot = &rest[o + 1..c];
                if !slot_ok(slot) {
                    return Err(TemplateError::BadSlot {
                        text: text.to_owned(),
                        slot: slot.to_owned(),
                    });
                }
                out.push(Piece::Slot(slot.to_owned()));
                rest = &rest[c + 1..];
            }
            (Some(_), None) => {
                return Err(TemplateError::Unclosed {
                    text: text.to_owned(),
                });
            }
            (_, Some(_)) => {
                return Err(TemplateError::Unopened {
                    text: text.to_owned(),
                });
            }
        }
    }
}

fn slot_ok(slot: &str) -> bool {
    !slot.is_empty()
        && slot.len() <= 64
        && slot
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.:".contains(&b))
}

/// Every slot name of `text`, in order.
pub fn slots(text: &str) -> Result<Vec<String>, TemplateError> {
    Ok(pieces(text)?
        .into_iter()
        .filter_map(|p| match p {
            Piece::Slot(s) => Some(s),
            Piece::Text(_) => None,
        })
        .collect())
}

/// `text` with each slot replaced by what `fill` gives for it.
pub fn render<E>(text: &str, fill: &mut dyn FnMut(&str) -> Result<String, E>) -> Result<String, E>
where
    E: From<TemplateError>,
{
    let mut out = String::new();
    for piece in pieces(text)? {
        match piece {
            Piece::Text(t) => out.push_str(&t),
            Piece::Slot(s) => out.push_str(&fill(&s)?),
        }
    }
    Ok(out)
}

/// What a slot of a step names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// `{host}`: the node id of the repetition's host.
    Host,
    /// `{rep}`: the number of the repetition, from 1.
    Rep,
    /// `{name}`: the value of the plan's parameter `name`.
    Param(String),
    /// `{file:name}`: the text of the file the parameter `name` names.
    File(String),
    /// `{sha256:name}`: the SHA-256, in lowercase hex, of that file.
    Sha256(String),
}

impl Slot {
    /// Reads a slot name of a step; `None` for a name that is not a step's slot.
    pub fn of(name: &str) -> Option<Self> {
        if let Some(param) = name.strip_prefix("file:") {
            return param_name_ok(param).then(|| Self::File(param.to_owned()));
        }
        if let Some(param) = name.strip_prefix("sha256:") {
            return param_name_ok(param).then(|| Self::Sha256(param.to_owned()));
        }
        match name {
            "host" => Some(Self::Host),
            "rep" => Some(Self::Rep),
            other => param_name_ok(other).then(|| Self::Param(other.to_owned())),
        }
    }

    /// The parameter the slot reads, if any.
    pub fn param(&self) -> Option<&str> {
        match self {
            Self::Param(p) | Self::File(p) | Self::Sha256(p) => Some(p),
            Self::Host | Self::Rep => None,
        }
    }
}

/// A parameter's name: 1 to 32 bytes of `[a-z0-9-]`, starting with a letter.
pub fn param_name_ok(name: &str) -> bool {
    id_ok(name, 32)
}

/// An id of a plan, a step, a measure or a run: 1 to `max` bytes of `[a-z0-9-]`, starting with a
/// letter.
pub fn id_ok(id: &str, max: usize) -> bool {
    !id.is_empty()
        && id.len() <= max
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
