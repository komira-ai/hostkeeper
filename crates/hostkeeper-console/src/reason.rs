//! The reason of a console audit line (`docs/design/console-broker.md`, decision D4): the
//! operator's reason first, then the details of the session (its id, kind, the attaching peer,
//! its duration and bytes), each as `key=value` after `; `, the whole cut at the audit's 256
//! bytes on a character boundary.

/// The most bytes the audit keeps of a reason.
pub const MAX_REASON_BYTES: usize = 256;

/// The reason of a console audit line: `reason`, then each of `details` as `; key=value`, cut at
/// [`MAX_REASON_BYTES`] on a character boundary.
#[must_use]
pub fn audit_reason(reason: &str, details: &[(&str, &str)]) -> String {
    let mut text = reason.to_owned();
    for (key, value) in details {
        if !text.is_empty() {
            text.push_str("; ");
        }
        text.push_str(key);
        text.push('=');
        text.push_str(value);
    }
    let mut end = text.len().min(MAX_REASON_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

#[cfg(test)]
#[path = "reason_tests.rs"]
mod tests;
