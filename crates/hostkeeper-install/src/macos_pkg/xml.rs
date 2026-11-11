//! A reader of the little XML a `.pkg` holds (`Distribution`, `PackageInfo`): its elements in
//! document order, each with its name, its attributes and the text before the next tag. It reads
//! no DTD and resolves only the five predefined entities and character references; anything else
//! it cannot read is an error, so a document is never read as holding less than it does.

use std::collections::BTreeMap;

/// One element: its name, its attributes, and the text that follows its start tag up to the next
/// tag, entities resolved and trimmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attributes: BTreeMap<String, String>,
    pub text: String,
}

impl Element {
    /// The attribute `name`'s value.
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }
}

/// Why a document was not read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum XmlError {
    #[error("a tag is not closed")]
    Unclosed,
    #[error("a tag has no name")]
    NoName,
    #[error("an attribute of <{0}> is not name=\"value\"")]
    BadAttribute(String),
    #[error("<{0}> names the attribute {1} twice")]
    DoubleAttribute(String, String),
    #[error("an entity {0:?} that is not one of XML's own")]
    Entity(String),
    #[error("a document type declaration, which is not read")]
    Doctype,
}

/// The elements of `text`, in document order. Comments, processing instructions and end tags are
/// skipped; CDATA is read as text.
pub fn elements(text: &str) -> Result<Vec<Element>, XmlError> {
    let mut found: Vec<Element> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let tail = &rest[start..];
        if let Some(after) = tail.strip_prefix("<!--") {
            rest = skip_past(after, "-->")?;
            continue;
        }
        if let Some(after) = tail.strip_prefix("<![CDATA[") {
            let end = after.find("]]>").ok_or(XmlError::Unclosed)?;
            if let Some(last) = found.last_mut() {
                last.text.push_str(after[..end].trim());
            }
            rest = &after[end + 3..];
            continue;
        }
        if tail.starts_with("<!") {
            return Err(XmlError::Doctype);
        }
        if let Some(after) = tail.strip_prefix("<?") {
            rest = skip_past(after, "?>")?;
            continue;
        }
        let end = tag_end(tail).ok_or(XmlError::Unclosed)?;
        let tag = &tail[1..end];
        rest = &tail[end + 1..];
        if tag.starts_with('/') {
            continue;
        }
        let mut element = start_tag(tag.strip_suffix('/').unwrap_or(tag))?;
        let next = rest.find('<').unwrap_or(rest.len());
        element.text = unescape(rest[..next].trim())?;
        found.push(element);
    }
    Ok(found)
}

/// Where the tag that `tail` starts with ends: its first `>` outside a quoted value.
fn tag_end(tail: &str) -> Option<usize> {
    let mut quote = None;
    for (at, c) in tail.char_indices() {
        match (quote, c) {
            (None, '>') => return Some(at),
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if open == c => quote = None,
            _ => {}
        }
    }
    None
}

fn skip_past<'a>(text: &'a str, end: &str) -> Result<&'a str, XmlError> {
    let at = text.find(end).ok_or(XmlError::Unclosed)?;
    Ok(&text[at + end.len()..])
}

/// The element of a start tag's inside: `name a="1" b='2'`.
fn start_tag(tag: &str) -> Result<Element, XmlError> {
    let tag = tag.trim();
    let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let name = &tag[..name_end];
    if name.is_empty() {
        return Err(XmlError::NoName);
    }
    let bad = || XmlError::BadAttribute(name.to_owned());
    let mut attributes = BTreeMap::new();
    let mut rest = tag[name_end..].trim_start();
    while !rest.is_empty() {
        let eq = rest.find('=').ok_or_else(bad)?;
        let key = rest[..eq].trim();
        let value = rest[eq + 1..].trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
        let quote = quote.ok_or_else(bad)?;
        let close = value[1..].find(quote).ok_or_else(bad)?;
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(bad());
        }
        let text = unescape(&value[1..1 + close])?;
        if attributes.insert(key.to_owned(), text).is_some() {
            return Err(XmlError::DoubleAttribute(name.to_owned(), key.to_owned()));
        }
        rest = value[close + 2..].trim_start();
    }
    Ok(Element {
        name: name.to_owned(),
        attributes,
        text: String::new(),
    })
}

/// `text` with its entities and character references resolved.
fn unescape(text: &str) -> Result<String, XmlError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after
            .find(';')
            .ok_or_else(|| XmlError::Entity(after.to_owned()))?;
        let name = &after[..end];
        let resolved = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => character(name),
        };
        out.push(resolved.ok_or_else(|| XmlError::Entity(name.to_owned()))?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A character reference's character: `#38` or `#x26`.
fn character(name: &str) -> Option<char> {
    let number = name.strip_prefix('#')?;
    let value = match number.strip_prefix('x') {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(value)
}

#[cfg(test)]
#[path = "xml_tests.rs"]
mod tests;
