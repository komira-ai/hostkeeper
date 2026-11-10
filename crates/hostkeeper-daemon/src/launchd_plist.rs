//! The executable a launchd plist starts, read as text (the first `<key>Program</key>` or
//! `ProgramArguments`), which the observer hashes for a `.pkg` service's `run` entry
//! ([`crate::observer`]).

/// `text` without its XML comments (`<!-- ... -->`); an unclosed comment runs to the end.
fn without_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("<!--") {
        out.push_str(before);
        rest = after.split_once("-->").map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out
}

/// `text` with its XML character and entity references decoded (`&#47;`, `&#x2f;`, `&amp;`,
/// `&lt;`, `&gt;`, `&quot;`, `&apos;`); an unknown reference is kept as it is.
fn decode_xml(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('&') {
        out.push_str(before);
        let decoded = after.split_once(';').and_then(|(name, tail)| {
            let number = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => name.strip_prefix('#').and_then(|dec| dec.parse().ok()),
            };
            let named = [
                ("amp", '&'),
                ("lt", '<'),
                ("gt", '>'),
                ("quot", '"'),
                ("apos", '\''),
            ]
            .into_iter()
            .find_map(|(entity, c)| (entity == name).then_some(c));
            number.and_then(char::from_u32).or(named).map(|c| (c, tail))
        });
        match decoded {
            Some((c, tail)) => {
                out.push(c);
                rest = tail;
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `<string>` that begins `text` (after white space), decoded; `None` when `text` does not
/// begin with one or it is not closed.
fn leading_string(text: &str) -> Option<String> {
    text.trim_start()
        .strip_prefix("<string>")?
        .split_once("</string>")
        .map(|(value, _)| decode_xml(value.trim()))
}

/// The first `<string>` of the `ProgramArguments` array of the plist `text`, whose comments are
/// already removed.
fn first_argument(text: &str) -> Option<String> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    leading_string(after.trim_start().strip_prefix("<array>")?)
}

/// The executable a launchd plist starts, as launchd chooses it: the `<string>` of its `Program`
/// key when it has that key, else the first `<string>` of its `ProgramArguments` array; decoded,
/// with its XML comments skipped. `None` when it has neither, or its `Program` key is not
/// followed by a string (launchd does not load such a job, so nothing is guessed in its place).
/// The keys are found as text, as the lint finds them: the first `<key>Program</key>` anywhere
/// counts, a nested dictionary's included.
pub fn plist_program(text: &str) -> Option<String> {
    let text = without_comments(text);
    match text.split_once("<key>Program</key>") {
        Some((_, after)) => leading_string(after),
        None => first_argument(&text),
    }
}

#[cfg(test)]
#[path = "launchd_plist_tests.rs"]
mod tests;
