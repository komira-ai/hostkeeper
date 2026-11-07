//! The times the flags of `serve` take: digits and one unit, `s`, `m`, `h` or `d`, as seconds.

/// `text` in seconds: digits, then one unit of `s`, `m`, `h` or `d`, and at least one second.
/// `lasts` finishes the sentence that says why none is refused (`a request waits`).
fn secs(text: &str, lasts: &str) -> Result<u64, String> {
    let wrong = || format!("{text:?} is not a time like 90m or 72h: digits, then s, m, h or d");
    let (digits, per) = [("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)]
        .into_iter()
        .find_map(|(unit, per)| text.strip_suffix(unit).map(|digits| (digits, per)))
        .ok_or_else(wrong)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(wrong());
    }
    let secs = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(per))
        .ok_or_else(|| format!("{text:?} is more seconds than this coordinator counts"))?;
    if secs == 0 {
        return Err(format!("{text:?} is no time: {lasts} at least one second"));
    }
    Ok(secs)
}

/// `--enrollment-request-ttl` in seconds.
pub(super) fn request_ttl_secs(text: &str) -> Result<u64, String> {
    secs(text, "a request waits")
}

/// `--record-approve-for` and `--record-approve-max` in seconds.
pub(super) fn record_window_secs(text: &str) -> Result<u64, String> {
    secs(text, "a record's approval window lasts")
}
